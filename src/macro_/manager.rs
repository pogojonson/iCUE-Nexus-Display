//! `MacroManager` — the K100 macro subsystem's owner: a manager thread
//! that executes `MacroCmd`s from the UI, owns the Bragi channel, runs the
//! 2 s monitor/keepalive/recording-watchdog tick, and drives the IFC2
//! reader, recorder and player threads.  Pure-Rust port of the Electron
//! host side (`macros.ts` + `main.ts` IPC handlers) plus the helper's
//! policy glue.
//!
//! ## Mode derivation (from `macros.ts`, single source of truth)
//!
//! * `sw` while recording OR calibrating OR any G-key binding exists
//! * `hw` when no bindings exist, or after an explicit `ReleaseToHw`
//! * playback never fires during recording/calibration or in `hw` mode
//! * press streaming (`listen`) stays on in both modes (keycap visualization)
//! * the persisted `gkeyMode` mirrors the derived mode (saved on change);
//!   "released" itself is session-scoped and never persisted
//!
//! ## Threading
//!
//! One manager thread (commands via `MacroCmd`, hardware notifications via
//! [`Internal`], 2 s tick for monitor duties) + the IFC2 reader thread
//! (spawned by `k100::Reader`), plus recorder/player threads spawned on
//! demand.  `MacroManager` holds the stop flag, the command sender and the
//! manager thread's join handle.

use super::k100::{
    discover, keepalive_due, Bragi, Devices, Reader, REC_MAX_S, MONITOR_TICK_S,
};
use super::play::Player;
use super::record::Recorder;
use crate::core::config::AppConfig;
use crate::core::events::{CoreEvent, LogLevel, MacroCmd, MacroState};
use crate::core::types::{
    GKeyAction, GKeyBinding, GkeyMode, Macro, MacroEvent, MacroKind, MacroStatus, GKEY_COUNT,
};
use crate::core::{save_locked, SharedConfig};
use crossbeam_channel::{Receiver, RecvError, Sender};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Messages from the hardware/decoder threads to the manager thread.
/// The manager is the only consumer: it runs the G-key binding policy,
/// forwards watch packets and logs, and updates playback status.
#[derive(Debug, Clone)]
pub(crate) enum Internal {
    GKey { g: usize, down: bool },
    Ifc2 { t: f64, hex: String },
    /// An IFC2 (or other) node died — the manager logs + notices the UI.
    DeviceLost { what: String },
    Log { level: LogLevel, msg: String },
    /// Playback start/stop with the run's sequence id (TS `playing` event).
    Playing { on: bool, seq: usize },
}

// ─── pure policy helpers (unit-tested) ────────────────────────────

/// Desired keyboard mode (`macros.ts` `desiredMode`): macro mode while
/// recording/calibrating or while bindings exist; hardware mode when
/// nothing is bound or the user released G-keys back to chords.
pub(crate) fn desired_mode(
    recording: bool,
    calibrating: bool,
    released: bool,
    binding_count: usize,
) -> GkeyMode {
    if recording || calibrating {
        return GkeyMode::Sw;
    }
    if released {
        return GkeyMode::Hw;
    }
    if binding_count > 0 {
        GkeyMode::Sw
    } else {
        GkeyMode::Hw
    }
}

/// Playback gating (TS `onGKey` + the `MacroCmd::Play` policy): never
/// during recording/calibration, never in hardware-chords mode.
pub(crate) fn playback_allowed(recording: bool, calibrating: bool, mode: GkeyMode) -> bool {
    !recording && !calibrating && mode == GkeyMode::Sw
}

/// Sanitize an incoming macro exactly like `main.ts` `macro:save-macro`:
/// name → `String(name || "Macro").slice(0, 60)` (60 chars here, JS slices
/// UTF-16 units), action ∈ {once, repeat, toggle} defaulting to `once`,
/// kind ∈ {macro, combo} defaulting to `macro`, speed finite & > 0 clamped
/// to 0.25..=4 otherwise 1, `created` 0 (absent on the Rust wire) → now.
pub(crate) fn sanitize_macro(mut m: Macro) -> Macro {
    if m.name.is_empty() {
        m.name = "Macro".to_string();
    }
    if m.name.chars().count() > 60 {
        m.name = m.name.chars().take(60).collect();
    }
    m.action = Some(m.action.unwrap_or(GKeyAction::Once));
    m.kind = Some(m.kind.unwrap_or(MacroKind::Macro));
    m.speed = Some(match m.speed {
        Some(s) if s.is_finite() && s > 0.0 => s.clamp(0.25, 4.0),
        _ => 1.0,
    });
    if m.created == 0 {
        m.created = crate::core::config::now_ms();
    }
    m
}

/// Snapshot of the persisted macro configuration for `CoreEvent::MacroState`.
pub(crate) fn macro_state(cfg: &AppConfig) -> MacroState {
    MacroState {
        macros: cfg.macros.clone(),
        gkeys: cfg.gkeys.clone(),
        gkey_mode: cfg.gkey_mode,
        gbit0: cfg.gbit0,
        gbits_calibrated: cfg.gbits_calibrated,
    }
}

/// Status-side view of the hidraw nodes: keys `"0" | "1" | "2"`.
fn ifaces_map(ifaces: &[Option<String>; 3]) -> HashMap<String, Option<String>> {
    (0..3).map(|i| (i.to_string(), ifaces[i].clone())).collect()
}

fn lock_cfg(cfg: &SharedConfig) -> MutexGuard<'_, AppConfig> {
    match cfg.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

// ─── public API ───────────────────────────────────────────────────

/// Handle to the running macro subsystem.  Cheap to clone-by-reference:
/// all methods take `&self`.
pub struct MacroManager {
    cmd_tx: Sender<MacroCmd>,
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl MacroManager {
    /// Spawn the manager thread + its device/monitor threads.
    /// `cfg` is the shared AppConfig (lock it to read/write macro state; call
    /// crate::core::save_locked while holding the lock after mutations).
    /// `ev` is the channel to the UI thread; emit CoreEvent::* through it.
    /// At startup emit MacroState + MacroStatus so the UI seeds itself.
    pub fn start(cfg: SharedConfig, ev: Sender<CoreEvent>) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (int_tx, int_rx) = crossbeam_channel::unbounded();

        // K100_GBIT0 overrides the initial G-bit offset (calibration debug
        // aid; only the reader/status see it — the persisted config keeps
        // its own value until an explicit SetGbit0).  K100_PYTHON is
        // meaningless now: there is no Python runtime.
        let gbit0 = std::env::var("K100_GBIT0")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or_else(|| lock_cfg(&cfg).gbit0);

        // Fully automatic mode (session-scoped release): re-derive from
        // bindings on every start, so a released keyboard never comes back
        // silently dead (TS `MacroManager.start`).
        let state = {
            let mut c = lock_cfg(&cfg);
            let derived = desired_mode(false, false, false, c.gkeys.len());
            if c.gkey_mode != derived {
                c.gkey_mode = derived;
                save_locked(&c);
            }
            macro_state(&c)
        };
        let _ = ev.send(CoreEvent::MacroState(Box::new(state)));
        let seed = MacroStatus {
            running: false,
            ready: false,
            mode: None,
            recording: false,
            playing: false,
            active_slot: None,
            gbit0,
            gbits_calibrated: lock_cfg(&cfg).gbits_calibrated,
            ifaces: HashMap::new(),
            evdev: None,
            last_error: None,
        };
        let _ = ev.send(CoreEvent::MacroStatus(Box::new(seed)));

        let stop = Arc::new(AtomicBool::new(false));
        let spawn = std::thread::Builder::new()
            .name("macro-manager".to_string())
            .spawn({
                let stop = Arc::clone(&stop);
                let cfg = cfg.clone();
                let ev = ev.clone();
                move || run_manager(cfg, ev, cmd_rx, int_tx, int_rx, stop, gbit0)
            });
        let handle = match spawn {
            Ok(h) => Some(h),
            Err(e) => {
                let _ = ev.send(CoreEvent::MacroLog {
                    level: LogLevel::Error,
                    msg: format!("macro manager failed to start: {e}"),
                });
                None
            }
        };
        MacroManager {
            cmd_tx,
            stop,
            handle: Mutex::new(handle),
        }
    }

    /// Non-blocking command dispatch (the UI sends MacroCmd variants).
    /// The manager's command sender — the UI uses this so its commands
    /// reach the manager thread (there is no separate UI-side channel).
    pub fn cmd_sender(&self) -> Sender<MacroCmd> {
        self.cmd_tx.clone()
    }

    /// App quit: stop player/recorder, revert keyboard to hw mode, join threads.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.cmd_tx.send(MacroCmd::Shutdown);
        let handle = {
            let mut g = match self.handle.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.take()
        };
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

// ─── manager thread ───────────────────────────────────────────────

fn run_manager(
    cfg: SharedConfig,
    ev: Sender<CoreEvent>,
    cmd_rx: Receiver<MacroCmd>,
    int_tx: Sender<Internal>,
    int_rx: Receiver<Internal>,
    stop: Arc<AtomicBool>,
    gbit0: u32,
) {
    // Keep one Internal sender alive for the loop's lifetime so the int
    // channel never reports "disconnected" while the manager runs.
    let _int_keep = int_tx.clone();
    let mut mgr = Mgr::new(cfg, ev, int_tx, gbit0);
    loop {
        let quit = crossbeam_channel::select! {
            recv(cmd_rx) -> msg => match msg {
                Ok(cmd) => {
                    let shutdown = matches!(cmd, MacroCmd::Shutdown);
                    if !shutdown && !stop.load(Ordering::SeqCst) {
                        mgr.handle_cmd(cmd);
                    }
                    shutdown || stop.load(Ordering::SeqCst)
                }
                // The UI dropped MacroManager — Python's stdin EOF path.
                Err(RecvError) => true,
            },
            recv(int_rx) -> msg => {
                if let Ok(m) = msg {
                    mgr.handle_internal(m);
                }
                false
            },
            // 2 s monitor tick: recording watchdog, keepalive, rescan
            default(Duration::from_secs(MONITOR_TICK_S)) => {
                if stop.load(Ordering::SeqCst) {
                    true
                } else {
                    mgr.tick();
                    false
                }
            }
        };
        if quit {
            break;
        }
    }
    mgr.cleanup();
}

/// Manager-thread state and command execution.
struct Mgr {
    cfg: SharedConfig,
    ev: Sender<CoreEvent>,
    bragi: Bragi,
    reader: Reader,
    player: Player,
    recorder: Option<Recorder>,
    /// Recording watchdog deadline (600 s, Python `rec_auto_stop`).
    rec_deadline: Option<Instant>,
    status: MacroStatus,
    /// Session-scoped "Release to hardware" override (never persisted).
    released: bool,
    /// True while the G-bit calibration wizard holds software mode.
    calibrating: bool,
    /// Sequence of the current playback (stale `Playing` filter).
    play_seq: usize,
    /// Last mode successfully set on the device (keepalive / reapply).
    last_mode: Option<GkeyMode>,
    last_keepalive: Instant,
    ifaces: [Option<String>; 3],
    evdev: Option<String>,
}

impl Mgr {
    fn new(cfg: SharedConfig, ev: Sender<CoreEvent>, int_tx: Sender<Internal>, gbit0: u32) -> Self {
        let devices = discover();
        let mut bragi = Bragi::new(int_tx.clone());
        if let Some(p) = devices.ifaces[1].clone() {
            if let Err(e) = bragi.open(&p) {
                let _ = int_tx.send(Internal::Log {
                    level: LogLevel::Error,
                    msg: format!("open cmd interface failed: {e}"),
                });
                bragi.close();
            }
        }
        let reader = Reader::start(devices.ifaces[2].clone(), gbit0, int_tx.clone());
        let player = Player::new(int_tx, ev.clone());
        let status = MacroStatus {
            running: false,
            ready: false,
            mode: None,
            recording: false,
            playing: false,
            active_slot: None,
            gbit0,
            gbits_calibrated: lock_cfg(&cfg).gbits_calibrated,
            ifaces: ifaces_map(&devices.ifaces),
            evdev: devices.evdev.clone(),
            last_error: None,
        };
        let mut mgr = Mgr {
            cfg,
            ev,
            bragi,
            reader,
            player,
            recorder: None,
            rec_deadline: None,
            status,
            released: false,
            calibrating: false,
            play_seq: 0,
            last_mode: None,
            last_keepalive: Instant::now(),
            ifaces: devices.ifaces,
            evdev: devices.evdev,
        };
        // The host's "ready" handler equivalent: report ready, apply the
        // persisted/derived mode (listen stays on inside the reader) — the
        // resulting push_status carries running/ready/mode/ifaces/evdev.
        mgr.status.running = true;
        mgr.status.ready = true;
        let want = { lock_cfg(&mgr.cfg).gkey_mode };
        mgr.apply_mode(want);
        mgr
    }

    // ── emit helpers ───────────────────────────────────────────────

    fn emit(&self, e: CoreEvent) {
        let _ = self.ev.send(e);
    }

    fn push_status(&self) {
        self.emit(CoreEvent::MacroStatus(Box::new(self.status.clone())));
    }

    fn push_state(&self) {
        let cfg = Arc::clone(&self.cfg);
        let c = lock_cfg(&cfg);
        let state = macro_state(&c);
        self.emit(CoreEvent::MacroState(Box::new(state)));
    }

    /// Mirror of the helper's `log()` plus the host's log handling: error
    /// logs also become `status.lastError` and push the status immediately.
    fn log(&mut self, level: LogLevel, msg: impl Into<String>) {
        let msg = msg.into();
        eprintln!(
            "[macro][{}] {msg}",
            match level {
                LogLevel::Info => "info",
                LogLevel::Error => "error",
            }
        );
        if level == LogLevel::Error {
            self.status.last_error = Some(msg.clone());
            self.push_status();
        }
        self.emit(CoreEvent::MacroLog { level, msg });
    }

    // ── mode management ────────────────────────────────────────────

    /// Push a mode to the keyboard (Bragi SET + verify) and update status.
    /// Mirrors TS `applyMode`: rescans when the channel is closed, uses the
    /// `no device` / `mode switch failed (…)` error texts, and always ends
    /// with a status push.  Press streaming is untouched — the reader keeps
    /// `listen` on in both modes.
    fn apply_mode(&mut self, mode: GkeyMode) {
        if !self.bragi.alive() {
            self.rescan(); // TS set-mode path rescans a closed channel first
        }
        if !self.bragi.alive() {
            self.status.last_error = Some("mode switch failed (no device)".to_string());
            self.push_status();
            return;
        }
        let out = self.bragi.set_mode(mode, true);
        if out.ok {
            self.last_mode = Some(mode);
            self.status.mode = Some(mode);
            self.status.last_error = None;
        } else {
            if let Some(v) = out.verified_mode() {
                self.status.mode = Some(v);
            }
            self.status.last_error = Some(format!("mode switch failed ({})", out.error_detail()));
        }
        self.push_status();
    }

    /// Re-derive the mode from state and apply it if it changed — the
    /// single path that changes modes (binding changes, recording start/stop
    /// and the calibration wizard funnel through here; TS `recomputeMode`).
    /// Returns whether the persisted mode changed (callers emit MacroState).
    fn recompute(&mut self, clear_release: bool, force: Option<GkeyMode>) -> bool {
        if clear_release {
            self.released = false;
        }
        let want = match force {
            Some(m) => m,
            None => desired_mode(
                self.status.recording,
                self.calibrating,
                self.released,
                self.binding_count(),
            ),
        };
        let changed = {
            let cfg = Arc::clone(&self.cfg);
            let mut c = lock_cfg(&cfg);
            if c.gkey_mode == want {
                false
            } else {
                c.gkey_mode = want;
                save_locked(&c);
                true
            }
        };
        if changed {
            self.apply_mode(want);
        }
        changed
    }

    fn binding_count(&self) -> usize {
        lock_cfg(&self.cfg).gkeys.len()
    }

    // ── command dispatch ───────────────────────────────────────────

    fn handle_cmd(&mut self, cmd: MacroCmd) {
        match cmd {
            MacroCmd::SaveMacro(m) => {
                let m = sanitize_macro(m);
                {
                    let cfg = Arc::clone(&self.cfg);
                    let mut c = lock_cfg(&cfg);
                    c.macros.insert(m.id.clone(), m);
                    save_locked(&c);
                }
                self.push_state();
            }
            MacroCmd::DeleteMacro(id) => {
                {
                    let cfg = Arc::clone(&self.cfg);
                    let mut c = lock_cfg(&cfg);
                    if c.macros.remove(&id).is_some() {
                        // drop bindings that referenced the macro (main.ts)
                        c.gkeys.retain(|_, b| b.macro_id != id);
                    }
                    save_locked(&c);
                }
                self.push_state();
            }
            MacroCmd::SetBinding(slot, binding) => self.set_binding(slot, binding),
            MacroCmd::ReleaseToHw => {
                // Explicit "Release to hardware" — the single manual control;
                // the flag is session-scoped, only the hw mode is persisted.
                self.released = true;
                {
                    let cfg = Arc::clone(&self.cfg);
                    let mut c = lock_cfg(&cfg);
                    c.gkey_mode = GkeyMode::Hw;
                    save_locked(&c);
                }
                self.push_state();
                // unconditional: releasing always re-sends hw (TS behaviour)
                self.apply_mode(GkeyMode::Hw);
            }
            MacroCmd::SetCalibrating(on) => {
                self.calibrating = on;
                // entering calibration clears a release override (TS
                // `setCalibrating` → recompute({ clearRelease: on }))
                self.recompute(on, None);
                self.push_state();
            }
            MacroCmd::RecordStart => self.record_start(),
            MacroCmd::RecordStop => self.record_stop(None),
            MacroCmd::Play {
                events,
                speed,
                repeat,
            } => self.play(events, speed, repeat),
            MacroCmd::StopPlay => self.player.stop(), // status via Playing
            MacroCmd::Watch(seconds) => {
                self.reader.set_watch(seconds);
                self.log(LogLevel::Info, format!("watching IFC2 for {seconds:.0}s"));
            }
            MacroCmd::SetGbit0(bit0) => {
                {
                    let cfg = Arc::clone(&self.cfg);
                    let mut c = lock_cfg(&cfg);
                    c.gbit0 = bit0;
                    c.gbits_calibrated = true;
                    save_locked(&c);
                }
                self.reader.set_gbit0(bit0);
                self.status.gbit0 = bit0;
                self.status.gbits_calibrated = true;
                self.log(LogLevel::Info, format!("G_BIT0={bit0}"));
                self.push_state();
                self.push_status();
            }
            MacroCmd::Rescan => {
                self.rescan();
                self.push_status();
            }
            // Intercepted by the run loop; kept for match exhaustiveness.
            MacroCmd::Shutdown => {}
        }
    }

    fn set_binding(&mut self, slot: usize, binding: Option<GKeyBinding>) {
        if !(1..=GKEY_COUNT).contains(&slot) {
            self.log(LogLevel::Error, format!("set-binding: invalid slot {slot}"));
            return;
        }
        // Validate before mutating (main.ts rejects unknown macros).
        if let Some(b) = &binding {
            let cfg = Arc::clone(&self.cfg);
            let c = lock_cfg(&cfg);
            if !c.macros.contains_key(&b.macro_id) {
                let msg = format!("set-binding: unknown macro {}", b.macro_id);
                drop(c);
                self.log(LogLevel::Error, msg);
                return;
            }
        }
        let key = slot.to_string();
        let binding_set = binding.is_some();
        {
            let cfg = Arc::clone(&self.cfg);
            let mut c = lock_cfg(&cfg);
            match binding {
                Some(b) => {
                    c.gkeys.insert(key, b);
                }
                None => {
                    c.gkeys.remove(&key);
                }
            }
            save_locked(&c);
        }
        // Assigning a binding counts as a macro action (clears a release
        // override); unbinding re-derives — removing the last binding hands
        // chords back (main.ts `macro:set-binding`).
        self.recompute(binding_set, None);
        self.push_state();
    }

    /// Test/G-key playback entry point (TS `startPlay`).  Returns success
    /// for `MacroNotice`; on failure `play_seq` was still consumed but no
    /// run exists (the previous run was cancelled and its stop will be
    /// seq-filtered), so the caller resets playing/active_slot itself.
    fn start_play(
        &mut self,
        slot: Option<usize>,
        events: Vec<MacroEvent>,
        speed: f64,
        repeat: bool,
    ) -> bool {
        self.play_seq += 1;
        let seq = self.play_seq;
        match self.player.play(events, speed, repeat, seq) {
            Ok(()) => {
                if let Some(s) = slot {
                    self.status.active_slot = Some(s);
                    self.push_status();
                }
                true
            }
            Err(e) => {
                // Player::play cancels any previous run before its
                // reachable failure points, so nothing is playing now —
                // and the cancelled run's stop notification will be
                // filtered out as stale by seq.  Reset here instead.
                self.status.playing = false;
                self.status.active_slot = None;
                self.log(LogLevel::Error, format!("play: {e}"));
                false
            }
        }
    }

    fn play(&mut self, events: Vec<MacroEvent>, speed: f64, repeat: bool) {
        // "playback allowed" policy: never while the recorder or the
        // calibration wizard holds the keyboard, never in hw-chords mode.
        if self.status.recording || self.calibrating {
            let why = if self.status.recording {
                "recording"
            } else {
                "calibration"
            };
            self.emit(CoreEvent::MacroNotice {
                ok: false,
                msg: format!("playback blocked ({why})"),
            });
            return;
        }
        let mode = { lock_cfg(&self.cfg).gkey_mode };
        if !playback_allowed(false, false, mode) {
            self.emit(CoreEvent::MacroNotice {
                ok: false,
                msg: "playback blocked (hardware mode)".to_string(),
            });
            return;
        }
        if events.is_empty() {
            self.log(LogLevel::Error, "play: no events");
            self.emit(CoreEvent::MacroNotice {
                ok: false,
                msg: "play: no events".to_string(),
            });
            return;
        }
        // Test playback: no slot semantics (TS `testPlay` → startPlay(null))
        let ok = self.start_play(None, events, speed, repeat);
        self.emit(CoreEvent::MacroNotice {
            ok,
            msg: if ok {
                "playing".to_string()
            } else {
                "playback failed".to_string()
            },
        });
    }

    // ── recording ──────────────────────────────────────────────────

    fn record_start(&mut self) {
        if self.status.recording {
            self.emit(CoreEvent::RecStarted {
                ok: false,
                err: Some("already recording".to_string()),
            });
            return;
        }
        // Recording forces macro mode (G-chords must not leak into the
        // capture) and counts as a macro action → clears a release.
        self.released = false;
        self.recompute(false, Some(GkeyMode::Sw));

        if self.evdev.is_none() {
            self.rescan();
        }
        let Some(path) = self.evdev.clone() else {
            self.record_failed("no K100 evdev node".to_string());
            return;
        };
        let rec = match Recorder::start(&path, self.ev.clone()) {
            Ok(r) => r,
            Err(e) => {
                self.record_failed(e);
                return;
            }
        };
        // The grab happens inside the reader thread — wait for its verdict
        // so "ok" really means "keys are captured".
        match rec.wait_started() {
            Ok(()) => {
                self.rec_deadline = Some(Instant::now() + Duration::from_secs(REC_MAX_S));
                self.recorder = Some(rec);
                self.status.recording = true;
                self.status.last_error = None;
                self.emit(CoreEvent::RecStarted {
                    ok: true,
                    err: None,
                });
                self.push_status();
            }
            Err(e) => {
                let _ = rec.stop(); // join — releases a partial grab
                self.record_failed(e);
            }
        }
    }

    fn record_failed(&mut self, err: String) {
        self.emit(CoreEvent::RecStarted {
            ok: false,
            err: Some(err.clone()),
        });
        self.status.recording = false;
        self.status.last_error = Some(err);
        self.push_status();
        // never started → leave auto-derived mode (TS recordStart)
        self.recompute(false, None);
    }

    /// Stop the recorder (if any), emit `RecStopped`, then return the
    /// keyboard to its auto-derived mode.  `reason` is set for watchdog
    /// stops (`timeout` / `reader-exited`), `None` for user-initiated ones.
    fn record_stop(&mut self, reason: Option<String>) {
        let rec = self.recorder.take();
        self.rec_deadline = None;
        self.status.recording = false;
        match rec {
            None => {
                self.emit(CoreEvent::RecStopped {
                    ok: false,
                    count: 0,
                    err: Some("not recording".to_string()),
                    reason,
                    events: Vec::new(),
                });
            }
            Some(rec) => {
                let (events, err) = rec.stop();
                let count = events.len();
                let ok = err.is_none();
                self.emit(CoreEvent::RecStopped {
                    ok,
                    count,
                    err,
                    reason,
                    events,
                });
            }
        }
        self.push_status();
        if self.recompute(false, None) {
            self.push_state();
        }
    }

    /// Watchdog (Python `_rec_watchdog`): reap a reader that died on its
    /// own, or auto-stop at the 600 s cap.
    fn rec_watchdog(&mut self) {
        let Some(rec) = self.recorder.as_ref() else {
            return;
        };
        if rec.is_finished() {
            self.record_stop(Some("reader-exited".to_string()));
        } else if self.rec_deadline.is_some_and(|d| Instant::now() >= d) {
            self.record_stop(Some("timeout".to_string()));
            self.log(
                LogLevel::Info,
                format!("recording auto-stopped after {REC_MAX_S}s"),
            );
        }
    }

    // ── G-key policy ───────────────────────────────────────────────

    fn on_g_key(&mut self, g: usize, down: bool) {
        if !(1..=GKEY_COUNT).contains(&g) {
            return;
        }
        self.emit(CoreEvent::GKey { g, down });
        // Never fire macros while a recording is capturing — the press is
        // for the recording UI, not for playback.
        if self.status.recording || self.calibrating {
            return;
        }
        let cfg = Arc::clone(&self.cfg);
        let (mode, macro_id) = {
            let c = lock_cfg(&cfg);
            (
                c.gkey_mode,
                c.gkeys.get(&g.to_string()).map(|b| b.macro_id.clone()),
            )
        };
        if mode != GkeyMode::Sw {
            return; // hardware chords mode: nothing to do
        }
        let Some(macro_id) = macro_id else {
            return;
        };
        let m = { lock_cfg(&cfg).macros.get(&macro_id).cloned() };
        let Some(m) = m else {
            return;
        };
        if m.events.is_empty() {
            return;
        }
        // Playback settings live on the macro itself (dialog-managed), not
        // per slot.
        let action = m.action.unwrap_or(GKeyAction::Once);
        let speed = m.speed.unwrap_or(1.0);
        match action {
            GKeyAction::Once => {
                if down {
                    self.start_play(Some(g), m.events, speed, false);
                }
            }
            GKeyAction::Repeat => {
                if down {
                    self.start_play(Some(g), m.events, speed, true);
                } else if self.status.active_slot == Some(g) {
                    self.player.stop();
                }
            }
            GKeyAction::Toggle => {
                if !down {
                    return;
                }
                if self.status.active_slot == Some(g) {
                    self.player.stop();
                } else {
                    self.start_play(Some(g), m.events, speed, true);
                }
            }
        }
    }

    /// TS `onPlaying`: only the current sequence may touch the status;
    /// the push happens even for stale notifications (like the TS handler).
    fn on_playing(&mut self, on: bool, seq: usize) {
        if seq == self.play_seq {
            if on {
                self.status.playing = true;
            } else {
                self.status.playing = false;
                self.status.active_slot = None;
            }
        }
        self.push_status();
    }

    // ── internal (thread → manager) messages ───────────────────────

    fn handle_internal(&mut self, msg: Internal) {
        match msg {
            Internal::GKey { g, down } => self.on_g_key(g, down),
            Internal::Ifc2 { t, hex } => self.emit(CoreEvent::Ifc2 { t, hex }),
            Internal::DeviceLost { what } => {
                self.log(
                    LogLevel::Error,
                    format!("device lost ({what}) — waiting for replug"),
                );
                self.emit(CoreEvent::MacroNotice {
                    ok: false,
                    msg: format!("device lost: {what}"),
                });
            }
            Internal::Log { level, msg } => self.log(level, msg),
            Internal::Playing { on, seq } => self.on_playing(on, seq),
        }
    }

    // ── monitor tick / rescan ──────────────────────────────────────

    /// 2 s tick (Python `monitor_loop` + `keepalive_loop` +
    /// `_rec_watchdog`): reap recordings, keepalive in sw mode, rescan on
    /// node change — and rescue a closed Bragi channel (the helper's
    /// `elif not bragi.alive` branch, whose reopen never actually fired
    /// in Python because unchanged paths skipped it; fixed here).
    fn tick(&mut self) {
        self.rec_watchdog();
        if keepalive_due(self.last_keepalive.elapsed()) {
            self.last_keepalive = Instant::now();
            if self.last_mode == Some(GkeyMode::Sw) && self.bragi.alive() {
                self.bragi.keepalive();
            }
        }
        let dev = discover();
        let nodes_changed = dev.ifaces != self.ifaces || dev.evdev != self.evdev;
        if nodes_changed || (!self.bragi.alive() && dev.ifaces[1].is_some()) {
            self.rescan();
        }
    }

    /// Re-enumerate the keyboard (Python `Helper.rescan`): reopen changed
    /// nodes, report discovery, and re-apply the last mode verified on the
    /// device (verify off, the helper's "reapply").
    fn rescan(&mut self) {
        let dev: Devices = discover();
        let old_ifaces = self.ifaces.clone();
        let old_evdev = self.evdev.clone();
        self.ifaces = dev.ifaces;
        self.evdev = dev.evdev;
        let changed = self.ifaces != old_ifaces || self.evdev != old_evdev;

        // interface 1: Bragi command channel
        let ifc1 = self.ifaces[1].clone();
        let reopen = old_ifaces[1] != self.ifaces[1] || !self.bragi.alive();
        if reopen {
            self.bragi.close();
            if let Some(p) = &ifc1 {
                if let Err(e) = self.bragi.open(p) {
                    self.log(LogLevel::Error, format!("reopen cmd failed: {e}"));
                }
            }
        }
        // interface 2: the reader reopens its own fd when the path changes
        if old_ifaces[2] != self.ifaces[2] {
            self.reader.set_path(self.ifaces[2].clone());
        }

        if changed {
            // status carries the new ifaces/evdev (Python's "devices found")
            self.status.ifaces = ifaces_map(&self.ifaces);
            self.status.evdev = self.evdev.clone();
            self.emit(CoreEvent::MacroNotice {
                ok: true,
                msg: "K100 devices found".to_string(),
            });
            self.push_status();
            // reapply the last set mode after replug (Python "reapply")
            if let (Some(m), true) = (self.last_mode, self.bragi.alive()) {
                let out = self.bragi.set_mode(m, false);
                if out.ok {
                    self.status.mode = Some(m);
                    self.status.last_error = None;
                } else {
                    self.status.last_error =
                        Some(format!("mode switch failed ({})", out.error_detail()));
                }
                self.push_status();
            }
        }
    }

    // ── shutdown ───────────────────────────────────────────────────

    /// App-shutdown teardown (Python `Helper.shutdown`): never leave
    /// keystrokes grabbed or keys held, ALWAYS hand G-keys back to
    /// hardware chords, close every fd.
    fn cleanup(&mut self) {
        if let Some(rec) = self.recorder.take() {
            let _ = rec.stop();
        }
        self.player.close();
        if self.bragi.alive() {
            let out = self.bragi.set_mode(GkeyMode::Hw, true);
            if out.ok {
                self.status.mode = Some(GkeyMode::Hw);
                self.status.last_error = None;
            } else {
                if let Some(v) = out.verified_mode() {
                    self.status.mode = Some(v);
                }
                self.status.last_error =
                    Some(format!("mode switch failed ({})", out.error_detail()));
            }
            self.log(LogLevel::Info, "reverted to HW mode on exit".to_string());
        }
        self.bragi.close();
        self.reader.shutdown();
        // mirror the host's process-exit status push (lastError survives)
        self.status.running = false;
        self.status.ready = false;
        self.status.mode = None;
        self.status.recording = false;
        self.status.playing = false;
        self.status.active_slot = None;
        self.push_status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{MacroEventKind, DEFAULT_GBIT0};

    #[test]
    fn mode_derivation_policy_table() {
        // (recording, calibrating, released, bindings) → expected mode
        let table: [((bool, bool, bool, usize), GkeyMode); 9] = [
            ((false, false, false, 0), GkeyMode::Hw),  // nothing bound
            ((false, false, false, 3), GkeyMode::Sw),  // bindings → sw
            ((true, false, false, 0), GkeyMode::Sw),   // recording forces sw
            ((false, true, false, 0), GkeyMode::Sw),   // calibrating → sw
            ((false, false, true, 0), GkeyMode::Hw),   // released → hw
            ((false, false, true, 5), GkeyMode::Hw),   // release beats bindings
            ((true, false, true, 0), GkeyMode::Sw),    // recording beats release
            ((false, true, true, 0), GkeyMode::Sw),    // calibration beats release
            ((true, true, true, 0), GkeyMode::Sw),
        ];
        for ((r, c, rel, n), want) in table {
            assert_eq!(
                desired_mode(r, c, rel, n),
                want,
                "recording={r} calibrating={c} released={rel} bindings={n}"
            );
        }
    }

    #[test]
    fn playback_gating_policy() {
        assert!(playback_allowed(false, false, GkeyMode::Sw));
        assert!(!playback_allowed(true, false, GkeyMode::Sw)); // recording
        assert!(!playback_allowed(false, true, GkeyMode::Sw)); // calibration
        assert!(!playback_allowed(false, false, GkeyMode::Hw)); // hw chords
    }

    fn fixture(id: &str) -> Macro {
        Macro {
            id: id.to_string(),
            name: String::new(),
            events: vec![MacroEvent {
                dt: 0,
                kind: MacroEventKind::Key,
                code: "KEY_A".to_string(),
                value: 1,
            }],
            created: 0,
            kind: None,
            action: None,
            speed: None,
        }
    }

    #[test]
    fn sanitize_clamps_like_main_ts() {
        let m = sanitize_macro(fixture("m1"));
        assert_eq!(m.name, "Macro"); // empty name → "Macro"
        assert_eq!(m.action, Some(GKeyAction::Once));
        assert_eq!(m.kind, Some(MacroKind::Macro));
        assert_eq!(m.speed, Some(1.0));
        assert!(m.created > 0); // 0 = absent → Date.now()

        // name truncation at 60 chars
        let mut long = fixture("m2");
        long.name = "x".repeat(120);
        let m = sanitize_macro(long);
        assert_eq!(m.name.chars().count(), 60);

        // speed: clamp up, clamp down, invalid → 1
        let mut s = fixture("m3");
        s.speed = Some(9.0);
        assert_eq!(sanitize_macro(s).speed, Some(4.0));
        let mut s = fixture("m4");
        s.speed = Some(0.1);
        assert_eq!(sanitize_macro(s).speed, Some(0.25));
        let mut s = fixture("m5");
        s.speed = Some(-3.0);
        assert_eq!(sanitize_macro(s).speed, Some(1.0)); // not > 0 → default
        let mut s = fixture("m6");
        s.speed = Some(f64::NAN);
        assert_eq!(sanitize_macro(s).speed, Some(1.0));
        let mut s = fixture("m7");
        s.speed = None;
        assert_eq!(sanitize_macro(s).speed, Some(1.0));
        let mut s = fixture("m8");
        s.speed = Some(2.5);
        assert_eq!(sanitize_macro(s).speed, Some(2.5));

        // action / kind / created pass through when already valid
        let mut m = fixture("m9");
        m.action = Some(GKeyAction::Toggle);
        m.kind = Some(MacroKind::Combo);
        m.created = 1_700_000_000_000;
        m.name = "ok".to_string();
        let m = sanitize_macro(m);
        assert_eq!(m.action, Some(GKeyAction::Toggle));
        assert_eq!(m.kind, Some(MacroKind::Combo));
        assert_eq!(m.created, 1_700_000_000_000);
        assert_eq!(m.name, "ok");
    }

    #[test]
    fn macro_state_snapshot_reflects_config() {
        let mut cfg = AppConfig::default();
        cfg.gkey_mode = GkeyMode::Sw;
        cfg.gbit0 = 150;
        cfg.gbits_calibrated = true;
        cfg.macros.insert("a".to_string(), fixture("a"));
        cfg.gkeys
            .insert("1".to_string(), GKeyBinding { macro_id: "a".to_string() });
        let st = macro_state(&cfg);
        assert_eq!(st.gkey_mode, GkeyMode::Sw);
        assert_eq!(st.gbit0, 150);
        assert!(st.gbits_calibrated);
        assert_eq!(st.macros.len(), 1);
        assert_eq!(st.gkeys.get("1").map(|b| b.macro_id.as_str()), Some("a"));

        let defaults = macro_state(&AppConfig::default());
        assert_eq!(defaults.gkey_mode, GkeyMode::Hw);
        assert_eq!(defaults.gbit0, DEFAULT_GBIT0);
        assert!(defaults.macros.is_empty());
    }

    #[test]
    fn ifaces_map_uses_string_keys() {
        let ifaces = [
            Some("/dev/hidraw27".to_string()),
            Some("/dev/hidraw28".to_string()),
            None,
        ];
        let map = ifaces_map(&ifaces);
        assert_eq!(map.get("0").unwrap().as_deref(), Some("/dev/hidraw27"));
        assert_eq!(map.get("1").unwrap().as_deref(), Some("/dev/hidraw28"));
        assert_eq!(map.get("2"), Some(&None));
        assert_eq!(map.len(), 3);
    }
}
