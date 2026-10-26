//! Push-to-talk key listener for macOS: a listen-only CGEventTap on its own run loop.

use core_foundation::{
    base::TCFType,
    mach_port::{CFMachPort, CFMachPortRef},
    runloop::{kCFRunLoopCommonModes, CFRunLoop},
};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use crate::{hotkey::KeyEvents, settings::PttKey};

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type TapCallback = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: TapCallback, info: *mut c_void) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGPreflightListenEventAccess() -> bool;
}

const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const LISTEN_ONLY: u32 = 1;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_INPUT: u32 = 0xFFFF_FFFF;
const FIELD_KEYCODE: u32 = 9;
const FIELD_AUTOREPEAT: u32 = 8;
const KEYCODE_ESCAPE: i64 = 53;

/// (keycodes, flag mask that is set while the key is down)
fn key_spec(key: PttKey) -> Option<(&'static [i64], u64)> {
    match key {
        PttKey::Fn => Some((&[63, 179], 0x0080_0000)),
        PttKey::RightOption => Some((&[61], 0x0000_0040)),
        PttKey::RightCommand => Some((&[54], 0x0000_0010)),
        PttKey::RightControl => Some((&[62], 0x0000_2000)),
        PttKey::RightShift => Some((&[60], 0x0000_0004)),
        PttKey::None => None,
    }
}

struct TapContext {
    events: &'static KeyEvents,
    port: AtomicPtr<c_void>,
}

extern "C" fn tap_callback(_proxy: CGEventTapProxy, etype: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = unsafe { &*(info as *const TapContext) };
    match etype {
        TAP_DISABLED_TIMEOUT | TAP_DISABLED_INPUT => {
            let port = ctx.port.load(Ordering::Relaxed);
            if !port.is_null() {
                unsafe { CGEventTapEnable(port as CFMachPortRef, true) };
            }
        }
        FLAGS_CHANGED => {
            let Some((codes, mask)) = key_spec(ctx.events.key()) else { return event };
            let code = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
            if codes.contains(&code) {
                let flags = unsafe { CGEventGetFlags(event) };
                ctx.events.ptt(flags & mask != 0);
            } else {
                ctx.events.other_key();
            }
        }
        KEY_DOWN => {
            let code = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
            let repeat = unsafe { CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) } != 0;
            if code == KEYCODE_ESCAPE {
                ctx.events.escape();
            } else if !repeat {
                ctx.events.other_key();
            }
        }
        _ => {}
    }
    event
}

/// Installs the tap (retrying until Input Monitoring is granted) and runs it forever.
pub fn run_key_listener(events: &'static KeyEvents, installed: Arc<AtomicBool>) {
    let ctx: &'static TapContext = Box::leak(Box::new(TapContext { events, port: AtomicPtr::new(std::ptr::null_mut()) }));
    let info = ctx as *const TapContext as *mut c_void;
    let mask = (1u64 << KEY_DOWN) | (1u64 << KEY_UP) | (1u64 << FLAGS_CHANGED);
    loop {
        // Without Input Monitoring the tap is created but silently receives nothing,
        // so wait for the grant (the onboarding asks for it) before installing.
        if !unsafe { CGPreflightListenEventAccess() } {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        let port = unsafe { CGEventTapCreate(SESSION_TAP, HEAD_INSERT, LISTEN_ONLY, mask, tap_callback, info) };
        if port.is_null() {
            // Input Monitoring not granted yet; try again shortly.
            thread::sleep(Duration::from_secs(2));
            continue;
        }
        ctx.port.store(port as *mut c_void, Ordering::Relaxed);
        let mach = unsafe { CFMachPort::wrap_under_create_rule(port) };
        let source = mach.create_runloop_source(0).expect("run loop source");
        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });
        unsafe { CGEventTapEnable(port, true) };
        installed.store(true, Ordering::Relaxed);
        log::info!("push-to-talk listener installed");
        CFRunLoop::run_current();
        installed.store(false, Ordering::Relaxed);
        log::warn!("event tap run loop exited; reinstalling");
        thread::sleep(Duration::from_secs(1));
    }
}
