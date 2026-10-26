//! Push-to-talk listener: a platform key listener (see `platform::run_key_listener`) feeds
//! key transitions into a small state machine (hold, tap, double-tap latch, combos).

use parking_lot::Mutex;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{platform, settings::PttKey};

const TAP_MAX: Duration = Duration::from_millis(250);
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(400);
const COMBO_WINDOW: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Copy)]
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

/// The sink a platform key listener reports into. Lives for the whole process.
pub struct KeyEvents {
    tx: Mutex<Sender<Input>>,
    key: Arc<Mutex<PttKey>>,
    held: AtomicBool,
}

impl KeyEvents {
    pub fn key(&self) -> PttKey {
        *self.key.lock()
    }

    /// The push-to-talk key went down or up (repeats are ignored).
    pub fn ptt(&self, down: bool) {
        if self.held.swap(down, Ordering::Relaxed) != down {
            self.send(if down { Input::PttDown } else { Input::PttUp });
        }
    }

    /// Any other key went down; only matters while the push-to-talk key is held.
    pub fn other_key(&self) {
        if self.held.load(Ordering::Relaxed) {
            self.send(Input::OtherKey);
        }
    }

    pub fn escape(&self) {
        self.send(Input::Escape);
    }

    fn send(&self, input: Input) {
        let _ = self.tx.lock().send(input);
    }
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

/// Starts the platform key listener thread and the state machine thread. `is_active`
/// reports whether a recording session is running (so the machine resyncs after the
/// widget or a shortcut ended it); `on_action` runs on the state machine thread.
pub fn spawn(
    initial: PttKey,
    is_active: impl Fn() -> bool + Send + 'static,
    on_action: impl Fn(Action) + Send + 'static,
) -> Listener {
    let key = Arc::new(Mutex::new(initial));
    let installed = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let events: &'static KeyEvents =
        Box::leak(Box::new(KeyEvents { tx: Mutex::new(tx), key: key.clone(), held: AtomicBool::new(false) }));
    let inst = installed.clone();
    thread::Builder::new()
        .name("wordfall-key-listener".into())
        .spawn(move || platform::run_key_listener(events, inst))
        .expect("spawn key listener thread");

    thread::Builder::new()
        .name("wordfall-ptt".into())
        .spawn(move || state_machine(rx, is_active, on_action))
        .expect("spawn ptt thread");

    Listener { key, installed }
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

struct Machine {
    phase: Phase,
    last_tap_up: Option<Instant>,
}

impl Machine {
    fn new() -> Self {
        Self { phase: Phase::Idle, last_tap_up: None }
    }

    fn step(&mut self, input: Input, now: Instant, active: bool, on_action: &impl Fn(Action)) {
        if matches!(self.phase, Phase::Latched) && !active {
            self.phase = Phase::Idle;
        }
        self.phase = match (self.phase, input) {
            (Phase::Idle, Input::PttDown) => {
                let double = self.last_tap_up.is_some_and(|t| now.duration_since(t) <= DOUBLE_TAP_WINDOW);
                self.last_tap_up = None;
                on_action(if double { Action::Latch } else { Action::StartHold });
                Phase::Holding { since: now, latch: double }
            }
            (Phase::Holding { latch: true, .. }, Input::PttUp) => Phase::Latched,
            (Phase::Holding { since, latch: false }, Input::PttUp) => {
                if now.duration_since(since) < TAP_MAX {
                    on_action(Action::Cancel);
                    self.last_tap_up = Some(now);
                } else {
                    on_action(Action::Stop);
                }
                Phase::Idle
            }
            (Phase::Holding { since, .. }, Input::OtherKey) if now.duration_since(since) < COMBO_WINDOW => {
                // Modifier+key shortcut (brightness, arrows, copy, …), not dictation.
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

fn state_machine(rx: Receiver<Input>, is_active: impl Fn() -> bool, on_action: impl Fn(Action)) {
    let mut machine = Machine::new();
    loop {
        let input = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(i) => i,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        machine.step(input, Instant::now(), is_active(), &on_action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Feeds (input, ms since start) pairs and returns the actions fired.
    fn run(inputs: &[(Input, u64)]) -> Vec<Action> {
        let t0 = Instant::now();
        let fired = RefCell::new(Vec::new());
        let mut m = Machine::new();
        for &(input, ms) in inputs {
            m.step(input, t0 + Duration::from_millis(ms), true, &|a| fired.borrow_mut().push(a));
        }
        fired.into_inner()
    }

    #[test]
    fn hold_starts_and_release_stops() {
        assert_eq!(run(&[(Input::PttDown, 0), (Input::PttUp, 900)]), vec![Action::StartHold, Action::Stop]);
    }

    #[test]
    fn quick_tap_cancels() {
        assert_eq!(run(&[(Input::PttDown, 0), (Input::PttUp, 100)]), vec![Action::StartHold, Action::Cancel]);
    }

    #[test]
    fn double_tap_latches_until_next_press() {
        let actions = run(&[
            (Input::PttDown, 0),
            (Input::PttUp, 100),
            (Input::PttDown, 300),
            (Input::PttUp, 400),
            (Input::PttDown, 5000),
            (Input::PttUp, 5100),
        ]);
        assert_eq!(actions, vec![Action::StartHold, Action::Cancel, Action::Latch, Action::Stop]);
    }

    #[test]
    fn combo_with_other_key_cancels() {
        assert_eq!(
            run(&[(Input::PttDown, 0), (Input::OtherKey, 200), (Input::PttUp, 600)]),
            vec![Action::StartHold, Action::Cancel]
        );
    }

    #[test]
    fn key_events_ignore_repeats_and_unheld_keys() {
        let (tx, rx) = mpsc::channel();
        let ev = KeyEvents { tx: Mutex::new(tx), key: Arc::new(Mutex::new(PttKey::RightControl)), held: AtomicBool::new(false) };
        ev.other_key();
        ev.ptt(true);
        ev.ptt(true);
        ev.other_key();
        ev.ptt(false);
        let got: Vec<String> = rx.try_iter().map(|i| format!("{i:?}")).collect();
        assert_eq!(got, ["PttDown", "OtherKey", "PttUp"]);
    }
}
