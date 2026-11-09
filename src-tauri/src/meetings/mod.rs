//! Meeting notes: record a meeting (microphone and the other side of the call), transcribe it
//! while it runs, tell speakers apart, and write notes with a local language model at the end.
//!
//! Each meeting lives in `<data>/meetings/<id>/`: `meeting.json` plus `mic.wav` and `system.wav`
//! (16 kHz, deleted after `meetingAudioDays`). While a meeting is recorded or processed, the copy
//! in memory is the truth and every edit goes through it; otherwise edits load and save the file.

mod pipeline;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    audio::{Recording, StreamResampler, WavWriter, TARGET_RATE},
    llm,
    session::notice,
    settings::Settings,
    speakers::{self, Embedder},
    storage::{read_json, write_json},
    store::new_id,
    vocab, Core,
};
pub use pipeline::ME;
use pipeline::{ChannelProcessor, Ctx, EchoFilter, LiveSpeakers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Mic,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub channel: Channel,
    pub speaker: u32,
    pub text: String,
    /// Language it was transcribed in (ISO 639-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speaker {
    pub id: u32,
    pub name: String,
    /// The user named this speaker (the name survives re-labelling).
    #[serde(default)]
    pub renamed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Recording,
    /// Transcribing the last bit and telling speakers apart.
    Processing,
    Summarizing,
    #[default]
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Meeting {
    pub id: String,
    /// Empty until the user or the summary names it.
    pub title: String,
    pub title_from_user: bool,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: u64,
    pub status: Status,
    /// The user's own notes, typed during or after the meeting.
    pub notes: String,
    pub segments: Vec<Segment>,
    pub speakers: Vec<Speaker>,
    /// Markdown notes written by the language model.
    pub summary: Option<String>,
    pub summary_model: Option<String>,
    pub language: Option<String>,
    /// Why the summary is missing (or what went wrong while recording).
    pub error: Option<String>,
    pub system_audio: bool,
    /// Stopped by the time limit rather than by the user.
    pub stopped_by_limit: bool,
    /// Recordings are still on disk (filled in when read).
    #[serde(skip_deserializing)]
    pub has_audio: bool,
}

/// What the meeting list shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingItem {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub status: Status,
    pub has_summary: bool,
    /// First sentence of the summary, or the first words said.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMeeting {
    pub id: String,
    pub started_at: String,
    /// When the time limit will stop it (RFC 3339), if there is one.
    pub stops_at: Option<String>,
}

struct Live {
    stop: Arc<AtomicBool>,
    info: ActiveMeeting,
}

#[derive(Default)]
pub struct Meetings {
    /// Meetings being recorded or processed; edits go to these copies.
    open: Mutex<HashMap<String, Arc<Mutex<Meeting>>>>,
    live: Mutex<Option<Live>>,
    /// One summary at a time: the model needs several GB.
    summarizing: Mutex<()>,
}

impl Meetings {
    pub fn active(&self) -> Option<ActiveMeeting> {
        self.live.lock().as_ref().map(|l| l.info.clone())
    }
}

// ---------- storage ----------

pub fn root(data: &Path) -> PathBuf {
    data.join("meetings")
}

fn dir_of(data: &Path, id: &str) -> PathBuf {
    root(data).join(id)
}

fn load(data: &Path, id: &str) -> Option<Meeting> {
    let mut m: Meeting = read_json(&dir_of(data, id).join("meeting.json"))?;
    m.has_audio = has_audio(data, id);
    Some(m)
}

fn has_audio(data: &Path, id: &str) -> bool {
    let d = dir_of(data, id);
    d.join("mic.wav").exists() || d.join("system.wav").exists()
}

fn save(data: &Path, m: &Meeting) {
    if let Err(e) = write_json(&dir_of(data, &m.id).join("meeting.json"), m) {
        log::error!("saving meeting {} failed: {e:#}", m.id);
    }
}

fn all(data: &Path) -> Vec<Meeting> {
    let Ok(entries) = std::fs::read_dir(root(data)) else { return Vec::new() };
    let mut out: Vec<Meeting> =
        entries.flatten().filter_map(|e| load(data, &e.file_name().to_string_lossy())).collect();
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

fn preview(m: &Meeting) -> String {
    let from_summary = m.summary.as_deref().and_then(|s| {
        s.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-')).map(str::to_string)
    });
    let text = from_summary.unwrap_or_else(|| m.segments.iter().take(6).map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "));
    let mut cut: String = text.chars().take(160).collect();
    if cut.len() < text.len() {
        cut.push('…');
    }
    cut
}

fn current(app: &AppHandle, id: &str) -> Option<Meeting> {
    let c = app.state::<Core>();
    let open = c.meetings.open.lock().get(id).cloned();
    match open {
        Some(m) => {
            let mut m = m.lock().clone();
            m.has_audio = has_audio(&c.dir, id);
            Some(m)
        }
        None => load(&c.dir, id),
    }
}

pub fn list(app: &AppHandle) -> Vec<MeetingItem> {
    let c = app.state::<Core>();
    let mut meetings = all(&c.dir);
    // Open meetings may be ahead of their files.
    for m in meetings.iter_mut() {
        if let Some(live) = c.meetings.open.lock().get(&m.id) {
            *m = live.lock().clone();
        }
    }
    meetings
        .iter()
        .map(|m| MeetingItem {
            id: m.id.clone(),
            title: m.title.clone(),
            started_at: m.started_at.clone(),
            duration_ms: m.duration_ms,
            status: m.status,
            has_summary: m.summary.is_some(),
            preview: preview(m),
        })
        .collect()
}

pub fn get(app: &AppHandle, id: &str) -> anyhow::Result<Meeting> {
    current(app, id).ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))
}

/// Applies `f` to a meeting (the open copy if there is one) and saves it.
pub fn edit(app: &AppHandle, id: &str, f: impl FnOnce(&mut Meeting)) -> anyhow::Result<Meeting> {
    let c = app.state::<Core>();
    let open = c.meetings.open.lock().get(id).cloned();
    let m = match open {
        Some(m) => {
            let mut g = m.lock();
            f(&mut g);
            save(&c.dir, &g);
            g.clone()
        }
        None => {
            let mut m = load(&c.dir, id).ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))?;
            f(&mut m);
            save(&c.dir, &m);
            m
        }
    };
    let _ = app.emit("meetings-changed", ());
    Ok(m)
}

pub fn delete(app: &AppHandle, id: &str) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    anyhow::ensure!(!c.meetings.open.lock().contains_key(id), "Stop the meeting before deleting it.");
    std::fs::remove_dir_all(dir_of(&c.dir, id))?;
    let _ = app.emit("meetings-changed", ());
    Ok(())
}

/// Deletes recordings older than `days` (0 = keep them).
pub fn prune_audio(data: &Path, days: u32) {
    if days == 0 {
        return;
    }
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
    for m in all(data) {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&m.started_at) else { continue };
        if started < cutoff && m.status == Status::Done {
            for f in ["mic.wav", "system.wav"] {
                let p = dir_of(data, &m.id).join(f);
                if p.exists() {
                    match std::fs::remove_file(&p) {
                        Ok(()) => log::info!("deleted {} of meeting {} (older than {days} days)", f, m.id),
                        Err(e) => log::warn!("could not delete {}: {e}", p.display()),
                    }
                }
            }
        }
    }
}

/// After a crash or forced quit, meetings may still say "recording". Their transcript up to the
/// last save is kept; the notes can be written from it.
pub fn recover(data: &Path) {
    for mut m in all(data) {
        if m.status != Status::Done {
            log::warn!("meeting {} was left {:?}; marking it done", m.id, m.status);
            if m.status == Status::Recording {
                m.error = Some("Wordfall quit while recording this meeting. The transcript up to that point was kept.".into());
            }
            m.status = Status::Done;
            if m.ended_at.is_none() {
                m.ended_at = Some(chrono::Utc::now().to_rfc3339());
            }
            save(data, &m);
        }
    }
}

// ---------- recording ----------

/// How often the live transcript catches up.
const PROCESS_EVERY: Duration = Duration::from_secs(8);
/// Process sooner when this much audio has piled up on a channel.
const PROCESS_AT: usize = 25 * 16_000;

fn emit_state(app: &AppHandle) {
    let active = app.state::<Core>().meetings.active();
    let _ = app.emit("meeting-state", json!({ "active": active }));
    crate::refresh_tray(app);
}

fn progress(app: &AppHandle, id: &str, phase: &str, fraction: Option<f32>, text: Option<&str>) {
    let _ = app.emit("meeting-progress", json!({ "id": id, "phase": phase, "fraction": fraction, "text": text }));
}

pub fn is_recording(app: &AppHandle) -> bool {
    app.state::<Core>().meetings.live.lock().is_some()
}

pub fn toggle(app: &AppHandle) {
    if is_recording(app) {
        stop(app);
    } else if let Err(e) = start(app) {
        notice(app, "error", "The meeting didn't start", &format!("{e:#}"));
    }
}

/// Starts recording a meeting. Returns its id.
pub fn start(app: &AppHandle) -> anyhow::Result<String> {
    let c = app.state::<Core>();
    let mut live = c.meetings.live.lock();
    anyhow::ensure!(live.is_none(), "A meeting is already being recorded.");
    let settings = c.settings.read().clone();
    if crate::platform::permissions().microphone != "granted" {
        crate::platform::request_microphone();
        anyhow::bail!("Wordfall needs microphone access to record meetings.");
    }
    crate::ensure_model_loaded(app);
    let mic = Recording::start(settings.input_device.as_deref(), settings.input_gain)
        .map_err(|e| anyhow::anyhow!("The microphone didn't start: {e}"))?;
    let system = if settings.meeting_system_audio {
        match Recording::start_system() {
            Ok(r) => Some(r),
            Err(e) => {
                log::warn!("system audio unavailable: {e:#}");
                notice(app, "warning", "Recording your microphone only", &format!("Other participants can't be recorded directly: {e}."));
                None
            }
        }
    } else {
        None
    };
    log::info!("meeting started: mic {}, system audio {}", mic.device_name, system.as_ref().map(|s| s.device_name.as_str()).unwrap_or("off"));

    let now = chrono::Utc::now();
    let meeting = Meeting {
        id: new_id(),
        started_at: now.to_rfc3339(),
        status: Status::Recording,
        system_audio: system.is_some(),
        speakers: vec![Speaker { id: ME, name: "Me".into(), renamed: false }],
        ..Default::default()
    };
    let id = meeting.id.clone();
    save(&c.dir, &meeting);
    let shared = Arc::new(Mutex::new(meeting));
    c.meetings.open.lock().insert(id.clone(), shared.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let limit = (settings.meeting_max_minutes > 0).then(|| Duration::from_secs(settings.meeting_max_minutes as u64 * 60));
    let info = ActiveMeeting {
        id: id.clone(),
        started_at: now.to_rfc3339(),
        stops_at: limit.map(|l| (now + chrono::Duration::from_std(l).unwrap_or_default()).to_rfc3339()),
    };
    *live = Some(Live { stop: stop.clone(), info });
    drop(live);

    // The speaker model is small; fetch it now if needed so it's there by the end at the latest.
    if settings.meeting_speakers {
        let dir = c.model_dirs.read().own.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = speakers::ensure_model(&dir).await {
                log::warn!("speaker model unavailable: {e:#}");
            }
        });
    }
    let app2 = app.clone();
    thread::Builder::new().name("wordfall-meeting".into()).spawn(move || {
        let busy = crate::platform::busy("Meeting");
        record(&app2, &shared, mic, system, &stop, limit, &settings);
        drop(busy);
        finish(&app2, &shared, &settings);
    })?;
    emit_state(app);
    let _ = app.emit("meetings-changed", ());
    Ok(id)
}

pub fn stop(app: &AppHandle) {
    if let Some(l) = app.state::<Core>().meetings.live.lock().as_ref() {
        l.stop.store(true, Ordering::SeqCst);
    }
}

struct Chan {
    channel: Channel,
    rec: Recording,
    resampler: StreamResampler,
    wav: Option<WavWriter>,
    proc: ChannelProcessor,
    /// 16 kHz samples so far (including filled gaps).
    received: u64,
    /// The start of this channel's timeline has been lined up with the microphone's.
    aligned: bool,
}

fn wait_for_recognizer(app: &AppHandle) {
    let c = app.state::<Core>();
    while c.sessions.dictation_busy() {
        thread::sleep(Duration::from_millis(50));
    }
}

fn load_embedder(app: &AppHandle, settings: &Settings) -> Option<Embedder> {
    if !settings.meeting_speakers {
        return None;
    }
    let path = speakers::model_path(&app.state::<Core>().model_dirs.read().own)?;
    match Embedder::load(&path) {
        Ok(e) => Some(e),
        Err(e) => {
            log::warn!("speaker model failed to load: {e:#}");
            None
        }
    }
}

fn record(
    app: &AppHandle,
    meeting: &Arc<Mutex<Meeting>>,
    mic: Recording,
    system: Option<Recording>,
    stop: &AtomicBool,
    limit: Option<Duration>,
    settings: &Settings,
) {
    let c = app.state::<Core>();
    let (id, dir) = {
        let m = meeting.lock();
        (m.id.clone(), dir_of(&c.dir, &m.id))
    };
    let open_wav = |name: &str| match WavWriter::open(&dir.join(name)) {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("can't keep the meeting recording {name}: {e:#}");
            None
        }
    };
    let mut chans: Vec<Chan> = Vec::new();
    if let Some(sys) = system {
        chans.push(Chan {
            channel: Channel::System,
            resampler: StreamResampler::new(sys.sample_rate(), TARGET_RATE),
            rec: sys,
            wav: open_wav("system.wav"),
            proc: ChannelProcessor::new(Channel::System),
            received: 0,
            aligned: false,
        });
    }
    chans.push(Chan {
        channel: Channel::Mic,
        resampler: StreamResampler::new(mic.sample_rate(), TARGET_RATE),
        rec: mic,
        wav: open_wav("mic.wav"),
        proc: ChannelProcessor::new(Channel::Mic),
        received: 0,
        aligned: true,
    });

    let mut embedder = load_embedder(app, settings);
    let mut embeddings: Vec<Option<Vec<f32>>> = Vec::new();
    let mut live = LiveSpeakers::new(settings.meeting_speakers, &meeting.lock().speakers);
    let mut echo = EchoFilter::default();
    let started = Instant::now();
    let mut last_pass = Instant::now();
    let mut last_save = Instant::now();
    let mut stopped_by_limit = false;
    let mut buf = Vec::new();
    let mut device_check = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(200));
        // Windows loopback follows one device; when the output changes (headphones plugged in),
        // follow it, or the other side would go silent.
        #[cfg(windows)]
        if device_check.elapsed() > Duration::from_secs(2) {
            device_check = Instant::now();
            if let Some(ch) = chans.iter_mut().find(|c| c.channel == Channel::System) {
                let current = crate::audio::default_output_name();
                if current.as_deref().is_some_and(|n| n != ch.rec.device_name) {
                    match Recording::start_system() {
                        Ok(r) => {
                            log::info!("system audio moved to {}", r.device_name);
                            ch.resampler = StreamResampler::new(r.sample_rate(), TARGET_RATE);
                            ch.rec = r;
                        }
                        Err(e) => log::warn!("could not follow the new output device: {e:#}"),
                    }
                }
            }
        }
        #[cfg(not(windows))]
        let _ = &mut device_check;
        let elapsed = started.elapsed();
        let expected = (elapsed.as_secs_f64() * TARGET_RATE as f64) as u64;
        let mut levels = json!({});
        let mic_first = chans.iter().find(|c| c.channel == Channel::Mic).and_then(|c| c.rec.first_sample_at());
        for ch in chans.iter_mut() {
            buf.clear();
            ch.resampler.push(&ch.rec.take(), &mut buf);
            // The system tap starts after the microphone (on macOS it builds an aggregate device
            // first), so its first sample was heard later. Unless its timeline starts that much
            // later too, everything on it is early against the microphone (by about a third of a
            // second on a MacBook), and echo of it is looked for in the wrong places.
            if !ch.aligned && !buf.is_empty() {
                if let (Some(first), Some(mic)) = (ch.rec.first_sample_at(), mic_first) {
                    ch.aligned = true;
                    let late = first.saturating_duration_since(mic);
                    if ch.received == 0 && late < Duration::from_secs(5) {
                        let pad = (late.as_secs_f64() * TARGET_RATE as f64) as usize;
                        buf.splice(0..0, std::iter::repeat(0.0).take(pad));
                        log::info!("system audio started {} ms after the microphone; lined up", late.as_millis());
                    }
                }
            }
            // Windows loopback delivers nothing while nothing plays: fill with silence so both
            // channels keep the same timeline.
            let behind = expected.saturating_sub(ch.received + buf.len() as u64);
            if ch.channel == Channel::System && behind > TARGET_RATE as u64 / 2 {
                let fill = (behind - TARGET_RATE as u64 / 10) as usize;
                buf.splice(0..0, std::iter::repeat(0.0).take(fill));
            }
            ch.received += buf.len() as u64;
            if let Some(w) = ch.wav.as_mut() {
                if let Err(e) = w.append(&buf) {
                    log::warn!("writing the meeting recording failed: {e:#}");
                    ch.wav = None;
                }
            }
            ch.proc.push(&buf);
            let level = ch.rec.levels().iter().copied().fold(0.0f32, f32::max);
            levels[if ch.channel == Channel::Mic { "mic" } else { "system" }] = json!(level);
        }
        let _ = app.emit("meeting-levels", &levels);
        meeting.lock().duration_ms = elapsed.as_millis() as u64;

        if limit.is_some_and(|l| elapsed >= l) && !stop.load(Ordering::SeqCst) {
            stopped_by_limit = true;
            stop.store(true, Ordering::SeqCst);
        }
        let stopping = stop.load(Ordering::SeqCst);
        let due = last_pass.elapsed() >= PROCESS_EVERY || chans.iter().any(|ch| ch.proc.pending_samples() >= PROCESS_AT);
        if due || stopping {
            last_pass = Instant::now();
            if stopping {
                meeting.lock().status = Status::Processing;
                let _ = app.emit("meeting-changed", json!({ "id": id }));
                progress(app, &id, "transcribing", None, None);
            }
            if embedder.is_none() && settings.meeting_speakers {
                embedder = load_embedder(app, settings);
            }
            let added =
                transcribe_pass(app, meeting, &mut chans, embedder.as_ref(), &mut embeddings, &mut live, &mut echo, settings, stopping);
            if added > 0 || last_save.elapsed() > Duration::from_secs(30) {
                save(&c.dir, &meeting.lock());
                last_save = Instant::now();
            }
        }
        if stopping {
            break;
        }
    }
    drop(chans);
    {
        let mut m = meeting.lock();
        m.duration_ms = started.elapsed().as_millis() as u64;
        m.ended_at = Some(chrono::Utc::now().to_rfc3339());
        m.stopped_by_limit = stopped_by_limit;
    }
    *c.meetings.live.lock() = None;
    emit_state(app);
    if stopped_by_limit {
        let hours = settings.meeting_max_minutes as f32 / 60.0;
        notice(app, "info", "Meeting stopped", &format!("It reached the {hours} h limit. Change it in Settings › Meetings."));
    }

    // Final speaker labels over the whole meeting.
    progress(app, &id, "speakers", None, None);
    let embedder = embedder.or_else(|| load_embedder(app, settings));
    let mut m = meeting.lock().clone();
    if let Some(e) = embedder.as_ref() {
        fill_embeddings(&dir, &m.segments, &mut embeddings, e);
    }
    embeddings.resize(m.segments.len(), None);
    let dropped = pipeline::drop_echo(&mut m.segments, &mut embeddings, &echo);
    if !dropped.is_empty() {
        let by_voice = dropped.iter().filter(|(_, why)| matches!(why, pipeline::Echo::Voice(..))).count();
        log::info!("dropped {} microphone segments that echoed the other side ({by_voice} by voice)", dropped.len());
    }
    pipeline::assign_speakers(&mut m.segments, &embeddings, &mut m.speakers, settings.meeting_speakers && embedder.is_some());
    m.language = pipeline::meeting_language(&m.segments);
    *meeting.lock() = m;
    save(&c.dir, &meeting.lock());
    let _ = app.emit("meeting-changed", json!({ "id": id }));
}

/// Computes embeddings for segments that don't have one yet (e.g. the model arrived late) from
/// the saved recordings.
fn fill_embeddings(dir: &Path, segments: &[Segment], embeddings: &mut Vec<Option<Vec<f32>>>, embedder: &Embedder) {
    embeddings.resize(segments.len(), None);
    for (s, e) in segments.iter().zip(embeddings.iter_mut()) {
        if e.is_some() {
            continue;
        }
        let file = dir.join(if s.channel == Channel::Mic { "mic.wav" } else { "system.wav" });
        let start = (s.start_ms * 16) as usize;
        let len = (s.end_ms.saturating_sub(s.start_ms) * 16) as usize;
        if let Ok(audio) = crate::audio::read_wav_16k_range(&file, start, len) {
            *e = embedder.embed(&audio).ok().flatten();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn transcribe_pass(
    app: &AppHandle,
    meeting: &Arc<Mutex<Meeting>>,
    chans: &mut [Chan],
    embedder: Option<&Embedder>,
    embeddings: &mut Vec<Option<Vec<f32>>>,
    live: &mut LiveSpeakers,
    echo: &mut EchoFilter,
    settings: &Settings,
    flush: bool,
) -> usize {
    let c = app.state::<Core>();
    if crate::session::wait_for_model(app).is_err() {
        // Keep the audio; the next pass tries again (the recording on disk is complete anyway).
        return 0;
    }
    let (dictionary, prompt) = {
        let store = c.store.lock();
        let dict_terms: Vec<String> = store.dictionary.iter().map(|e| e.replacement.clone()).collect();
        let terms = vocab::terms(
            settings.prompt_mode,
            &vocab::Sources { dictionary: &dict_terms, user_terms: &store.vocabulary, projects: &c.projects },
        );
        (settings.dictionary_enabled.then(|| store.dictionary.clone()), vocab::prompt(&terms))
    };
    let yield_fn = || wait_for_recognizer(app);
    let ctx = Ctx {
        engine: &c.engine,
        embedder,
        languages: &settings.languages,
        prompt: prompt.as_deref(),
        decode: &settings.decode,
        dictionary: dictionary.as_deref(),
        remove_fillers: settings.remove_fillers,
        yield_to_dictation: &yield_fn,
    };
    let mut new_segments = Vec::new();
    // System audio first, so microphone echo of it can be recognised.
    let mut system_speech = Vec::new();
    let mut system_language = None;
    for ch in chans.iter_mut() {
        if ch.channel == Channel::Mic {
            ch.proc.adopt_language(system_language.as_deref());
        }
        let heard = match ch.proc.process(&ctx, flush, &system_speech) {
            Ok(h) => h,
            Err(e) => {
                log::error!("meeting transcription failed: {e:#}");
                continue;
            }
        };
        if ch.channel == Channel::System {
            system_speech = ch.proc.speech.clone();
            system_language = ch.proc.language.clone();
        }
        let mut m = meeting.lock();
        for h in heard {
            let seg = Segment {
                start_ms: h.start_ms,
                end_ms: h.end_ms,
                channel: ch.channel,
                speaker: ME,
                text: h.text,
                language: h.language,
            };
            if seg.channel == Channel::Mic && echo.check(&seg, h.embedding.as_deref(), &m.segments).is_some() {
                continue;
            }
            let Meeting { speakers, .. } = &mut *m;
            let speaker = live.assign(seg.channel, h.embedding.as_deref(), speakers);
            let seg = Segment { speaker, ..seg };
            m.segments.push(seg.clone());
            embeddings.push(h.embedding);
            new_segments.push(seg);
        }
    }
    if !new_segments.is_empty() {
        let m = meeting.lock();
        let _ = app.emit("meeting-segments", json!({ "id": m.id, "segments": new_segments, "speakers": m.speakers }));
    }
    new_segments.len()
}

/// After recording: writes the notes if a summary model is there.
fn finish(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>, settings: &Settings) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
    let has_speech = !meeting.lock().segments.is_empty();
    let model = llm::entry(&settings.summary_model).filter(|e| llm::locate(&c.model_dirs.read().own, e).is_some());
    if !has_speech {
        let mut m = meeting.lock();
        m.status = Status::Done;
        m.error = Some("Nothing was said, so there's nothing to summarise.".into());
    } else if model.is_none() {
        let mut m = meeting.lock();
        m.status = Status::Done;
        m.error = Some("Download a summary model in Settings › Meetings to get notes for this meeting.".into());
    } else {
        write_summary(app, meeting);
    }
    save(&c.dir, &meeting.lock());
    c.meetings.open.lock().remove(&id);
    prune_audio(&c.dir, settings.meeting_audio_days);
    let _ = app.emit("meeting-changed", json!({ "id": id }));
    let _ = app.emit("meetings-changed", ());
}

/// Gets the summary model's GPU code compiled before the first notes need it (see
/// `llm::warm_up`), unless a meeting is being recorded or summarised.
pub fn warm_up_summaries(app: &AppHandle) {
    let c = app.state::<Core>();
    if is_recording(app) {
        return;
    }
    let Some(_one_at_a_time) = c.meetings.summarizing.try_lock() else { return };
    let chosen = c.settings.read().summary_model.clone();
    let own = c.model_dirs.read().own.clone();
    // The kernels don't depend on the model; any downloaded one will do.
    let model = llm::entry(&chosen)
        .and_then(|e| llm::locate(&own, e))
        .or_else(|| llm::CATALOG.iter().find_map(|e| llm::locate(&own, e)));
    if let Some(model) = model {
        llm::warm_up(&model, &c.dir);
    }
}

/// Writes (or rewrites) the notes of a finished meeting in the background.
pub fn summarize(app: &AppHandle, id: &str) -> anyhow::Result<()> {
    let c = app.state::<Core>();
    anyhow::ensure!(!c.meetings.open.lock().contains_key(id), "This meeting is still being processed.");
    let m = load(&c.dir, id).ok_or_else(|| anyhow::anyhow!("That meeting no longer exists."))?;
    anyhow::ensure!(!m.segments.is_empty(), "Nothing was said in this meeting.");
    let settings = c.settings.read().clone();
    let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("Pick a summary model in Settings › Meetings."))?;
    anyhow::ensure!(
        llm::locate(&c.model_dirs.read().own, entry).is_some(),
        "{} isn't downloaded yet. Download it in Settings › Meetings.",
        entry.display_name
    );
    let shared = Arc::new(Mutex::new(m));
    c.meetings.open.lock().insert(id.to_string(), shared.clone());
    let app = app.clone();
    let id = id.to_string();
    thread::spawn(move || {
        write_summary(&app, &shared);
        let c = app.state::<Core>();
        save(&c.dir, &shared.lock());
        c.meetings.open.lock().remove(&id);
        let _ = app.emit("meeting-changed", json!({ "id": id }));
        let _ = app.emit("meetings-changed", ());
    });
    Ok(())
}

fn write_summary(app: &AppHandle, meeting: &Arc<Mutex<Meeting>>) {
    let c = app.state::<Core>();
    let id = meeting.lock().id.clone();
    {
        let mut m = meeting.lock();
        m.status = Status::Summarizing;
        m.error = None;
    }
    let _ = app.emit("meeting-changed", json!({ "id": id }));
    progress(app, &id, "loading", None, None);
    let _one_at_a_time = c.meetings.summarizing.lock();
    let settings = c.settings.read().clone();
    let snapshot = meeting.lock().clone();
    let result = (|| -> anyhow::Result<(String, String)> {
        let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
        let path = llm::locate(&c.model_dirs.read().own, entry)
            .ok_or_else(|| anyhow::anyhow!("{} isn't downloaded", entry.display_name))?;
        let started = Instant::now();
        let mut worker = llm::Worker::spawn(&path, &c.dir.join("llm.log"))?;
        let lines = pipeline::transcript_lines(&snapshot.segments, &snapshot.speakers);
        let used: Vec<u32> = snapshot.segments.iter().map(|s| s.speaker).collect();
        let speakers: Vec<String> =
            snapshot.speakers.iter().filter(|s| used.contains(&s.id)).map(|s| s.name.clone()).collect();
        let when = chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
            .map(|t| t.with_timezone(&chrono::Local).format("%A %-d %B %Y, %H:%M").to_string())
            .unwrap_or_default();
        let input = llm::SummaryInput {
            title: snapshot.title_from_user.then_some(snapshot.title.as_str()).filter(|t| !t.trim().is_empty()),
            notes: &snapshot.notes,
            transcript: &lines,
            speakers: &speakers,
            language: snapshot.language.as_deref(),
            when: &when,
            minutes: snapshot.duration_ms / 60_000,
        };
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let summary = llm::summarize(&mut worker, &input, |p| {
            if last_emit.elapsed() < Duration::from_millis(120) {
                return;
            }
            last_emit = Instant::now();
            match p {
                llm::Progress::Reading(f) => progress(app, &id, "reading", Some(f), None),
                llm::Progress::Writing(t) => progress(app, &id, "writing", None, Some(t)),
            }
        })?;
        log::info!("meeting notes written in {:.1}s", started.elapsed().as_secs_f32());
        Ok((summary, entry.id.to_string()))
    })();
    let mut m = meeting.lock();
    m.status = Status::Done;
    match result {
        Ok((summary, model)) => {
            if !m.title_from_user {
                if let Some(t) = llm::title_of(&summary) {
                    m.title = t;
                }
            }
            m.summary = Some(summary);
            m.summary_model = Some(model);
            drop(m);
            notice(app, "success", "Meeting notes are ready", &meeting.lock().title);
        }
        Err(e) => {
            log::error!("meeting summary failed: {e:#}");
            m.error = Some(format!("The notes couldn't be written: {e:#}"));
        }
    }
}

// ---------- `wordfall --meeting` ----------

/// `wordfall --meeting <mic.wav> [--system <system.wav>] [--no-summary]`: runs a recorded meeting
/// through the same pipeline as a live one (fed in real-time-sized pieces) and prints the
/// transcript and notes.
pub fn cli(args: &[String]) -> anyhow::Result<()> {
    use crate::{store::Store, transcribe::{catalog_entry, Engine}};
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let mic_path = arg("--meeting").ok_or_else(|| anyhow::anyhow!("missing mic WAV"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let settings = Settings::load(&data_dir);
    let model_dirs = settings.model_dirs(&data_dir);
    let entry = catalog_entry(&settings.model).ok_or_else(|| anyhow::anyhow!("unknown model"))?;
    let (path, _) = model_dirs.locate(entry).ok_or_else(|| anyhow::anyhow!("{} is not downloaded", entry.id))?;
    let engine = Engine::default();
    engine.load(entry, &path)?;
    *engine.silero_path.lock() = Some(model_dirs.own.join(crate::transcribe::SILERO_FILE)).filter(|p| p.exists());
    if settings.languages.len() > 1 {
        engine.prepare_detection()?;
    }
    let embedder = speakers::model_path(&model_dirs.own).map(|p| Embedder::load(&p)).transpose()?;
    let dictionary = Store::load(&data_dir).dictionary;

    let mut sources = vec![(Channel::Mic, crate::audio::read_wav_16k(Path::new(&mic_path))?)];
    if let Some(sys) = arg("--system") {
        sources.insert(0, (Channel::System, crate::audio::read_wav_16k(Path::new(&sys))?));
    }
    let started = Instant::now();
    let no_yield = || {};
    let ctx = Ctx {
        engine: &engine,
        embedder: embedder.as_ref(),
        languages: &settings.languages,
        prompt: None,
        decode: &settings.decode,
        dictionary: Some(&dictionary),
        remove_fillers: true,
        yield_to_dictation: &no_yield,
    };
    let mut procs: Vec<(ChannelProcessor, &[f32])> =
        sources.iter().map(|(ch, audio)| (ChannelProcessor::new(*ch), audio.as_slice())).collect();
    let longest = sources.iter().map(|(_, a)| a.len()).max().unwrap_or(0);
    let step = 8 * 16_000;
    let mut meeting = Meeting { id: "cli".into(), started_at: chrono::Utc::now().to_rfc3339(), ..Default::default() };
    meeting.speakers.push(Speaker { id: ME, name: "Me".into(), renamed: false });
    let mut embeddings = Vec::new();
    let mut live = LiveSpeakers::new(embedder.is_some(), &meeting.speakers);
    let mut echo = EchoFilter::default();
    let mut pos = 0;
    while pos < longest {
        let end = (pos + step).min(longest);
        let flush = end == longest;
        let mut system_speech = Vec::new();
        let mut system_language = None;
        for (proc, audio) in procs.iter_mut() {
            proc.push(&audio[pos.min(audio.len())..end.min(audio.len())]);
            if proc.channel == Channel::Mic {
                proc.adopt_language(system_language.as_deref());
            }
            let heard = proc.process(&ctx, flush, &system_speech)?;
            if proc.channel == Channel::System {
                system_speech = proc.speech.clone();
                system_language = proc.language.clone();
            }
            for h in heard {
                let seg = Segment {
                    start_ms: h.start_ms,
                    end_ms: h.end_ms,
                    channel: proc.channel,
                    speaker: ME,
                    text: h.text,
                    language: h.language,
                };
                if seg.channel == Channel::Mic {
                    if let Some(why) = echo.check(&seg, h.embedding.as_deref(), &meeting.segments) {
                        eprintln!("(echo dropped, {why:?}) {}", seg.text);
                        continue;
                    }
                }
                let speaker = live.assign(seg.channel, h.embedding.as_deref(), &mut meeting.speakers);
                eprintln!("[{:>6.1}s] {:?} {} | {}", seg.start_ms as f32 / 1000.0, seg.channel, speaker, seg.text);
                meeting.segments.push(Segment { speaker, ..seg });
                embeddings.push(h.embedding);
            }
        }
        pos = end;
    }
    meeting.duration_ms = longest as u64 / 16;
    let transcribed = started.elapsed();
    for (seg, why) in pipeline::drop_echo(&mut meeting.segments, &mut embeddings, &echo) {
        eprintln!("(echo dropped at the end, {why:?}) [{:>6.1}s] {}", seg.start_ms as f32 / 1000.0, seg.text);
    }
    pipeline::assign_speakers(&mut meeting.segments, &embeddings, &mut meeting.speakers, embedder.is_some());
    meeting.language = pipeline::meeting_language(&meeting.segments);
    let lines = pipeline::transcript_lines(&meeting.segments, &meeting.speakers);
    println!("--- transcript ({:.1}s for {:.0}s of audio) ---", transcribed.as_secs_f32(), longest as f32 / 16_000.0);
    for l in &lines {
        println!("{l}");
    }
    if args.iter().any(|a| a == "--no-summary") {
        return Ok(());
    }
    let speakers: Vec<String> = meeting.speakers.iter().map(|s| s.name.clone()).collect();
    print_notes(&data_dir, &settings, &lines, &speakers, meeting.language.as_deref(), meeting.duration_ms / 60_000, args)
}

/// `wordfall --meeting-notes <transcript.txt> [--language de] [--minutes 30] [--notes "…"]`: writes
/// notes for a transcript as `--meeting` prints it ("[mm:ss] Speaker: text" lines), without the
/// audio. For working on the notes prompt.
pub fn cli_notes(args: &[String]) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let path = arg("--meeting-notes").ok_or_else(|| anyhow::anyhow!("missing transcript file"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let settings = Settings::load(&data_dir);
    let line = regex::Regex::new(r"^\[[\d:]+\] ([^:]+): ")?;
    let lines: Vec<String> = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?.lines().filter(|l| line.is_match(l)).map(str::to_string).collect();
    anyhow::ensure!(!lines.is_empty(), "no transcript lines in {path}");
    let mut speakers: Vec<String> = Vec::new();
    for l in &lines {
        let name = line.captures(l).expect("matched above")[1].to_string();
        if !speakers.contains(&name) {
            speakers.push(name);
        }
    }
    let minutes = arg("--minutes").and_then(|m| m.parse().ok()).unwrap_or(1);
    print_notes(&data_dir, &settings, &lines, &speakers, arg("--language").as_deref(), minutes, args)
}

fn print_notes(
    data_dir: &Path,
    settings: &Settings,
    lines: &[String],
    speakers: &[String],
    language: Option<&str>,
    minutes: u64,
    args: &[String],
) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let entry = llm::entry(&settings.summary_model).ok_or_else(|| anyhow::anyhow!("unknown summary model"))?;
    let path = llm::locate(&settings.model_dirs(data_dir).own, entry).ok_or_else(|| anyhow::anyhow!("{} is not downloaded", entry.id))?;
    let t = Instant::now();
    let mut worker = llm::Worker::spawn(&path, &data_dir.join("llm.log"))?;
    let notes = arg("--notes").unwrap_or_default();
    let input = llm::SummaryInput {
        title: None,
        notes: &notes,
        transcript: lines,
        speakers,
        language,
        when: &chrono::Local::now().format("%A %-d %B %Y, %H:%M").to_string(),
        minutes,
    };
    let summary = llm::summarize(&mut worker, &input, |_| {})?;
    println!("--- notes ({:.1}s, language {language:?}) ---\n{summary}", t.elapsed().as_secs_f32());
    Ok(())
}
