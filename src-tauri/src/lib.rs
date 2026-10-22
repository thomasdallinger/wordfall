mod audio;
mod bench;
mod bench_report;
mod commands;
mod hotkey;
mod lab;
mod macos;
mod metrics;
mod postprocess;
mod session;
mod settings;
mod storage;
mod store;
mod transcribe;
mod vad;
mod vocab;
mod widget;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc, OnceLock},
    thread,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, Manager, RunEvent, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use session::{RecMode, Sessions};
use settings::Settings;
use store::Store;
use transcribe::{catalog_entry, Engine, ModelDirs};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    Unloaded,
    Loading,
    Ready,
    Error,
}

pub struct Core {
    pub dir: PathBuf,
    pub settings: RwLock<Settings>,
    pub store: Mutex<Store>,
    pub engine: Arc<Engine>,
    pub model_state: Mutex<(ModelState, Option<String>)>,
    pub model_dirs: RwLock<ModelDirs>,
    pub sessions: Sessions,
    pub listener: OnceLock<hotkey::Listener>,
    pub downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub widget_programmatic_move: Mutex<bool>,
    pub widget_size: Mutex<(f64, f64)>,
    /// Project folder names used as vocabulary hints (scanned at startup).
    pub projects: Vec<String>,
    /// In-progress Voice Lab recording (prompt id, recording).
    pub lab: Mutex<Option<(String, audio::Recording)>>,
}

const DASHBOARD: &str = "dashboard";

pub fn show_dashboard(app: &AppHandle) {
    let _ = app.set_activation_policy(ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window(DASHBOARD) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn set_model_state(app: &AppHandle, state: ModelState, model: Option<String>, error: Option<String>) {
    *app.state::<Core>().model_state.lock() = (state, error.clone());
    let _ = app.emit("model-state", json!({ "state": state, "model": model, "error": error }));
}

/// Loads the configured model in the background if it isn't loaded or loading.
/// Returns false when the model isn't on disk.
pub fn ensure_model_loaded(app: &AppHandle) -> bool {
    let core = app.state::<Core>();
    let wanted = core.settings.read().model.clone();
    if core.engine.loaded_model().as_deref() == Some(wanted.as_str()) {
        return true;
    }
    let Some(entry) = catalog_entry(&wanted) else { return false };
    let Some((path, _)) = core.model_dirs.read().locate(entry) else {
        set_model_state(app, ModelState::Unloaded, Some(wanted), None);
        return false;
    };
    {
        let mut st = core.model_state.lock();
        if st.0 == ModelState::Loading {
            return true;
        }
        *st = (ModelState::Loading, None);
    }
    let _ = app.emit("model-state", json!({ "state": ModelState::Loading, "model": wanted, "error": null }));
    let (app, engine) = (app.clone(), core.engine.clone());
    thread::spawn(move || {
        if engine.loaded_model().is_some() {
            engine.unload();
        }
        match engine.load(entry, &path) {
            Ok(()) => set_model_state(&app, ModelState::Ready, Some(entry.id.into()), None),
            Err(e) => {
                log::error!("{e:#}");
                set_model_state(&app, ModelState::Error, Some(entry.id.into()), Some(format!("{e:#}")));
            }
        }
        // The user may have switched models while we were loading.
        let now_wanted = app.state::<Core>().settings.read().model.clone();
        if now_wanted != entry.id {
            ensure_model_loaded(&app);
        }
    });
    true
}

pub fn register_shortcuts(app: &AppHandle) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let s = app.state::<Core>().settings.read().clone();
    if let Err(e) = gs.register(s.show_widget_shortcut.as_str()) {
        log::warn!("could not register {}: {e}", s.show_widget_shortcut);
    }
    if let Some(t) = s.toggle_shortcut.as_deref().filter(|t| !t.is_empty()) {
        if let Err(e) = gs.register(t) {
            log::warn!("could not register toggle shortcut {t}: {e}");
            session::notice(app, "warning", "Shortcut unavailable", &format!("{t} is taken by another app."));
        }
    }
}

fn on_shortcut(app: &AppHandle, shortcut: &tauri_plugin_global_shortcut::Shortcut, state: ShortcutState) {
    if state != ShortcutState::Pressed {
        return;
    }
    let s = app.state::<Core>().settings.read().clone();
    let matches = |acc: &str| acc.parse::<tauri_plugin_global_shortcut::Shortcut>().ok().as_ref() == Some(shortcut);
    if matches(&s.show_widget_shortcut) {
        widget::reveal(app);
    } else if s.toggle_shortcut.as_deref().is_some_and(matches) {
        session::toggle(app, RecMode::Toggle);
    }
}

pub fn apply_autostart(app: &AppHandle) {
    let enabled = app.state::<Core>().settings.read().launch_at_login;
    let al = app.autolaunch();
    let current = al.is_enabled().unwrap_or(false);
    let res = match (enabled, current) {
        (true, false) => al.enable(),
        (false, true) => al.disable(),
        _ => Ok(()),
    };
    if let Err(e) = res {
        log::warn!("autostart update failed: {e}");
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Wordfall", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start / Stop Dictation", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Wordfall", true, Some("Cmd+Q"))?;
    let menu = Menu::with_items(app, &[&open, &toggle, &PredefinedMenuItem::separator(app)?, &quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("wordfall")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Wordfall")
        .menu(&menu)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "open" => show_dashboard(app),
            "toggle" => session::toggle(app, RecMode::Click),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn init_logging(dir: &std::path::Path) {
    struct FileLog(Mutex<std::fs::File>);
    impl log::Log for FileLog {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info && m.target().starts_with("wordfall")
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                use std::io::Write;
                let line = format!("{} {:<5} {}\n", chrono::Local::now().format("%F %T%.3f"), r.level(), r.args());
                eprint!("{line}");
                let _ = self.0.lock().write_all(line.as_bytes());
            }
        }
        fn flush(&self) {}
    }
    let _ = std::fs::create_dir_all(dir);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("wordfall.log")) {
        let _ = log::set_boxed_logger(Box::new(FileLog(Mutex::new(f))));
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// `wordfall --transcribe <file.wav> [--model <id>]`: headless pipeline check that prints JSON.
fn cli_transcribe(args: &[String]) -> anyhow::Result<()> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let wav = arg("--transcribe").ok_or_else(|| anyhow::anyhow!("missing WAV path"))?;
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?.join("com.wordfall.app");
    let settings = Settings::load(&data_dir);
    let model_id = arg("--model").unwrap_or(settings.model.clone());
    let entry = catalog_entry(&model_id).ok_or_else(|| anyhow::anyhow!("unknown model {model_id}"))?;
    let dirs = settings.model_dirs(&data_dir);
    let (path, source) = dirs.locate(entry).ok_or_else(|| anyhow::anyhow!("{model_id} is not downloaded"))?;

    let audio = audio::read_wav_16k(std::path::Path::new(&wav))?;
    let engine = Engine::default();
    let load_started = std::time::Instant::now();
    engine.load(entry, &path)?;
    let load_ms = load_started.elapsed().as_millis();
    let dictionary = Store::load(&data_dir).dictionary;
    let t = engine.transcribe(&audio, settings.language.as_deref(), None, &settings.decode)?;
    let cleaned = postprocess::clean(&t.text, settings.remove_fillers, Some(&dictionary), false);
    println!(
        "{}",
        json!({ "model": model_id, "source": source, "loadMs": load_ms, "transcribeMs": t.elapsed_ms,
                "audioSeconds": audio.len() as f32 / 16_000.0, "raw": t.text, "text": cleaned.text })
    );
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("bench") {
        if let Err(e) = bench::run_cli(&args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--transcribe") {
        if let Err(e) = cli_transcribe(&args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    let autostarted = args.iter().any(|a| a == "--autostart");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_dashboard(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--autostart"])))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| on_shortcut(app, shortcut, event.state()))
                .build(),
        )
        .setup(move |app| {
            let handle = app.handle().clone();
            let dir = app.path().app_data_dir()?;
            init_logging(&dir);
            log::info!("Wordfall {} starting (autostart: {autostarted})", app.package_info().version);
            log::info!("permissions: {:?}, fn key: {:?}", macos::permissions(), macos::fn_usage());

            let settings = Settings::load(&dir);
            let store = Store::load(&dir);
            let model_dirs = settings.model_dirs(&dir);
            app.manage(Core {
                dir: dir.clone(),
                settings: RwLock::new(settings.clone()),
                store: Mutex::new(store),
                engine: Arc::new(Engine::default()),
                model_state: Mutex::new((ModelState::Unloaded, None)),
                model_dirs: RwLock::new(model_dirs),
                sessions: Sessions::default(),
                listener: OnceLock::new(),
                downloads: Mutex::new(HashMap::new()),
                widget_programmatic_move: Mutex::new(false),
                widget_size: Mutex::new((140.0, 40.0)),
                projects: dirs::home_dir().map(|h| vocab::project_names(&h)).unwrap_or_default(),
                lab: Mutex::new(None),
            });
            let silero_app = handle.clone();
            tauri::async_runtime::spawn(async move {
                let dir = silero_app.state::<Core>().model_dirs.read().own.clone();
                match transcribe::ensure_silero(&dir).await {
                    Ok(path) => *silero_app.state::<Core>().engine.silero_path.lock() = Some(path),
                    Err(e) => log::warn!("Silero VAD unavailable, using energy gate: {e:#}"),
                }
            });

            // Push-to-talk listener.
            let (h1, h2) = (handle.clone(), handle.clone());
            let listener = hotkey::spawn(
                settings.ptt_key,
                move || h1.state::<Core>().sessions.is_active(),
                move |action| match action {
                    hotkey::Action::StartHold => session::start(&h2, RecMode::Ptt),
                    hotkey::Action::Latch => {
                        session::start(&h2, RecMode::Latched);
                        session::latch(&h2);
                    }
                    hotkey::Action::Stop => session::stop(&h2),
                    hotkey::Action::Cancel | hotkey::Action::Escape => session::cancel(&h2),
                },
            );
            let _ = app.state::<Core>().listener.set(listener);

            macos::spawn_focus_tracker();
            widget::create(&handle)?;
            widget::spawn_follow_cursor(handle.clone());
            session::widget_visibility_changed(&handle, settings.widget_visibility);

            build_tray(&handle)?;
            register_shortcuts(&handle);
            apply_autostart(&handle);
            ensure_model_loaded(&handle);
            commands::spawn_permission_watch(handle.clone());

            if autostarted && settings.onboarding_done {
                let _ = handle.set_activation_policy(ActivationPolicy::Accessory);
            } else {
                show_dashboard(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == DASHBOARD {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    let _ = window.app_handle().set_activation_policy(ActivationPolicy::Accessory);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::update_settings,
            commands::get_history,
            commands::delete_history,
            commands::clear_history,
            commands::export_history_csv,
            commands::get_stats,
            commands::list_dictionary,
            commands::upsert_dictionary,
            commands::delete_dictionary,
            commands::list_suggestions,
            commands::accept_suggestion,
            commands::dismiss_suggestion,
            commands::dismiss_all_suggestions,
            commands::list_models,
            commands::download_model,
            commands::delete_model,
            commands::set_model,
            commands::start_recording,
            commands::stop_recording,
            commands::cancel_recording,
            commands::toggle_recording,
            commands::list_input_devices,
            commands::check_permissions,
            commands::request_permission,
            commands::set_fn_do_nothing,
            commands::copy_text,
            commands::show_dashboard,
            commands::import_data,
            commands::resize_widget,
            commands::correct_history,
            commands::verify_history,
            commands::list_vocabulary,
            commands::set_vocabulary,
            commands::suggested_vocabulary,
            commands::lab_prompts,
            commands::lab_start,
            commands::lab_stop,
            commands::lab_cancel,
            commands::lab_delete,
            commands::bench_info,
            commands::open_path,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Wordfall");

    app.run(|app, event| {
        if let RunEvent::Reopen { .. } = event {
            show_dashboard(app);
        }
    });
}
