//! macOS integrations: permissions, paste injection, focus restore, Fn/Globe setting,
//! recording cues and system-output muting.

use std::{
    path::Path,
    process::Command,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::Duration,
};

use core_foundation::{base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, string::CFString};
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

use super::{FnUsage, Permissions};

/// Identifies the app to paste into: a process id.
pub type AppRef = i32;

pub fn permissions() -> Permissions {
    Permissions {
        microphone: microphone_status(),
        accessibility: unsafe { AXIsProcessTrusted() },
        input_monitoring: unsafe { CGPreflightListenEventAccess() },
    }
}

fn microphone_status() -> &'static str {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return "undetermined" };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
    match status {
        AVAuthorizationStatus::Authorized => "granted",
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => "denied",
        _ => "undetermined",
    }
}

/// Shows the system microphone prompt (first time) or opens System Settings (if denied).
pub fn request_microphone() {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    match microphone_status() {
        "granted" => {}
        "denied" => open_privacy_pane("Privacy_Microphone"),
        _ => {
            let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return };
            let handler = block2::RcBlock::new(|granted: objc2::runtime::Bool| {
                log::info!("microphone prompt answered: granted={}", granted.as_bool());
            });
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler) };
        }
    }
}

pub fn request_accessibility() {
    let key = CFString::new("AXTrustedCheckOptionPrompt");
    let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
    unsafe { AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef()) };
    open_privacy_pane("Privacy_Accessibility");
}

pub fn request_input_monitoring() {
    if !unsafe { CGRequestListenEventAccess() } {
        open_privacy_pane("Privacy_ListenEvent");
    }
}

fn open_privacy_pane(anchor: &str) {
    let _ = Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}"))
        .spawn();
}

// ---------- focus tracking ----------

static LAST_FOREIGN_PID: AtomicI32 = AtomicI32::new(0);

/// The frontmost app, unless it's Wordfall itself.
pub fn frontmost_foreign_app() -> Option<AppRef> {
    let ws = NSWorkspace::sharedWorkspace();
    let app = ws.frontmostApplication()?;
    let pid = app.processIdentifier();
    (pid != std::process::id() as i32).then_some(pid)
}

/// The app the user was last working in (clicking the widget can activate Wordfall).
pub fn last_foreign_app() -> Option<AppRef> {
    frontmost_foreign_app().or_else(|| Some(LAST_FOREIGN_PID.load(Ordering::Relaxed)).filter(|p| *p > 0))
}

pub fn spawn_focus_tracker() {
    thread::spawn(|| loop {
        if let Some(pid) = frontmost_foreign_app() {
            LAST_FOREIGN_PID.store(pid, Ordering::Relaxed);
        }
        thread::sleep(Duration::from_millis(300));
    });
}

fn wordfall_is_frontmost() -> bool {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|a| a.processIdentifier() == std::process::id() as i32)
        .unwrap_or(false)
}

/// If Wordfall stole focus (e.g. the widget was clicked), hand it back to `pid`.
pub fn restore_focus(pid: Option<AppRef>) {
    let Some(pid) = pid else { return };
    if !wordfall_is_frontmost() {
        return;
    }
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        #[allow(deprecated)]
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        thread::sleep(Duration::from_millis(180));
    }
}

// ---------- injection ----------

const KEY_V: u16 = 9;

pub fn post_paste() -> anyhow::Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| anyhow::anyhow!("could not create event source"))?;
    let down = CGEvent::new_keyboard_event(source.clone(), KEY_V, true)
        .map_err(|_| anyhow::anyhow!("could not create key event"))?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    let up = CGEvent::new_keyboard_event(source, KEY_V, false)
        .map_err(|_| anyhow::anyhow!("could not create key event"))?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);
    thread::sleep(Duration::from_millis(12));
    up.post(CGEventTapLocation::HID);
    Ok(())
}

pub fn ensure_can_paste() -> anyhow::Result<()> {
    // Synthetic ⌘V is silently dropped without Accessibility.
    anyhow::ensure!(unsafe { AXIsProcessTrusted() }, "Wordfall needs Accessibility permission to paste");
    Ok(())
}

/// Opens a file or folder with its default app.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    Command::new("open").arg(path).spawn().map(drop)
}

// ---------- Fn / Globe key system setting ----------

pub fn fn_usage() -> FnUsage {
    let out = Command::new("defaults").args(["read", "com.apple.HIToolbox", "AppleFnUsageType"]).output();
    let value = out.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i64>().ok());
    // macOS default when unset is "Show Emoji & Symbols" (2) on recent versions.
    let effective = value.unwrap_or(2);
    let label = match effective {
        0 => "Do Nothing",
        1 => "Change Input Source",
        2 => "Show Emoji & Symbols",
        3 => "Start Dictation",
        _ => "Unknown",
    };
    FnUsage { value, label: label.into(), ok: effective == 0 }
}

pub fn set_fn_do_nothing() -> FnUsage {
    let _ = Command::new("defaults")
        .args(["write", "com.apple.HIToolbox", "AppleFnUsageType", "-int", "0"])
        .status();
    let _ = Command::new("/System/Library/PrivateFrameworks/SystemAdministration.framework/Resources/activateSettings")
        .arg("-u")
        .status();
    fn_usage()
}

// ---------- cues & muting ----------

pub fn play_cue(start: bool) {
    let sound = if start { "/System/Library/Sounds/Tink.aiff" } else { "/System/Library/Sounds/Pop.aiff" };
    let _ = Command::new("afplay").args(["-v", "0.35", sound]).spawn();
}

fn osascript(script: &str) -> Option<String> {
    let out = Command::new("osascript").args(["-e", script]).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Mutes system output; returns whether it was already muted (to restore later).
pub fn mute_output() -> bool {
    let was_muted = osascript("output muted of (get volume settings)").as_deref() == Some("true");
    if !was_muted {
        osascript("set volume output muted true");
    }
    was_muted
}

pub fn unmute_output(was_muted: bool) {
    if !was_muted {
        thread::spawn(|| {
            osascript("set volume output muted false");
        });
    }
}
