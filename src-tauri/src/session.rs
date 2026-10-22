//! Recording session orchestration: start → capture → transcribe → clean → inject → history.

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    audio::Recording,
    macos, postprocess,
    settings::WidgetVisibility,
    store::make_record,
    transcribe::catalog_entry,
    vocab, widget, Core,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecState {
    Idle,
    Listening,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecMode {
    Ptt,
    Toggle,
    Latched,
    Click,
}

struct Active {
    id: u64,
    recording: Recording,
    mode: RecMode,
    focus_pid: Option<i32>,
    was_muted: Option<bool>,
}

#[derive(Default)]
pub struct Sessions {
    active: Mutex<Option<Active>>,
    next_id: AtomicU64,
    processing: AtomicU64,
    pub state: Mutex<(Option<RecState>, Option<RecMode>)>,
    starting: AtomicBool,
}

impl Sessions {
    pub fn is_active(&self) -> bool {
        self.active.lock().is_some() || self.starting.load(Ordering::Relaxed)
    }

    pub fn current(&self) -> (RecState, Option<RecMode>) {
        let s = self.state.lock();
        (s.0.unwrap_or(RecState::Idle), s.1)
    }
}

fn core(app: &AppHandle) -> tauri::State<'_, Core> {
    app.state::<Core>()
}

fn publish(app: &AppHandle, state: RecState, mode: Option<RecMode>) {
    *core(app).sessions.state.lock() = (Some(state), mode);
    let _ = app.emit("recording-state", json!({ "state": state, "mode": mode }));
    widget::on_recording_state(app, state);
}

pub fn notice(app: &AppHandle, kind: &str, title: &str, message: &str) {
    let _ = app.emit("notice", json!({ "kind": kind, "title": title, "message": message }));
}

pub fn start(app: &AppHandle, mode: RecMode) {
    let c = core(app);
    if c.sessions.active.lock().is_some() || c.sessions.starting.swap(true, Ordering::SeqCst) {
        return;
    }
    let settings = c.settings.read().clone();
    // Widget/tray clicks may have activated Wordfall; paste back into the app the user was in.
    let focus_pid = if mode == RecMode::Click { macos::last_foreign_pid() } else { macos::frontmost_foreign_pid() };
    // Kick off model loading early if needed; the recording can proceed meanwhile.
    crate::ensure_model_loaded(app);

    if settings.sounds_enabled {
        macos::play_cue(true);
    }
    let recording = match Recording::start(settings.input_device.as_deref(), settings.input_gain) {
        Ok(r) => r,
        Err(e) => {
            c.sessions.starting.store(false, Ordering::SeqCst);
            log::error!("could not start recording: {e}");
            notice(app, "error", "Microphone unavailable", &format!("{e}. Check microphone permission in System Settings."));
            return;
        }
    };
    log::info!("recording ({mode:?}) from {}", recording.device_name);
    let was_muted = settings.mute_system_audio.then(macos::mute_output);
    let id = c.sessions.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    *c.sessions.active.lock() = Some(Active { id, recording, mode, focus_pid, was_muted });
    c.sessions.starting.store(false, Ordering::SeqCst);
    publish(app, RecState::Listening, Some(mode));
    spawn_level_meter(app.clone(), id);
}

/// Switch an in-flight hold into hands-free mode (UI only; the hotkey machine owns timing).
pub fn latch(app: &AppHandle) {
    let c = core(app);
    let mut guard = c.sessions.active.lock();
    if let Some(a) = guard.as_mut() {
        a.mode = RecMode::Latched;
        drop(guard);
        publish(app, RecState::Listening, Some(RecMode::Latched));
    }
}

fn spawn_level_meter(app: AppHandle, id: u64) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(33));
        let levels = {
            let c = core(&app);
            let guard = c.sessions.active.lock();
            match guard.as_ref() {
                Some(a) if a.id == id => a.recording.levels(),
                _ => return,
            }
        };
        let _ = app.emit("audio-levels", json!({ "levels": levels }));
    });
}

pub fn cancel(app: &AppHandle) {
    let c = core(app);
    let Some(active) = c.sessions.active.lock().take() else { return };
    let was_muted = active.was_muted;
    drop(active.recording.finish());
    if let Some(m) = was_muted {
        macos::unmute_output(m);
    }
    let busy = c.sessions.processing.load(Ordering::SeqCst) > 0;
    publish(app, if busy { RecState::Processing } else { RecState::Idle }, None);
}

pub fn toggle(app: &AppHandle, mode: RecMode) {
    if core(app).sessions.active.lock().is_some() {
        stop(app);
    } else {
        start(app, mode);
    }
}

pub fn stop(app: &AppHandle) {
    let c = core(app);
    let Some(active) = c.sessions.active.lock().take() else { return };
    c.sessions.processing.fetch_add(1, Ordering::SeqCst);
    publish(app, RecState::Processing, Some(active.mode));
    let app = app.clone();
    thread::spawn(move || {
        let result = process(&app, active);
        let c = core(&app);
        let remaining = c.sessions.processing.fetch_sub(1, Ordering::SeqCst) - 1;
        if let Err(e) = result {
            log::error!("dictation failed: {e:#}");
            notice(&app, "error", "Dictation failed", &format!("{e:#}"));
        }
        if c.sessions.active.lock().is_none() {
            publish(&app, if remaining > 0 { RecState::Processing } else { RecState::Idle }, None);
        }
    });
}

fn process(app: &AppHandle, active: Active) -> anyhow::Result<()> {
    let c = core(app);
    let settings = c.settings.read().clone();
    let released = std::time::Instant::now();
    let captured = active.recording.finish();
    let drain_ms = released.elapsed().as_millis() as u64;
    if let Some(m) = active.was_muted {
        macos::unmute_output(m);
    }
    if settings.sounds_enabled {
        macos::play_cue(false);
    }
    if captured.duration < Duration::from_millis(300) || captured.samples_16k.len() < 4_000 {
        return Ok(());
    }
    if captured.rms < 0.0008 {
        notice(app, "warning", "No audio", "Your microphone picked up silence. Check the input device or mic boost.");
        return Ok(());
    }

    let model_id = wait_for_model(app)?;
    let (dictionary, prompt) = {
        let store = c.store.lock();
        let dict_terms: Vec<String> = store.dictionary.iter().map(|e| e.replacement.clone()).collect();
        let terms = vocab::terms(
            settings.prompt_mode,
            &vocab::Sources { dictionary: &dict_terms, user_terms: &store.vocabulary, projects: &c.projects },
        );
        (settings.dictionary_enabled.then(|| store.dictionary.clone()), vocab::prompt(&terms))
    };
    let model_wait_ms = released.elapsed().as_millis() as u64 - drain_ms;
    let transcript =
        c.engine.transcribe(&captured.samples_16k, settings.language.as_deref(), prompt.as_deref(), &settings.decode)?;
    let post_started = std::time::Instant::now();
    let quiet = captured.rms < 0.01;
    let cleaned = postprocess::clean(&transcript.text, settings.remove_fillers, dictionary.as_deref(), quiet);
    let post_ms = post_started.elapsed().as_millis() as u64;
    if cleaned.text.is_empty() {
        log::info!("no speech in {:.1}s clip (rms {:.4}, raw {:?})", captured.duration.as_secs_f32(), captured.rms, transcript.text);
        notice(app, "info", "No speech detected", "Hold the key a little longer and speak clearly.");
        return Ok(());
    }

    let paste_started = std::time::Instant::now();
    let mut output = cleaned.text.clone();
    if !output.ends_with(char::is_whitespace) {
        output.push(' ');
    }
    if settings.copy_only {
        macos::copy_text(output.trim_end())?;
        notice(app, "success", "Copied to clipboard", "Paste it wherever you like.");
    } else {
        macos::restore_focus(active.focus_pid);
        if let Err(e) = macos::paste_text(&output) {
            macos::copy_text(output.trim_end())?;
            notice(app, "warning", "Couldn't paste", &format!("{e}. The text is on your clipboard."));
        }
    }

    let paste_ms = paste_started.elapsed().as_millis() as u64;
    log::info!(
        "dictation {:.1}s: total {}ms = drain {drain_ms} + model-wait {model_wait_ms} + transcribe {} + post {post_ms} + paste {paste_ms} | mic start {}ms | conf {:.2} | rms {:.4}: {:?}",
        captured.duration.as_secs_f32(),
        released.elapsed().as_millis(),
        transcript.elapsed_ms,
        captured.start_latency_ms,
        transcript.confidence,
        captured.rms,
        cleaned.text
    );

    let mut record = make_record(
        cleaned.text.clone(),
        transcript.text.clone(),
        &model_id,
        captured.duration.as_millis() as u64,
        transcript.elapsed_ms,
    );
    if settings.keep_audio {
        let path = c.dir.join("audio").join(format!("{}.wav", record.id));
        match crate::audio::write_wav_16k(&path, &captured.samples_16k) {
            Ok(()) => record.has_audio = true,
            Err(e) => log::warn!("could not keep audio: {e}"),
        }
        prune_audio(&c.dir.join("audio"), 1000);
    }
    {
        let mut store = c.store.lock();
        store.add_record(record.clone());
        let mut new_suggestions = false;
        for (heard, intended) in postprocess::detect_suggestions(&cleaned.text) {
            new_suggestions |= store.note_suggestion(&heard, &intended, "whisperKnownFailure", 0.8);
        }
        if new_suggestions {
            let _ = app.emit("dictionary-changed", ());
        }
    }
    let _ = app.emit("transcription-complete", json!({ "record": record }));
    let _ = app.emit("history-changed", ());
    Ok(())
}

/// Keeps only the newest `keep` saved dictation WAVs.
fn prune_audio(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// Blocks until the selected model is loaded (loading it if necessary).
fn wait_for_model(app: &AppHandle) -> anyhow::Result<String> {
    let c = core(app);
    let wanted = c.settings.read().model.clone();
    for _ in 0..600 {
        if let Some(id) = c.engine.loaded_model() {
            return Ok(id);
        }
        let state = c.model_state.lock().0;
        if state == crate::ModelState::Error || state == crate::ModelState::Unloaded {
            if !crate::ensure_model_loaded(app) {
                let name = catalog_entry(&wanted).map(|e| e.display_name).unwrap_or("selected model");
                anyhow::bail!("{name} isn't downloaded. Open Settings → Voice to download a model.");
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("model is taking too long to load")
}

pub fn widget_visibility_changed(app: &AppHandle, vis: WidgetVisibility) {
    let (state, _) = core(app).sessions.current();
    widget::apply_visibility(app, vis, state);
}
