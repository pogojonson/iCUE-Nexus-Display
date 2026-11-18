//! K100 keystroke recording — pure-Rust port of `k100play.py::Recorder`.
//!
//! The reader thread owns the evdev node end to end: it opens, EVIOCGRABs,
//! reads and — via `GrabGuard`, on every exit path including panic unwinds —
//! ungrabs and closes it, so keystrokes are captured but never reach
//! applications while a recording is running (the Python docstring's
//! hard-won lesson: never close the fd from another thread).
//!
//! Only EV_KEY (autorepeat value 2 skipped) and EV_REL events are recorded,
//! with `dt` = milliseconds since the previous event — the exact Python
//! filter, resolved through the ported per-type name tables.

use super::k100::{wait_readable, PollOutcome};
use super::play::{key_name, rel_name};
use crate::core::events::{CoreEvent, LogLevel};
use crate::core::types::{MacroEvent, MacroEventKind};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use evdev::raw_stream::RawDevice;
use evdev::{EventType, InputEvent};
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// Select timeout so the stop flag is noticed promptly (Python `POLL_S`).
const POLL: Duration = Duration::from_millis(200);
/// How long `RecordStart` waits for the grab verdict (Python's
/// `started_evt.wait(1.5)`): "ok" must mean keys are captured, not just
/// that the thread spawned.
const START_TIMEOUT: Duration = Duration::from_millis(1500);

pub(crate) struct Recorder {
    stop_tx: Sender<()>,
    started_rx: Receiver<Result<(), String>>,
    handle: Option<JoinHandle<Result<Vec<MacroEvent>, String>>>,
}

impl Recorder {
    /// Spawn the reader thread against the K100's evdev node.
    pub(crate) fn start(path: &str, ev: Sender<CoreEvent>) -> Result<Self, String> {
        let (stop_tx, stop_rx) = crossbeam_channel::unbounded();
        let (started_tx, started_rx) = crossbeam_channel::unbounded();
        let path = path.to_owned();
        let handle = std::thread::Builder::new()
            .name("k100-recorder".to_string())
            .spawn(move || run(&path, ev, stop_rx, started_tx))
            .map_err(|e| format!("spawn recorder: {e}"))?;
        Ok(Recorder {
            stop_tx,
            started_rx,
            handle: Some(handle),
        })
    }

    /// Wait for the grab verdict: `Ok(())` means EVIOCGRAB succeeded and
    /// keys are being captured exclusively.
    pub(crate) fn wait_started(&self) -> Result<(), String> {
        match self.started_rx.recv_timeout(START_TIMEOUT) {
            Ok(res) => res,
            Err(_) => Err("recorder did not start".to_string()),
        }
    }

    /// `true` when the reader thread has finished on its own (device
    /// vanished, grab lost) — the watchdog reaps these.
    pub(crate) fn is_finished(&self) -> bool {
        self.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }

    /// Signal the reader to quit, join it (the 200 ms poll bounds how long
    /// that takes) and hand back everything captured plus its error —
    /// Python `Recorder.stop`.
    pub(crate) fn stop(mut self) -> (Vec<MacroEvent>, Option<String>) {
        let _ = self.stop_tx.send(());
        let Some(handle) = self.handle.take() else {
            return (Vec::new(), None);
        };
        match handle.join() {
            Ok(Ok(events)) => (events, None),
            Ok(Err(e)) => (Vec::new(), Some(e)),
            Err(_) => (Vec::new(), Some("recorder thread panicked".to_string())),
        }
    }
}

impl Drop for Recorder {
    /// Never leak an EVIOCGRAB: if the manager tears down without an
    /// explicit `stop()`, joining here still releases the grab.
    fn drop(&mut self) {
        if self.handle.is_some() {
            let _ = self.stop_tx.send(());
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// The reader thread: open + grab (the only failure path), then a
/// poll/fetch loop until stopped or the device dies.
fn run(
    path: &str,
    ev: Sender<CoreEvent>,
    stop_rx: Receiver<()>,
    started_tx: Sender<Result<(), String>>,
) -> Result<Vec<MacroEvent>, String> {
    let opened = RawDevice::open(path).and_then(|mut d| {
        d.grab()?; // exclusive: other clients (X/Wayland) see nothing
        Ok(d)
    });
    let dev = match opened {
        Ok(d) => d,
        Err(e) => {
            let msg = e.to_string();
            let _ = ev.send(CoreEvent::MacroLog {
                level: LogLevel::Error,
                msg: format!("record grab failed: {msg}"),
            });
            let _ = started_tx.send(Err(msg.clone()));
            return Err(msg);
        }
    };
    // From here on the guard owns the fd: ungrab + close on every path.
    let mut guard = GrabGuard(Some(dev));
    let _ = started_tx.send(Ok(()));

    let mut events: Vec<MacroEvent> = Vec::new();
    let mut prev: Option<SystemTime> = None;
    loop {
        if stopped(&stop_rx) {
            break;
        }
        let Some(fd) = guard.dev().map(|d| d.as_raw_fd()) else {
            break;
        };
        match wait_readable(fd, POLL) {
            PollOutcome::TimedOut => continue,
            // Device vanished mid-recording: break exactly like Python's
            // `select` OSError path (the error stays None — the reaped
            // `RecStopped` reports `reason: reader-exited`).
            PollOutcome::Failed(_) => break,
            PollOutcome::Ready => {}
        }
        if stopped(&stop_rx) {
            break;
        }
        let Some(dev) = guard.dev() else {
            break;
        };
        match dev.fetch_events() {
            Ok(iter) => {
                for e in iter {
                    if stopped(&stop_rx) {
                        break;
                    }
                    let Some(kind) = classify(e.event_type().0, e.value()) else {
                        continue;
                    };
                    let ts = e.timestamp();
                    let dt = dt_between(prev, ts);
                    prev = Some(ts);
                    let rec = MacroEvent {
                        dt,
                        kind,
                        code: code_name(&e, kind),
                        value: e.value(),
                    };
                    events.push(rec.clone());
                    let _ = ev.send(CoreEvent::RecEvent(rec));
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted || err.kind() == ErrorKind::WouldBlock => {
                continue; // EINTR / EAGAIN — retry, per the Python loop
            }
            // Read error (device gone): break silently, like Python.
            Err(_) => break,
        }
    }
    // Dropping the guard ungrabs and closes the fd — the only close path.
    drop(guard);
    Ok(events)
}

/// Event filter (Python's record loop): EV_KEY minus autorepeat (value 2),
/// and EV_REL; everything else is ignored.
fn classify(event_type: u16, value: i32) -> Option<MacroEventKind> {
    match event_type {
        t if t == EventType::KEY.0 => (value != 2).then_some(MacroEventKind::Key),
        t if t == EventType::RELATIVE.0 => Some(MacroEventKind::Rel),
        _ => None,
    }
}

/// evdev code → recorded name via the per-type table; unknown codes fall
/// back to their numeric string, exactly like Python's
/// `by_val.get(ev.code, str(ev.code))` (the player later skips them).
fn code_name(e: &InputEvent, kind: MacroEventKind) -> String {
    let name = match kind {
        MacroEventKind::Key => key_name(e.code()),
        MacroEventKind::Rel => rel_name(e.code()),
    };
    name.map(str::to_owned)
        .unwrap_or_else(|| e.code().to_string())
}

/// Milliseconds between consecutive event timestamps: 0 for the first
/// event and for a backwards clock (Python
/// `0 if prev is None else max(0, round((ts - prev) * 1000))`).
fn dt_between(prev: Option<SystemTime>, ts: SystemTime) -> u64 {
    let Some(prev) = prev else {
        return 0;
    };
    match ts.duration_since(prev) {
        Ok(d) => (d.as_secs_f64() * 1000.0).round() as u64,
        Err(_) => 0,
    }
}

fn stopped(rx: &Receiver<()>) -> bool {
    !matches!(rx.try_recv(), Err(TryRecvError::Empty))
}

/// Owns the grabbed evdev node.  Dropping it — normal exit *or* unwind —
/// is the only way the fd is ever released, so EVIOCGRAB can never leak.
struct GrabGuard(Option<RawDevice>);

impl GrabGuard {
    fn dev(&mut self) -> Option<&mut RawDevice> {
        self.0.as_mut()
    }
}

impl Drop for GrabGuard {
    fn drop(&mut self) {
        if let Some(mut dev) = self.0.take() {
            let _ = dev.ungrab(); // best effort; closing the fd (immediately
            // below, when `dev` drops) releases the grab regardless)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn event_classification() {
        let key = EventType::KEY.0;
        let rel = EventType::RELATIVE.0;
        assert_eq!(classify(key, 1), Some(MacroEventKind::Key)); // down
        assert_eq!(classify(key, 0), Some(MacroEventKind::Key)); // up
        assert_eq!(classify(key, 2), None); // autorepeat — never recorded
        assert_eq!(classify(rel, -3), Some(MacroEventKind::Rel));
        assert_eq!(classify(0x00, 0), None); // EV_SYN
        assert_eq!(classify(0x04, 1), None); // EV_MSC
    }

    #[test]
    fn dt_between_matches_python() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(dt_between(None, t0), 0); // first event
        assert_eq!(dt_between(Some(t0), t0), 0);
        assert_eq!(
            dt_between(Some(t0), t0 + Duration::from_millis(1500)),
            1500
        );
        // sub-millisecond rounding
        assert_eq!(
            dt_between(Some(t0), t0 + Duration::from_micros(12_700)),
            13
        );
        // backwards clock → clamped to 0 (Python's max(0, …))
        assert_eq!(dt_between(Some(t0), t0 - Duration::from_millis(500)), 0);
    }

    #[test]
    fn code_names_use_per_type_tables() {
        use crate::macro_::play::{key_code, rel_code};
        // a KEY event whose code collides with REL_WHEEL must record KEY_7
        let e = InputEvent::new(EventType::KEY.0, 8, 1);
        assert_eq!(code_name(&e, MacroEventKind::Key), "KEY_7");
        let e = InputEvent::new(EventType::RELATIVE.0, 8, -1);
        assert_eq!(code_name(&e, MacroEventKind::Rel), "REL_WHEEL");
        // round-trip back to codes (the player's lookup direction)
        assert_eq!(key_code("KEY_7"), Some(8));
        assert_eq!(rel_code("REL_WHEEL"), Some(8));
        // unknown code falls back to the numeric string (Python str(code))
        let e = InputEvent::new(EventType::KEY.0, 84, 1); // no name in table
        assert_eq!(key_name_missing(84), true);
        assert_eq!(code_name(&e, MacroEventKind::Key), "84");
    }

    fn key_name_missing(code: u16) -> bool {
        crate::macro_::play::key_name(code).is_none()
    }

    /// Opens the K100 evdev node WITHOUT grabbing — the live session keeps
    /// receiving keystrokes; only verifies the node is readable.
    #[test]
    #[ignore = "needs the K100 plugged in"]
    fn hardware_evdev_node_opens() {
        let dev = crate::macro_::k100::discover();
        let path = dev.evdev.expect("K100 evdev node");
        let node = RawDevice::open(&path).expect("open evdev node");
        assert!(
            node.name().unwrap_or("").contains("K100"),
            "wrong device: {:?}",
            node.name()
        );
        // deliberately no grab(): an EVIOCGRAB here would swallow the
        // user's keystrokes for the duration of the test
    }
}
