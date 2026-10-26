//! OS integrations behind one interface: permissions, focus tracking, paste injection,
//! recording cues, output muting and the push-to-talk key listener.
//! `macos.rs` and `windows.rs` provide the same set of functions.

use serde::Serialize;
use std::{thread, time::Duration};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_keys;
#[cfg(target_os = "macos")]
pub use {macos::*, macos_keys::run_key_listener};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_keys;
#[cfg(windows)]
pub use {windows::*, windows_keys::run_key_listener};

#[cfg(not(any(target_os = "macos", windows)))]
compile_error!("Wordfall supports macOS and Windows.");

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub microphone: &'static str,
    pub accessibility: bool,
    pub input_monitoring: bool,
}

/// What the macOS Fn/Globe key does besides dictation (always `ok` elsewhere).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FnUsage {
    pub value: Option<i64>,
    pub label: String,
    pub ok: bool,
}

/// Puts `text` on the clipboard, sends the paste shortcut, then restores the previous clipboard text.
pub fn paste_text(text: &str) -> anyhow::Result<()> {
    // A synthetic paste can be silently dropped (macOS without Accessibility); fail so the
    // caller keeps the text on the clipboard instead of restoring over it.
    ensure_can_paste()?;
    let mut clipboard = arboard::Clipboard::new()?;
    let previous = clipboard.get_text().ok();
    clipboard.set_text(text.to_string())?;
    thread::sleep(Duration::from_millis(40));
    post_paste()?;
    let ours = text.to_string();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(600));
        if let (Ok(mut cb), Some(prev)) = (arboard::Clipboard::new(), previous) {
            // Only restore if nobody else changed the clipboard meanwhile.
            if cb.get_text().ok().as_deref() == Some(ours.as_str()) {
                let _ = cb.set_text(prev);
            }
        }
    });
    Ok(())
}

pub fn copy_text(text: &str) -> anyhow::Result<()> {
    arboard::Clipboard::new()?.set_text(text.to_string())?;
    Ok(())
}
