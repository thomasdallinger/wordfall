//! Push-to-talk listener: a listen-only CGEventTap on its own run loop thread feeds
//! key transitions into a small state machine (hold, tap, double-tap latch, combos).

use core_foundation::{
    base::TCFType,
    mach_port::{CFMachPort, CFMachPortRef},
    runloop::{kCFRunLoopCommonModes, CFRunLoop},
};
use parking_lot::Mutex;
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::settings::PttKey;

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

const TAP_MAX: Duration = Duration::from_millis(250);
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(400);
const COMBO_WINDOW: Duration = Duration::from_millis(1000);

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

#[derive(Debug)]
enum Input {
    PttDown,
    PttUp,
    OtherKey,
    Escape,
}

/// What the state machine asks the session controller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    StartHold,
    Latch,
    Stop,
    Cancel,
    Escape,
}

struct TapContext {
    tx: Sender<Input>,
    key: Arc<Mutex<PttKey>>,
    held: AtomicBool,
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
            let Some((codes, mask)) = key_spec(*ctx.key.lock()) else { return event };
            let code = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
            let flags = unsafe { CGEventGetFlags(event) };
            if codes.contains(&code) {
                let down = flags & mask != 0;
                let was = ctx.held.swap(down, Ordering::Relaxed);
                if down != was {
                    let _ = ctx.tx.send(if down { Input::PttDown } else { Input::PttUp });
                }
            } else if ctx.held.load(Ordering::Relaxed) {
                let _ = ctx.tx.send(Input::OtherKey);
            }
        }
        KEY_DOWN => {
            let code = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
            let repeat = unsafe { CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) } != 0;
            if code == KEYCODE_ESCAPE {
                let _ = ctx.tx.send(Input::Escape);
            } else if !repeat && ctx.held.load(Ordering::Relaxed) {
                let _ = ctx.tx.send(Input::OtherKey);
            }
        }
        _ => {}
    }
    event
}

pub struct Listener {
    pub key: Arc<Mutex<PttKey>>,
    pub installed: Arc<AtomicBool>,
}

impl Listener {
    pub fn set_key(&self, key: PttKey) {
        *self.key.lock() = key;
    }
}

/// Starts the tap thread (retrying until Input Monitoring is granted) and the state
/// machine thread. `is_active` reports whether a recording session is running (so the
/// machine resyncs after the widget or a shortcut ended it); `on_action` runs on the
/// state machine thread.
pub fn spawn(
    initial: PttKey,
    is_active: impl Fn() -> bool + Send + 'static,
    on_action: impl Fn(Action) + Send + 'static,
) -> Listener {
    let key = Arc::new(Mutex::new(initial));
    let installed = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let ctx = Box::leak(Box::new(TapContext {
        tx,
        key: key.clone(),
        held: AtomicBool::new(false),
        port: AtomicPtr::new(std::ptr::null_mut()),
    }));
    let ctx_addr = ctx as *const TapContext as usize;
    let inst = installed.clone();
    thread::Builder::new()
        .name("wordfall-event-tap".into())
        .spawn(move || run_tap(ctx_addr, inst))
        .expect("spawn event tap thread");

    thread::Builder::new()
        .name("wordfall-ptt".into())
        .spawn(move || state_machine(rx, is_active, on_action))
        .expect("spawn ptt thread");

    Listener { key, installed }
}

fn run_tap(ctx_addr: usize, installed: Arc<AtomicBool>) {
    let ctx = unsafe { &*(ctx_addr as *const TapContext) };
    let mask = (1u64 << KEY_DOWN) | (1u64 << KEY_UP) | (1u64 << FLAGS_CHANGED);
    loop {
        // Without Input Monitoring the tap is created but silently receives nothing,
        // so wait for the grant (the onboarding asks for it) before installing.
        if !unsafe { CGPreflightListenEventAccess() } {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        let port = unsafe { CGEventTapCreate(SESSION_TAP, HEAD_INSERT, LISTEN_ONLY, mask, tap_callback, ctx_addr as *mut c_void) };
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// Key is down and we are recording; stop on release (unless it was a quick tap).
    Holding { since: Instant, latch: bool },
    /// Hands-free recording after a double tap; the next press (released) stops it.
    Latched,
    /// Key pressed while latched; stop fires on release.
    StopOnRelease,
}

fn state_machine(rx: Receiver<Input>, is_active: impl Fn() -> bool, on_action: impl Fn(Action)) {
    let mut phase = Phase::Idle;
    let mut last_tap_up: Option<Instant> = None;
    loop {
        let input = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(i) => i,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let now = Instant::now();
        if matches!(phase, Phase::Latched) && !is_active() {
            phase = Phase::Idle;
        }
        phase = match (phase, input) {
            (Phase::Idle, Input::PttDown) => {
                let double = last_tap_up.is_some_and(|t| now.duration_since(t) <= DOUBLE_TAP_WINDOW);
                last_tap_up = None;
                on_action(if double { Action::Latch } else { Action::StartHold });
                Phase::Holding { since: now, latch: double }
            }
            (Phase::Holding { latch: true, .. }, Input::PttUp) => Phase::Latched,
            (Phase::Holding { since, latch: false }, Input::PttUp) => {
                if now.duration_since(since) < TAP_MAX {
                    on_action(Action::Cancel);
                    last_tap_up = Some(now);
                } else {
                    on_action(Action::Stop);
                }
                Phase::Idle
            }
            (Phase::Holding { since, .. }, Input::OtherKey) if now.duration_since(since) < COMBO_WINDOW => {
                // Fn+key shortcut (brightness, arrows, …), not dictation.
                on_action(Action::Cancel);
                Phase::Idle
            }
            (Phase::Latched, Input::PttDown) => Phase::StopOnRelease,
            (Phase::StopOnRelease, Input::PttUp) => {
                on_action(Action::Stop);
                Phase::Idle
            }
            (_, Input::Escape) => {
                on_action(Action::Escape);
                Phase::Idle
            }
            (p, _) => p,
        };
    }
}
