//! The floating Questions window: live questions of the meeting being recorded, above the call.
//! It never takes focus (the call keeps the keyboard) but takes clicks, sits at the top right of
//! the screen with the cursor, and remembers where the user dragged it.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};
use tauri::{AppHandle, Manager, Monitor, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

use crate::Core;

pub const LABEL: &str = "questions";
/// Logical size until the UI reports its content (`resize`).
const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 120.0;
/// Gap to the top-right corner of the work area (logical px).
const MARGIN: f64 = 16.0;

/// Set while Wordfall itself moves the window, so that isn't remembered as the user's choice.
static PLACING: AtomicBool = AtomicBool::new(false);

/// Shows the window (creating it if needed).
pub fn open(app: &AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        let win = match h.get_webview_window(LABEL) {
            Some(w) => w,
            None => match create(&h) {
                Ok(w) => w,
                Err(e) => {
                    log::warn!("the questions window did not open: {e}");
                    return;
                }
            },
        };
        place(&h, &win);
        let _ = win.show();
    });
}

/// Hides the window for now (the meeting view can show it again).
pub fn hide(app: &AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = h.get_webview_window(LABEL) {
            let _ = w.hide();
        }
    });
}

/// Closes the window when the meeting ends.
pub fn close(app: &AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = h.get_webview_window(LABEL) {
            let _ = w.destroy();
        }
    });
}

fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Wordfall Questions")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .focusable(false)
        .accept_first_mouse(true)
        .visible(false)
        .build()?;
    let handle = app.clone();
    win.on_window_event(move |e| {
        if let WindowEvent::Moved(pos) = e {
            remember(&handle, *pos);
        }
    });
    Ok(win)
}

/// Top-right spot on `monitor` for a window `width` physical px wide.
fn anchor(monitor: &Monitor, width: i32) -> PhysicalPosition<i32> {
    let wa = monitor.work_area();
    let margin = (MARGIN * monitor.scale_factor()) as i32;
    PhysicalPosition::new(wa.position.x + wa.size.width as i32 - width - margin, wa.position.y + margin)
}

fn monitor_of(app: &AppHandle, win: &WebviewWindow) -> Option<Monitor> {
    let cursor = app.cursor_position().ok().and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten());
    cursor.or_else(|| win.current_monitor().ok().flatten()).or_else(|| app.primary_monitor().ok().flatten())
}

fn place(app: &AppHandle, win: &WebviewWindow) {
    let Some(monitor) = monitor_of(app, win) else { return };
    let width = win.outer_size().map(|s| s.width as i32).unwrap_or((WIDTH * monitor.scale_factor()) as i32);
    let a = anchor(&monitor, width);
    let (dx, dy) = app.state::<Core>().settings.read().questions_window_position.unwrap_or((0, 0));
    let scale = monitor.scale_factor();
    let target = PhysicalPosition::new(a.x + (dx as f64 * scale) as i32, a.y + (dy as f64 * scale) as i32);
    PLACING.store(true, Ordering::SeqCst);
    let _ = win.set_position(target);
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(300));
        PLACING.store(false, Ordering::SeqCst);
    });
}

fn remember(app: &AppHandle, pos: PhysicalPosition<i32>) {
    if PLACING.load(Ordering::SeqCst) {
        return;
    }
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let Ok(Some(monitor)) = win.current_monitor() else { return };
    let width = win.outer_size().map(|s| s.width as i32).unwrap_or(0);
    let a = anchor(&monitor, width);
    let scale = monitor.scale_factor();
    let offset = (((pos.x - a.x) as f64 / scale) as i32, ((pos.y - a.y) as f64 / scale) as i32);
    let core = app.state::<Core>();
    let mut settings = core.settings.write();
    if settings.questions_window_position != Some(offset) {
        settings.questions_window_position = Some(offset);
        let _ = settings.save(&core.dir);
    }
}

/// Fits the window to the UI's content (logical px); its top-left corner stays put.
pub fn resize(app: &AppHandle, width: f64, height: f64) {
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let _ = win.set_size(tauri::LogicalSize::new(width.clamp(120.0, 480.0), height.clamp(40.0, 720.0)));
}
