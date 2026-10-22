//! Tauri IPC commands (see docs/ipc.md).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{atomic::{AtomicBool, Ordering}, Arc},
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    audio, lab, macos,
    session::{self, RecMode, RecState},
    settings::Settings,
    store::{self, DictEntry, HistoryRecord, ImportResult, Stats, Suggestion},
    transcribe::{self, catalog_entry, ModelInfo},
    widget, Core, ModelState,
};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    settings: Settings,
    stats: Stats,
    rec_state: RecState,
    rec_mode: Option<RecMode>,
    model_state: ModelState,
    model_error: Option<String>,
    active_model: Option<String>,
    permissions: macos::Permissions,
    fn_usage: macos::FnUsage,
    ptt_listening: bool,
    version: String,
}

#[tauri::command]
pub fn get_snapshot(app: AppHandle, core: State<'_, Core>) -> Snapshot {
    let (rec_state, rec_mode) = core.sessions.current();
    let (model_state, model_error) = core.model_state.lock().clone();
    Snapshot {
        settings: core.settings.read().clone(),
        stats: core.store.lock().stats(),
        rec_state,
        rec_mode,
        model_state,
        model_error,
        active_model: core.engine.loaded_model(),
        permissions: macos::permissions(),
        fn_usage: macos::fn_usage(),
        ptt_listening: core.listener.get().is_some_and(|l| l.installed.load(Ordering::Relaxed)),
        version: app.package_info().version.to_string(),
    }
}

#[tauri::command]
pub fn update_settings(app: AppHandle, core: State<'_, Core>, patch: Value) -> CmdResult<Settings> {
    let (before, after) = {
        let mut s = core.settings.write();
        let before = s.clone();
        let next = s.patched(patch).map_err(err)?;
        next.save(&core.dir).map_err(err)?;
        *s = next.clone();
        (before, next)
    };
    if before.ptt_key != after.ptt_key {
        if let Some(l) = core.listener.get() {
            l.set_key(after.ptt_key);
        }
    }
    if before.toggle_shortcut != after.toggle_shortcut {
        crate::register_shortcuts(&app);
    }
    if before.launch_at_login != after.launch_at_login {
        crate::apply_autostart(&app);
    }
    if before.model_folders != after.model_folders {
        *core.model_dirs.write() = after.model_dirs(&core.dir);
        let _ = app.emit("models-changed", ());
    }
    if before.model != after.model || before.model_folders != after.model_folders {
        crate::ensure_model_loaded(&app);
    }
    if before.widget_visibility != after.widget_visibility {
        session::widget_visibility_changed(&app, after.widget_visibility);
    }
    if before.follow_cursor != after.follow_cursor && after.follow_cursor {
        widget::position(&app);
    }
    let _ = app.emit("settings-changed", &after);
    Ok(after)
}

#[derive(Serialize)]
pub struct HistoryPage {
    total: usize,
    records: Vec<HistoryRecord>,
}

#[tauri::command]
pub fn get_history(core: State<'_, Core>, query: Option<String>, offset: usize, limit: usize) -> HistoryPage {
    let (total, mut records) = core.store.lock().history_page(query.as_deref(), offset, limit.min(500));
    let audio = core.dir.join("audio");
    for r in &mut records {
        r.has_audio = audio.join(format!("{}.wav", r.id)).exists();
    }
    HistoryPage { total, records }
}

/// Replace a transcript with the user's correction; it becomes benchmark ground truth.
#[tauri::command]
pub fn correct_history(app: AppHandle, core: State<'_, Core>, id: String, text: String) -> CmdResult<()> {
    if text.trim().is_empty() {
        return Err("The corrected text can't be empty.".into());
    }
    if !core.store.lock().set_reference(&id, Some(text)) {
        return Err("Transcript not found.".into());
    }
    let _ = app.emit("history-changed", ());
    Ok(())
}

/// Mark a transcript as exactly right (ground truth = current text).
#[tauri::command]
pub fn verify_history(app: AppHandle, core: State<'_, Core>, id: String) -> CmdResult<()> {
    if !core.store.lock().set_reference(&id, None) {
        return Err("Transcript not found.".into());
    }
    let _ = app.emit("history-changed", ());
    Ok(())
}

#[tauri::command]
pub fn list_vocabulary(core: State<'_, Core>) -> Vec<String> {
    core.store.lock().vocabulary.clone()
}

#[tauri::command]
pub fn set_vocabulary(app: AppHandle, core: State<'_, Core>, terms: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let clean: Vec<String> = terms
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
        .collect();
    let mut store = core.store.lock();
    store.vocabulary = clean.clone();
    store.save_vocabulary();
    let _ = app.emit("dictionary-changed", ());
    clean
}

/// Suggested hint words: project folder names not yet in the vocabulary.
#[tauri::command]
pub fn suggested_vocabulary(core: State<'_, Core>) -> Vec<String> {
    let store = core.store.lock();
    core.projects
        .iter()
        .filter(|p| !store.vocabulary.iter().any(|v| v.eq_ignore_ascii_case(p)))
        .cloned()
        .collect()
}

// ---------- Voice Lab ----------

#[tauri::command]
pub fn lab_prompts(core: State<'_, Core>) -> Vec<lab::LabPrompt> {
    let store = core.store.lock();
    lab::prompts(&core.dir, &store, &core.projects)
}

#[tauri::command]
pub fn lab_start(app: AppHandle, core: State<'_, Core>, prompt_id: String) -> CmdResult<()> {
    if core.sessions.is_active() {
        return Err("Finish the current dictation first.".into());
    }
    if macos::permissions().microphone != "granted" {
        macos::request_microphone();
        return Err("Microphone access is needed.".into());
    }
    let s = core.settings.read().clone();
    let rec = audio::Recording::start(s.input_device.as_deref(), s.input_gain).map_err(err)?;
    *core.lab.lock() = Some((prompt_id, rec));
    let app2 = app.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(33));
        let levels = match app2.state::<Core>().lab.lock().as_ref() {
            Some((_, r)) => r.levels(),
            None => return,
        };
        let _ = app2.emit("lab-levels", json!({ "levels": levels }));
    });
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabStopResult {
    prompt_id: String,
    hypothesis: String,
    wer: f64,
    ms: u64,
    seconds: f32,
}

#[tauri::command]
pub async fn lab_stop(app: AppHandle) -> CmdResult<LabStopResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Core>();
        let (prompt_id, rec) = core.lab.lock().take().ok_or("Not recording.")?;
        let captured = rec.finish();
        let seconds = captured.samples_16k.len() as f32 / 16_000.0;
        if seconds < 0.4 {
            return Err("That was too short. Hold the button while you read.".to_string());
        }
        crate::ensure_model_loaded(&app);
        for _ in 0..300 {
            if core.engine.loaded_model().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let store = core.store.lock();
        let prompts = lab::prompts(&core.dir, &store, &core.projects);
        let prompt = prompts.iter().find(|p| p.id == prompt_id).ok_or("Unknown prompt.")?.clone();
        let settings = core.settings.read().clone();
        let terms = lab::prompt_terms(&store, &core.projects, settings.prompt_mode);
        let dictionary = store.dictionary.clone();
        drop(store);
        let r = lab::save_and_score(
            &core.dir,
            &prompt,
            &captured.samples_16k,
            &core.engine,
            &settings.decode,
            crate::vocab::prompt(&terms).as_deref(),
            &dictionary,
        )
        .map_err(err)?;
        Ok(LabStopResult { prompt_id, hypothesis: r.hypothesis, wer: r.wer, ms: r.ms, seconds })
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub fn lab_cancel(core: State<'_, Core>) {
    if let Some((_, rec)) = core.lab.lock().take() {
        drop(rec.finish());
    }
}

#[tauri::command]
pub fn lab_delete(core: State<'_, Core>, prompt_id: String) {
    lab::delete(&core.dir, &prompt_id);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchInfo {
    root: String,
    tiers: std::collections::BTreeMap<String, usize>,
    history_with_audio: usize,
    history_verified: usize,
    latest_report: Option<String>,
}

/// Corpus sizes and the newest report, for the Voice Lab page.
#[tauri::command]
pub fn bench_info(core: State<'_, Core>) -> BenchInfo {
    let root = core.dir.join("bench");
    let mut tiers = std::collections::BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten().filter(|e| e.path().is_dir()) {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "results" {
                continue;
            }
            let n = std::fs::read_dir(e.path())
                .map(|it| it.flatten().filter(|f| f.path().extension().is_some_and(|x| x == "wav")).count())
                .unwrap_or(0);
            tiers.insert(name, n);
        }
    }
    let audio = core.dir.join("audio");
    let store = core.store.lock();
    let with_audio: Vec<_> = store.all_records().iter().filter(|r| audio.join(format!("{}.wav", r.id)).exists()).collect();
    let latest_report = std::fs::read_dir(root.join("results"))
        .ok()
        .and_then(|it| it.flatten().map(|e| e.path()).filter(|p| p.join("report.html").exists()).max())
        .map(|p| p.join("report.html").display().to_string());
    BenchInfo {
        root: root.display().to_string(),
        tiers,
        history_with_audio: with_audio.len(),
        history_verified: with_audio.iter().filter(|r| r.reference.is_some()).count(),
        latest_report,
    }
}

/// Opens a file or folder inside Wordfall's data directory with the default app.
#[tauri::command]
pub fn open_path(core: State<'_, Core>, path: String) -> CmdResult<()> {
    let p = std::path::PathBuf::from(&path);
    if !p.starts_with(&core.dir) {
        return Err("Only Wordfall's own files can be opened.".into());
    }
    std::process::Command::new("open").arg(&p).spawn().map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn delete_history(app: AppHandle, core: State<'_, Core>, id: String) {
    core.store.lock().delete_record(&id);
    let _ = app.emit("history-changed", ());
}

#[tauri::command]
pub fn clear_history(app: AppHandle, core: State<'_, Core>) {
    core.store.lock().clear_history();
    let _ = app.emit("history-changed", ());
}

#[tauri::command]
pub fn export_history_csv(core: State<'_, Core>) -> CmdResult<String> {
    let csv = store::export_csv(core.store.lock().all_records());
    let dir = dirs::download_dir().or_else(dirs::home_dir).ok_or("no downloads folder")?;
    let path = dir.join(format!("wordfall-history-{}.csv", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::write(&path, csv).map_err(err)?;
    Ok(path.display().to_string())
}

#[tauri::command]
pub fn get_stats(core: State<'_, Core>) -> Stats {
    core.store.lock().stats()
}

#[tauri::command]
pub fn list_dictionary(core: State<'_, Core>) -> Vec<DictEntry> {
    core.store.lock().dictionary.clone()
}

#[tauri::command]
pub fn upsert_dictionary(
    app: AppHandle,
    core: State<'_, Core>,
    id: Option<String>,
    original: String,
    replacement: String,
) -> CmdResult<DictEntry> {
    if original.trim().is_empty() || replacement.trim().is_empty() {
        return Err("Both the heard word and its replacement are required.".into());
    }
    let e = core.store.lock().upsert_dictionary(id, original, replacement);
    let _ = app.emit("dictionary-changed", ());
    Ok(e)
}

#[tauri::command]
pub fn delete_dictionary(app: AppHandle, core: State<'_, Core>, id: String) {
    core.store.lock().delete_dictionary(&id);
    let _ = app.emit("dictionary-changed", ());
}

#[tauri::command]
pub fn list_suggestions(core: State<'_, Core>) -> Vec<Suggestion> {
    core.store.lock().open_suggestions()
}

#[tauri::command]
pub fn accept_suggestion(app: AppHandle, core: State<'_, Core>, id: String, replacement: Option<String>) {
    core.store.lock().accept_suggestion(&id, replacement);
    let _ = app.emit("dictionary-changed", ());
}

#[tauri::command]
pub fn dismiss_suggestion(app: AppHandle, core: State<'_, Core>, id: String) {
    core.store.lock().dismiss_suggestion(&id);
    let _ = app.emit("dictionary-changed", ());
}

#[tauri::command]
pub fn dismiss_all_suggestions(app: AppHandle, core: State<'_, Core>) {
    core.store.lock().dismiss_all_suggestions();
    let _ = app.emit("dictionary-changed", ());
}

#[tauri::command]
pub fn list_models(core: State<'_, Core>) -> Vec<ModelInfo> {
    core.model_dirs.read().list()
}

#[tauri::command]
pub fn download_model(app: AppHandle, core: State<'_, Core>, id: String) -> CmdResult<()> {
    let entry = catalog_entry(&id).ok_or("unknown model")?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut downloads = core.downloads.lock();
        if downloads.contains_key(&id) {
            return Ok(());
        }
        downloads.insert(id.clone(), cancel.clone());
    }
    let own = core.model_dirs.read().own.clone();
    tauri::async_runtime::spawn(async move {
        let progress_app = app.clone();
        let res = transcribe::download(entry, &own, cancel, |done, total| {
            let _ = progress_app.emit("model-download-progress", json!({ "id": entry.id, "downloaded": done, "total": total }));
        })
        .await;
        app.state::<Core>().downloads.lock().remove(entry.id);
        match res {
            Ok(_) => {
                session::notice(&app, "success", "Model ready", &format!("{} downloaded.", entry.display_name));
                if app.state::<Core>().settings.read().model == entry.id {
                    crate::ensure_model_loaded(&app);
                }
            }
            Err(e) => session::notice(&app, "error", "Download failed", &format!("{}: {e:#}", entry.display_name)),
        }
        let _ = app.emit("models-changed", ());
    });
    Ok(())
}

#[tauri::command]
pub fn delete_model(app: AppHandle, core: State<'_, Core>, id: String) -> CmdResult<()> {
    let entry = catalog_entry(&id).ok_or("unknown model")?;
    if core.settings.read().model == id {
        return Err("Switch to another model before deleting the active one.".into());
    }
    if let Some(cancel) = core.downloads.lock().get(&id) {
        cancel.store(true, Ordering::Relaxed);
    }
    let located = core.model_dirs.read().locate(entry);
    match located {
        Some((path, "app")) => std::fs::remove_file(path).map_err(err)?,
        Some(_) => return Err("This model lives in a folder you added; remove it there or remove the folder in Settings.".into()),
        None => {}
    }
    let _ = app.emit("models-changed", ());
    Ok(())
}

#[tauri::command]
pub fn set_model(app: AppHandle, core: State<'_, Core>, id: String) -> CmdResult<()> {
    let entry = catalog_entry(&id).ok_or("unknown model")?;
    if core.model_dirs.read().locate(entry).is_none() {
        return Err(format!("{} isn't downloaded yet.", entry.display_name));
    }
    update_settings(app, core, json!({ "model": id }))?;
    Ok(())
}

#[tauri::command]
pub fn start_recording(app: AppHandle) {
    thread::spawn(move || session::start(&app, RecMode::Click));
}

#[tauri::command]
pub fn stop_recording(app: AppHandle) {
    session::stop(&app);
}

#[tauri::command]
pub fn cancel_recording(app: AppHandle) {
    session::cancel(&app);
}

#[tauri::command]
pub fn toggle_recording(app: AppHandle) {
    thread::spawn(move || session::toggle(&app, RecMode::Click));
}

#[tauri::command]
pub fn list_input_devices() -> Vec<audio::AudioDevice> {
    audio::list_input_devices()
}

#[tauri::command]
pub fn check_permissions() -> macos::Permissions {
    macos::permissions()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionKind {
    Microphone,
    Accessibility,
    InputMonitoring,
}

#[tauri::command]
pub fn request_permission(kind: PermissionKind) {
    match kind {
        PermissionKind::Microphone => macos::request_microphone(),
        PermissionKind::Accessibility => {
            macos::request_accessibility();
            macos::open_privacy_pane("Privacy_Accessibility");
        }
        PermissionKind::InputMonitoring => macos::request_input_monitoring(),
    }
}

#[tauri::command]
pub fn set_fn_do_nothing() -> macos::FnUsage {
    macos::set_fn_do_nothing()
}

#[tauri::command]
pub fn copy_text(text: String) -> CmdResult<()> {
    macos::copy_text(&text).map_err(err)
}

#[tauri::command]
pub fn show_dashboard(app: AppHandle) {
    crate::show_dashboard(&app);
}

/// Imports history, dictionary and suggestions from a folder of JSON exports, and adds
/// the folder (or its `models/` subfolder) to the model folders if it holds Whisper models.
#[tauri::command]
pub fn import_data(app: AppHandle, core: State<'_, Core>, path: String) -> CmdResult<ImportResult> {
    let dir = std::path::PathBuf::from(&path);
    if !dir.is_dir() {
        return Err("Choose a folder to import from.".into());
    }
    let mut r = core.store.lock().import_folder(&dir);
    if let Some(models) = transcribe::ModelDirs::find_model_folder(&dir) {
        let folder = models.display().to_string();
        let already = core.settings.read().model_folders.contains(&folder);
        if !already {
            let mut folders = core.settings.read().model_folders.clone();
            folders.push(folder.clone());
            update_settings(app.clone(), core.clone(), json!({ "modelFolders": folders }))?;
        }
        r.model_folder = Some(folder);
    }
    if r.history + r.dictionary + r.suggestions == 0 && r.model_folder.is_none() {
        return Err("Nothing importable was found in that folder.".into());
    }
    let _ = app.emit("history-changed", ());
    let _ = app.emit("dictionary-changed", ());
    let _ = app.emit("models-changed", ());
    Ok(r)
}

#[tauri::command]
pub fn resize_widget(app: AppHandle, width: f64, height: f64) {
    widget::resize(&app, width, height);
}

/// Emits `permissions-changed` whenever a permission flips (e.g. granted in System Settings).
pub fn spawn_permission_watch(app: AppHandle) {
    thread::spawn(move || {
        let mut last = macos::permissions();
        loop {
            thread::sleep(Duration::from_secs(2));
            let now = macos::permissions();
            if now != last {
                log::info!("permissions changed: {now:?}");
                let _ = app.emit("permissions-changed", &now);
                last = now;
            }
        }
    });
}
