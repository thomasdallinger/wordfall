//! Reading a shared screen on macOS: the windows that could show one, a picture of one window
//! (`screencapture -l`, which also works while the window is covered), and the Screen Recording
//! permission it needs. Without that permission macOS still hands out pictures, but of the
//! wallpaper only, and window titles are left out of the list.

use std::{path::Path, process::Command};

use core_foundation::{
    base::{CFType, TCFType},
    dictionary::{CFDictionary, CFDictionaryRef},
    number::CFNumber,
    string::CFString,
};
use core_graphics::window::{copy_window_info, kCGNullWindowID, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly};

use super::ScreenWindow;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Whether Wordfall may read other apps' windows (System Settings › Privacy & Security › Screen
/// & System Audio Recording). A grant takes effect after Wordfall is reopened.
pub fn screen_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Asks macOS for Screen Recording permission (a prompt the first time), or opens its settings.
pub fn request_screen_permission() {
    if !unsafe { CGRequestScreenCaptureAccess() } {
        let _ = Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture").spawn();
    }
}

/// Apps whose windows usually carry a shared screen, best first.
const MEETING_APPS: &[&str] = &["zoom.us", "Microsoft Teams", "Webex", "Cisco Webex Meetings", "Slack", "FaceTime", "Discord", "Skype", "Whereby", "GoTo Meeting", "Around"];
/// Browser windows with these words in their title are meetings too.
const MEETING_TITLES: &[&str] = &["Meet", "Teams", "Zoom", "Webex", "Jitsi", "Whereby"];
/// System furniture, never a shared screen.
const NOT_WINDOWS: &[&str] = &["Window Server", "Dock", "Control Center", "Notification Center", "SystemUIServer", "Spotlight", "WindowManager", "Wallpaper"];

fn text(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<String> {
    d.find(CFString::from_static_string(key)).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string())
}

fn number(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<i64> {
    d.find(CFString::from_static_string(key)).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_i64())
}

/// How likely a window shows a meeting's shared screen (lower is likelier).
fn rank(w: &ScreenWindow) -> usize {
    match MEETING_APPS.iter().position(|a| w.app.eq_ignore_ascii_case(a)) {
        Some(i) => i,
        None if MEETING_TITLES.iter().any(|t| w.title.contains(t)) => MEETING_APPS.len(),
        None => MEETING_APPS.len() + 1,
    }
}

/// Normal app windows on screen (not Wordfall's), meeting apps first.
pub fn screen_windows() -> Vec<ScreenWindow> {
    let Some(list) = copy_window_info(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID) else {
        return Vec::new();
    };
    let own = std::process::id() as i64;
    let mut out: Vec<ScreenWindow> = list
        .iter()
        .filter_map(|item| {
            let d: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(*item as CFDictionaryRef) };
            let layer = number(&d, "kCGWindowLayer").unwrap_or(1);
            let pid = number(&d, "kCGWindowOwnerPID").unwrap_or(0);
            let app = text(&d, "kCGWindowOwnerName").unwrap_or_default();
            let bounds = d.find(CFString::from_static_string("kCGWindowBounds")).and_then(|v| v.downcast::<CFDictionary>());
            let size = bounds.map(|b| {
                let b: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(b.as_concrete_TypeRef()) };
                (number(&b, "Width").unwrap_or(0), number(&b, "Height").unwrap_or(0))
            });
            let big_enough = size.is_some_and(|(w, h)| w >= 320 && h >= 200);
            (layer == 0 && pid != own && big_enough && !app.is_empty() && !NOT_WINDOWS.contains(&app.as_str())).then(|| ScreenWindow {
                window_id: number(&d, "kCGWindowNumber").unwrap_or(0) as u32,
                app,
                title: text(&d, "kCGWindowName").unwrap_or_default(),
            })
        })
        .collect();
    // Stable: windows of one app stay front to back.
    out.sort_by_key(rank);
    out
}

/// A picture of window `id` as a PNG at `to` (without its shadow, covered or not).
pub fn capture_window(id: u32, to: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(to);
    let out = Command::new("screencapture").args(["-x", "-o", "-t", "png", &format!("-l{id}")]).arg(to).output()?;
    let made = std::fs::metadata(to).map(|m| m.len() > 0).unwrap_or(false);
    anyhow::ensure!(out.status.success() && made, "the window is gone ({})", String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}
