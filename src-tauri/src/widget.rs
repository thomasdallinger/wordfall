//! The floating pill window: creation, bottom-center placement, follow-cursor across
//! monitors, remembered drag offset, and visibility rules.

use std::{thread, time::Duration};
use tauri::{
    AppHandle, Manager, Monitor, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use crate::{session::RecState, settings::WidgetVisibility, Core};

pub const LABEL: &str = "widget";
/// Initial size; the UI reports its real content size via `resize_widget`.
const WIDTH: f64 = 140.0;
const HEIGHT: f64 = 40.0;
/// Gap between the pill and the bottom of the work area (logical px).
const BOTTOM_MARGIN: f64 = 14.0;

pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Wordfall Widget")
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

    // Remember where the user dragged the pill, relative to their monitor's bottom-center.
    let handle = app.clone();
    win.on_window_event(move |e| {
        if let WindowEvent::Moved(pos) = e {
            remember_offset(&handle, *pos);
        }
    });
    Ok(win)
}

fn anchor(app: &AppHandle, monitor: &Monitor) -> PhysicalPosition<i32> {
    let wa = monitor.work_area();
    let scale = monitor.scale_factor();
    let (lw, lh) = *app.state::<Core>().widget_size.lock();
    let w = (lw * scale) as i32;
    let h = (lh * scale) as i32;
    PhysicalPosition::new(
        wa.position.x + (wa.size.width as i32 - w) / 2,
        wa.position.y + wa.size.height as i32 - h - (BOTTOM_MARGIN * scale) as i32,
    )
}

fn remember_offset(app: &AppHandle, pos: PhysicalPosition<i32>) {
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let Ok(Some(monitor)) = win.current_monitor() else { return };
    let core = app.state::<Core>();
    if *core.widget_programmatic_move.lock() {
        return;
    }
    let a = anchor(app, &monitor);
    let scale = monitor.scale_factor();
    let offset = (((pos.x - a.x) as f64 / scale) as i32, ((pos.y - a.y) as f64 / scale) as i32);
    let mut settings = core.settings.write();
    if settings.widget_position != Some(offset) {
        settings.widget_position = Some(offset);
        let _ = settings.save(&core.dir);
    }
}

fn place_on(app: &AppHandle, win: &WebviewWindow, monitor: &Monitor) {
    let core = app.state::<Core>();
    let (dx, dy) = core.settings.read().widget_position.unwrap_or((0, 0));
    let a = anchor(app, monitor);
    let scale = monitor.scale_factor();
    let target = PhysicalPosition::new(a.x + (dx as f64 * scale) as i32, a.y + (dy as f64 * scale) as i32);
    if win.outer_position().ok() == Some(target) {
        return;
    }
    *core.widget_programmatic_move.lock() = true;
    let _ = win.set_position(target);
    let handle = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        *handle.state::<Core>().widget_programmatic_move.lock() = false;
    });
}

fn cursor_monitor(app: &AppHandle) -> Option<Monitor> {
    let p = app.cursor_position().ok()?;
    app.monitor_from_point(p.x, p.y).ok().flatten().or_else(|| app.primary_monitor().ok().flatten())
}

pub fn position(app: &AppHandle) {
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let monitor = cursor_monitor(app).or_else(|| win.current_monitor().ok().flatten());
    if let Some(m) = monitor {
        place_on(app, &win, &m);
    }
}

/// Polls the cursor and moves the pill to whichever monitor it's on.
pub fn spawn_follow_cursor(app: AppHandle) {
    thread::spawn(move || {
        let mut last: Option<String> = None;
        loop {
            thread::sleep(Duration::from_millis(400));
            let follow = app.state::<Core>().settings.read().follow_cursor;
            if !follow {
                continue;
            }
            let Some(m) = cursor_monitor(&app) else { continue };
            let key = format!("{:?}{:?}", m.position(), m.size());
            if last.as_deref() != Some(&key) {
                last = Some(key);
                let h = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(win) = h.get_webview_window(LABEL) {
                        place_on(&h, &win, &m);
                    }
                });
            }
        }
    });
}

pub fn apply_visibility(app: &AppHandle, vis: WidgetVisibility, state: RecState) {
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let show = match vis {
        WidgetVisibility::Always => true,
        WidgetVisibility::Recording => state != RecState::Idle,
        WidgetVisibility::Hidden => false,
    };
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        if show {
            if !win.is_visible().unwrap_or(false) {
                position(&h);
                let _ = win.show();
            }
        } else {
            let _ = win.hide();
        }
    });
}

pub fn on_recording_state(app: &AppHandle, state: RecState) {
    let vis = app.state::<Core>().settings.read().widget_visibility;
    apply_visibility(app, vis, state);
}

/// Resize to the UI's content, keeping the pill anchored at the bottom-center.
pub fn resize(app: &AppHandle, width: f64, height: f64) {
    let (width, height) = (width.clamp(24.0, 420.0), height.clamp(24.0, 420.0));
    {
        let core = app.state::<Core>();
        let mut size = core.widget_size.lock();
        if (size.0 - width).abs() < 0.5 && (size.1 - height).abs() < 0.5 {
            return;
        }
        *size = (width, height);
    }
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let Ok(Some(monitor)) = win.current_monitor() else { return };
    let _ = win.set_size(tauri::LogicalSize::new(width, height));
    place_on(app, &win, &monitor);
}

/// Global "show widget" shortcut: always reveal the pill, even when set to hidden.
pub fn reveal(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(LABEL) {
        position(app);
        let _ = win.show();
    }
}
