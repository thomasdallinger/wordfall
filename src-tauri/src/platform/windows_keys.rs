//! Push-to-talk key listener for Windows: a low-level keyboard hook (listen-only; every
//! event is passed on) on a thread with its own message loop.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    thread,
    time::Duration,
};

use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_ESCAPE, VK_LCONTROL, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN},
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
            HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
        },
    },
};

use crate::{hotkey::KeyEvents, settings::PttKey};

static EVENTS: OnceLock<&'static KeyEvents> = OnceLock::new();

/// The Fn key never reaches Windows, so it has no mapping here.
fn ptt_vk(key: PttKey) -> Option<VIRTUAL_KEY> {
    match key {
        PttKey::RightControl => Some(VK_RCONTROL),
        PttKey::RightOption => Some(VK_RMENU),
        PttKey::RightShift => Some(VK_RSHIFT),
        PttKey::RightCommand => Some(VK_RWIN),
        PttKey::Fn | PttKey::None => None,
    }
}

/// AltGr layouts (German, …) send a fake Left Ctrl with this scan code alongside Right Alt.
const ALTGR_FAKE_CTRL_SCAN: u32 = 0x21D;
/// An unassigned virtual key; tapping it stops a lone Alt/Win press from opening a menu or Start.
const VK_UNASSIGNED: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(events) = EVENTS.get() {
            let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if info.flags.0 & LLKHF_INJECTED.0 == 0 {
                let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                on_key(events, VIRTUAL_KEY(info.vkCode as u16), info.scanCode, down);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn on_key(events: &KeyEvents, vk: VIRTUAL_KEY, scan: u32, down: bool) {
    let ptt = ptt_vk(events.key());
    if Some(vk) == ptt {
        if down && matches!(vk, VK_RMENU | VK_RWIN) {
            let _ = super::windows::send_keys(&[(VK_UNASSIGNED, false), (VK_UNASSIGNED, true)]);
        }
        events.ptt(down);
    } else if vk == VK_LCONTROL && scan == ALTGR_FAKE_CTRL_SCAN {
        // Part of Right Alt on AltGr layouts, not a separate key.
    } else if down && vk == VK_ESCAPE {
        events.escape();
    } else if down {
        events.other_key();
    }
}

/// Installs the hook and pumps its messages forever (reinstalling if it ever stops).
pub fn run_key_listener(events: &'static KeyEvents, installed: Arc<AtomicBool>) {
    let _ = EVENTS.set(events);
    loop {
        let module = unsafe { GetModuleHandleW(None) }.ok().map(|m| HINSTANCE(m.0));
        let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) } {
            Ok(h) => h,
            Err(e) => {
                log::warn!("could not install keyboard hook: {e}");
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        installed.store(true, Ordering::Relaxed);
        log::info!("push-to-talk listener installed");
        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        installed.store(false, Ordering::Relaxed);
        let _ = unsafe { UnhookWindowsHookEx(hook) };
        log::warn!("keyboard hook message loop exited; reinstalling");
        thread::sleep(Duration::from_secs(1));
    }
}
