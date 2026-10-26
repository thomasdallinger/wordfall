//! Windows integrations: microphone privacy setting, paste injection, focus restore,
//! recording cues and system-output muting. Windows needs no Accessibility or Input
//! Monitoring grants, so those always report as allowed.

use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::atomic::{AtomicIsize, Ordering},
    thread,
    time::Duration,
};

use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{ERROR_SUCCESS, HWND},
        Media::Audio::{
            eConsole, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, PlaySoundW,
            SND_ASYNC, SND_MEMORY, SND_NODEFAULT,
        },
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
            Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
        },
        UI::{
            Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
                VK_CONTROL,
            },
            Shell::ShellExecuteW,
            WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow, SW_SHOWNORMAL},
        },
    },
};

use super::{FnUsage, Permissions};

/// Identifies the app to paste into: its top-level window handle.
pub type AppRef = isize;

pub fn permissions() -> Permissions {
    Permissions { microphone: microphone_status(), accessibility: true, input_monitoring: true }
}

const MIC_CONSENT: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// "denied" when microphone access is off for this user or for desktop apps
/// (Settings → Privacy & security → Microphone).
fn microphone_status() -> &'static str {
    let denied = [MIC_CONSENT.to_string(), format!(r"{MIC_CONSENT}\NonPackaged")]
        .iter()
        .any(|key| consent_value(key).as_deref() == Some("Deny"));
    if denied {
        "denied"
    } else {
        "granted"
    }
}

fn consent_value(subkey: &str) -> Option<String> {
    let key = wide(subkey);
    let mut buf = [0u16; 16];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            w!("Value"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    // `size` is in bytes and includes the terminating NUL.
    (status == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}

pub fn request_microphone() {
    if microphone_status() == "denied" {
        shell_open(&wide("ms-settings:privacy-microphone"));
    }
}

pub fn request_accessibility() {}

pub fn request_input_monitoring() {}

/// Opens a file or folder with its default app.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    let p: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if shell_open(&p) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("could not open {}", path.display())))
    }
}

fn shell_open(target: &[u16]) -> bool {
    // ShellExecute returns a value > 32 on success.
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(target.as_ptr()), None, None, SW_SHOWNORMAL) };
    r.0 as isize > 32
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

// ---------- focus tracking ----------

static LAST_FOREIGN: AtomicIsize = AtomicIsize::new(0);

fn foreground() -> Option<(HWND, u32)> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    Some((hwnd, pid))
}

/// The foreground window, unless it belongs to Wordfall.
pub fn frontmost_foreign_app() -> Option<AppRef> {
    let (hwnd, pid) = foreground()?;
    (pid != std::process::id()).then_some(hwnd.0 as isize)
}

/// The window the user was last working in (clicking the dashboard activates Wordfall).
pub fn last_foreign_app() -> Option<AppRef> {
    frontmost_foreign_app().or_else(|| Some(LAST_FOREIGN.load(Ordering::Relaxed)).filter(|h| *h != 0))
}

pub fn spawn_focus_tracker() {
    thread::spawn(|| loop {
        if let Some(h) = frontmost_foreign_app() {
            LAST_FOREIGN.store(h, Ordering::Relaxed);
        }
        thread::sleep(Duration::from_millis(300));
    });
}

/// If Wordfall has focus (e.g. the dashboard was clicked), hand it back to `target`.
pub fn restore_focus(target: Option<AppRef>) {
    let Some(target) = target else { return };
    if !foreground().is_some_and(|(_, pid)| pid == std::process::id()) {
        return;
    }
    // A foreground process may give the foreground away.
    if unsafe { SetForegroundWindow(HWND(target as *mut _)) }.as_bool() {
        thread::sleep(Duration::from_millis(150));
    }
}

// ---------- injection ----------

const KEY_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Sends a key press and release; injected input is ignored by our own key listener.
pub(super) fn send_keys(keys: &[(VIRTUAL_KEY, bool)]) -> anyhow::Result<()> {
    let inputs: Vec<INPUT> = keys.iter().map(|&(vk, up)| key_input(vk, up)).collect();
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    anyhow::ensure!(sent as usize == inputs.len(), "the paste keystroke was blocked");
    Ok(())
}

pub fn post_paste() -> anyhow::Result<()> {
    send_keys(&[(VK_CONTROL, false), (KEY_V, false), (KEY_V, true), (VK_CONTROL, true)])
}

pub fn ensure_can_paste() -> anyhow::Result<()> {
    Ok(())
}

// ---------- Fn key (macOS only) ----------

pub fn fn_usage() -> FnUsage {
    FnUsage { value: None, label: String::new(), ok: true }
}

pub fn set_fn_do_nothing() -> FnUsage {
    fn_usage()
}

// ---------- cues & muting ----------

pub fn play_cue(start: bool) {
    // SND_ASYNC keeps reading the buffer while playing; the cue lives for the whole process.
    let wav = crate::cues::wav(start);
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr().cast()), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

fn endpoint_volume() -> windows::core::Result<IAudioEndpointVolume> {
    unsafe {
        // Each call runs on a fresh thread, so join the multithreaded apartment there.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
    }
}

/// Mutes system output; returns whether it was already muted (to restore later).
pub fn mute_output() -> bool {
    thread::spawn(|| {
        let vol = endpoint_volume().ok()?;
        let was_muted = unsafe { vol.GetMute() }.ok()?.as_bool();
        if !was_muted {
            let _ = unsafe { vol.SetMute(true, std::ptr::null()) };
        }
        Some(was_muted)
    })
    .join()
    .ok()
    .flatten()
    // Unknown state: report "already muted" so we never unmute something we didn't mute.
    .unwrap_or(true)
}

pub fn unmute_output(was_muted: bool) {
    if !was_muted {
        thread::spawn(|| {
            if let Ok(vol) = endpoint_volume() {
                let _ = unsafe { vol.SetMute(false, std::ptr::null()) };
            }
        });
    }
}
