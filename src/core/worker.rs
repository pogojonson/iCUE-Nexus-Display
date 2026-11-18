//! Display worker thread — owns ALL hardware I/O (Nexus HID + sensor
//! sampling), so the UI thread never blocks.
//!
//! Message protocol (see `core::events`):
//!   UI → worker: `WorkerMsg` (Reload / Connect / Disconnect / animations / Shutdown)
//!   worker → UI: `CoreEvent` (Connection, DeviceInfo, Sensors, PageChange, …)
//!
//! Cadence rules — three independent timers:
//!
//! * **data** = `displayUpdateInterval` (per page) — sensor sampling, severity
//!   evaluation and the UI event stream all follow this rate. The next
//!   deadline is absolute (`lastDataReadAt + interval`) so it doesn't drift.
//! * **animation** = `animationPeriodMs(alertStyle)` — while a selected sensor
//!   is critical, extra frames are uploaded on each phase boundary from the
//!   CACHED readings, never later than the data deadline (25 ms floor). The
//!   animation cadence never samples sensors or emits to the UI.
//! * **poll** = `sensorPollInterval` (2000 ms, not persisted) — severity
//!   watchdog only; fresh readings are adopted only on a severity crossing,
//!   then a render is scheduled within 50 ms.
//!
//! Page rotation also runs here (the Electron app had it in main): it adopts
//! the next page's brightness/interval, persists, and emits PageChange.

use crate::core::config::now_ms;
use crate::core::device::NexusDevice;
use crate::core::events::{CoreEvent, WorkerMsg};
use crate::core::renderer::{render_frame, RenderConfig};
use crate::core::sensors::SensorReader;
use crate::core::thresholds::{animation_period_ms, apply_severities};
use crate::core::types::*;
use crate::core::{save_locked, AppConfig, SharedConfig};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const SENSOR_EMIT_MIN_INTERVAL: Duration = Duration::from_millis(250);

pub fn spawn(
    cfg: SharedConfig,
    cmd_rx: Receiver<WorkerMsg>,
    event_tx: Sender<CoreEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("nexus-worker".into())
        .spawn(move || worker_loop(cfg, cmd_rx, event_tx))
        .expect("spawn worker thread")
}

struct Worker {
    cfg: SharedConfig,
    event_tx: Sender<CoreEvent>,
    device: NexusDevice,
    sensors: SensorReader,
    running: bool,
    // ─── on-change sensor emission (coalesced) ───
    last_emit_sig: String,
    last_emit_at: Instant,
    pending_emit: Option<Vec<SensorReading>>,
    emit_timer_deadline: Option<Instant>,
    // ─── frame skip ───
    last_frame_sig: String,
    last_severity_sig: String,
    last_anim_step: i64, // -1 = animation inactive
    // ─── cached readings ───
    cached: Vec<SensorReading>,
    last_data_read_at: Instant,
    // ─── timers (absolute deadlines) ───
    display_due: Option<Instant>,
    poll_due: Option<Instant>,
    rotation_due: Option<Instant>,
    last_logged_severity: HashMap<String, Severity>,
    emit_logged: bool,
}

fn worker_loop(cfg: SharedConfig, cmd_rx: Receiver<WorkerMsg>, event_tx: Sender<CoreEvent>) {
    let rotation_due = rotation_deadline(&cfg, Instant::now());
    let mut w = Worker {
        cfg,
        event_tx,
        device: NexusDevice::new(),
        sensors: SensorReader::new(),
        running: false,
        last_emit_sig: String::new(),
        last_emit_at: Instant::now() - SENSOR_EMIT_MIN_INTERVAL,
        pending_emit: None,
        emit_timer_deadline: None,
        last_frame_sig: String::new(),
        last_severity_sig: String::new(),
        last_anim_step: -1,
        cached: Vec::new(),
        last_data_read_at: Instant::now(),
        display_due: None,
        poll_due: None,
        rotation_due,
        last_logged_severity: HashMap::new(),
        emit_logged: false,
    };
    w.main_loop(cmd_rx);
}

impl Worker {
    fn main_loop(&mut self, cmd_rx: Receiver<WorkerMsg>) {
        loop {
            // Sleep until the earliest deadline, woken early by commands.
            let now = Instant::now();
            let next = [
                self.display_due,
                self.poll_due,
                self.rotation_due,
                self.emit_timer_deadline,
            ]
            .into_iter()
            .flatten()
            .min();

            let msg = match next {
                Some(deadline) if deadline > now => {
                    match cmd_rx.recv_timeout(deadline - now) {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                Some(_) => match cmd_rx.try_recv() {
                    Ok(m) => Some(m),
                    Err(_) => None, // timers are due — fall through
                },
                None => match cmd_rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                },
            };

            if let Some(msg) = msg {
                if !self.handle_cmd(msg) {
                    break;
                }
            }
            self.run_due_timers();
        }
        // Shutdown path: restore idle animation and close the device.
        self.do_disconnect();
    }

    /// Returns false when the thread should exit.
    fn handle_cmd(&mut self, msg: WorkerMsg) -> bool {
        match msg {
            WorkerMsg::Reload => self.apply_config(),
            WorkerMsg::Connect => self.do_connect(),
            WorkerMsg::Disconnect => self.do_disconnect(),
            WorkerMsg::PlayAnimation { anim, run_loop } => {
                if let Err(e) = self.device.play_animation(AnimationId(anim), run_loop) {
                    self.emit(CoreEvent::WorkerError(e));
                }
            }
            WorkerMsg::StopAnimation => {
                if let Err(e) = self.device.stop_animation() {
                    self.emit(CoreEvent::WorkerError(e));
                }
            }
            WorkerMsg::Shutdown => {
                self.do_disconnect();
                return false;
            }
        }
        true
    }

    fn emit(&self, ev: CoreEvent) {
        let _ = self.event_tx.send(ev);
    }

    // ─── Connect / disconnect ────────────────────────────────────────

    fn do_connect(&mut self) {
        self.emit(CoreEvent::Connection(ConnectionStatus::Connecting));
        // Discover first (reads firmware/serial/geometry), then open.
        let info = self.device.discover();
        let info_clone = info.clone();
        match self.device.connect() {
            Ok(()) => {
                self.emit(CoreEvent::DeviceInfo(info_clone));
                self.running = true;
                self.last_frame_sig.clear();
                self.last_emit_sig.clear();
                self.last_severity_sig.clear();
                self.last_anim_step = -1;
                self.cached.clear();
                self.last_data_read_at = Instant::now() - Duration::from_secs(3600);
                self.pending_emit = None;
                self.emit_timer_deadline = None;
                // Apply brightness now that the port is open.
                let brightness = self.with_cfg(|c| c.brightness);
                if let Err(e) = self.device.set_brightness(brightness) {
                    self.emit(CoreEvent::WorkerError(e));
                }
                self.display_due = Some(Instant::now());
                self.poll_due = Some(Instant::now());
                self.emit(CoreEvent::Connection(ConnectionStatus::Connected));
            }
            Err(e) => {
                self.emit(CoreEvent::DeviceInfo(None));
                self.emit(CoreEvent::WorkerError(e));
                self.emit(CoreEvent::Connection(ConnectionStatus::Error));
            }
        }
    }

    fn do_disconnect(&mut self) {
        let was_running = self.running;
        self.running = false;
        self.display_due = None;
        self.poll_due = None;
        self.pending_emit = None;
        self.emit_timer_deadline = None;
        // Drop the snapshot — a reconnect must sample fresh values.
        self.cached.clear();
        self.last_data_read_at = Instant::now() - Duration::from_secs(3600);
        self.last_anim_step = -1;
        if self.device.is_connected() {
            self.device.disconnect(); // restores idle animation (best effort)
        }
        if was_running {
            self.emit(CoreEvent::Connection(ConnectionStatus::Disconnected));
        }
    }

    // ─── Config ──────────────────────────────────────────────────────

    fn with_cfg<T>(&self, f: impl FnOnce(&AppConfig) -> T) -> T {
        let guard = self.cfg.lock().unwrap();
        f(&guard)
    }

    fn apply_config(&mut self) {
        // Re-read shared config; re-apply brightness if changed; re-evaluate
        // thresholds against cached readings; render promptly.
        let (brightness, poll_interval, thresholds, alert_style) = {
            let guard = self.cfg.lock().unwrap();
            (
                guard.brightness,
                guard.sensor_poll_interval,
                guard.thresholds.clone(),
                guard.alert_style.clone(),
            )
        };
        let _ = (&thresholds, &alert_style); // used below via lock

        if self.device.is_connected() {
            if let Err(e) = self.device.set_brightness(brightness) {
                self.emit(CoreEvent::WorkerError(e));
            }
        }

        if self.running {
            // Threshold edits arrive as reloads — re-evaluate against the
            // cached readings (pure in-memory pass, no sensor sample).
            if !self.cached.is_empty() {
                let thresholds = self.with_cfg(|c| c.thresholds.clone());
                apply_severities(&mut self.cached, &thresholds);
                self.log_severity_changes(&self.cached.clone());
                let readings = self.cached.clone();
                self.maybe_emit(readings);
            }
            if let Some(due) = self.poll_due {
                // Reschedule only if the interval actually moved — approximated
                // by simply keeping the existing deadline (2 s cadence either
                // way); the next natural poll picks the new interval up.
                let _ = due;
            }
            // Page/content may have changed — render promptly. A no-change
            // tick is cheap (frame signature unchanged → upload skipped).
            self.display_due = Some(Instant::now() + Duration::from_millis(50));
            let _ = poll_interval;
        }
        self.rotation_due = rotation_deadline(&self.cfg, Instant::now());
    }

    // ─── Timers ──────────────────────────────────────────────────────

    fn run_due_timers(&mut self) {
        let now = Instant::now();
        if self.rotation_due.is_some_and(|d| d <= now) {
            self.rotation_tick();
        }
        if self.poll_due.is_some_and(|d| d <= now) {
            self.poll_tick();
        }
        if self.display_due.is_some_and(|d| d <= now) {
            self.display_tick();
        }
        if self.emit_timer_deadline.is_some_and(|d| d <= now) {
            self.flush_emit();
        }
    }

    fn display_tick(&mut self) {
        self.display_due = None;
        if !self.running || !self.device.is_connected() {
            return;
        }
        // ── Data phase: sample at the update-rate cadence ──
        let interval = self.with_cfg(|c| c.display_update_interval);
        let mut anim_active = false;
        let mut period = 500u64; // only meaningful when anim_active
        let data_due = self.last_data_read_at.elapsed().as_millis() as u64 >= interval;
        if data_due {
            let mut readings = self.sensors.read_all_sensors();
            let thresholds = self.with_cfg(|c| c.thresholds.clone());
            apply_severities(&mut readings, &thresholds);
            self.log_severity_changes(&readings);
            self.last_data_read_at = Instant::now();
            let sig = readings_signature(&readings);
            self.maybe_emit_with_sig(readings.clone(), sig);
            self.cached = readings;
        }
        let readings = self.cached.clone();
        let page = self.current_page();
        // Always refresh — a stale signature (e.g. after the last page is
        // removed) would make the poll watchdog re-trigger on every poll.
        self.last_severity_sig = severity_signature(&readings, page.as_ref());

        if let Some(page) = page.as_ref() {
            let alert_style = self.with_cfg(|c| c.alert_style.clone());
            anim_active = critical_visible(&readings, page);
            period = animation_period_ms(&alert_style);
            let sig = frame_signature(page, &readings);
            let anim_step: i64 = if anim_active {
                (now_ms() / period.max(1)) as i64
            } else {
                0
            };
            let phase_changed = anim_active && anim_step != self.last_anim_step;
            // Animation frames bypass the value signature — the phase IS the change.
            if phase_changed || sig != self.last_frame_sig {
                let mut rc = RenderConfig::from_page(page, &readings, Some(&alert_style));
                rc.anim_step = anim_step.max(0) as u64;
                let frame = render_frame(&rc);
                if let Err(e) = self.device.upload_with_dithering(&frame) {
                    self.emit(CoreEvent::WorkerError(e));
                }
                self.last_frame_sig = sig;
            }
            self.last_anim_step = if anim_active { anim_step } else { -1 };
        }

        // Next data deadline is absolute (last read + interval) — no drift.
        let elapsed = self.last_data_read_at.elapsed().as_millis() as u64;
        let mut next_delay = interval.saturating_sub(elapsed);
        if anim_active {
            // Animation frames land on the next phase boundary, but never
            // later than the data deadline — the update rate stays authoritative.
            let now_ms_v = now_ms();
            let until_boundary = period.saturating_sub(now_ms_v % period.max(1));
            next_delay = next_delay.min(until_boundary.max(25));
        }
        self.display_due = Some(Instant::now() + Duration::from_millis(next_delay));
    }

    fn poll_tick(&mut self) {
        self.poll_due = None;
        if self.running && self.device.is_connected() {
            let mut readings = self.sensors.read_all_sensors();
            let thresholds = self.with_cfg(|c| c.thresholds.clone());
            apply_severities(&mut readings, &thresholds);
            self.log_severity_changes(&readings);
            // Severity watchdog only: adopt fresh readings only when a
            // crossing on the page's selected sensors was detected.
            let sig = severity_signature(&readings, self.current_page().as_ref());
            if sig != self.last_severity_sig {
                self.cached = readings.clone();
                self.last_data_read_at = Instant::now();
                self.last_severity_sig = sig;
                let esig = readings_signature(&readings);
                self.maybe_emit_with_sig(readings, esig);
                self.display_due = Some(Instant::now() + Duration::from_millis(50));
            }
        }
        if self.running {
            let interval = self.with_cfg(|c| c.sensor_poll_interval);
            self.poll_due = Some(Instant::now() + Duration::from_millis(interval.max(200)));
        }
    }

    fn rotation_tick(&mut self) {
        let rotated = {
            let mut guard = self.cfg.lock().unwrap();
            let interval = guard.page_rotation_interval;
            if interval == 0 || guard.pages.len() <= 1 {
                None
            } else {
                let next = (guard.current_page_index + 1) % guard.pages.len();
                guard.current_page_index = next;
                let page = guard.pages[next].clone();
                // Adopt the page's brightness/interval (main.ts rotation).
                guard.brightness = page.brightness;
                guard.display_update_interval = page.display_update_interval;
                save_locked(&guard);
                Some((page.brightness, page.display_update_interval, next))
            }
        };
        if let Some((_brightness, _interval, idx)) = rotated {
            self.emit(CoreEvent::PageChange {
                current_page_index: idx,
            });
            // The frame signature covers the page config — force a redraw.
            self.last_frame_sig.clear();
            self.display_due = Some(Instant::now() + Duration::from_millis(50));
        }
        self.rotation_due = rotation_deadline(&self.cfg, Instant::now());
    }

    // ─── Sensor emission ─────────────────────────────────────────────

    fn maybe_emit(&mut self, readings: Vec<SensorReading>) {
        let sig = readings_signature(&readings);
        self.maybe_emit_with_sig(readings, sig);
    }

    fn maybe_emit_with_sig(&mut self, readings: Vec<SensorReading>, sig: String) {
        if sig == self.last_emit_sig {
            return;
        }
        let since = self.last_emit_at.elapsed();
        if since >= SENSOR_EMIT_MIN_INTERVAL {
            self.do_emit(readings, sig);
            return;
        }
        // Under the coalesce window — keep the latest snapshot, flush later.
        self.pending_emit = Some(readings);
        if self.emit_timer_deadline.is_none() {
            self.emit_timer_deadline = Some(self.last_emit_at + SENSOR_EMIT_MIN_INTERVAL);
        }
    }

    fn flush_emit(&mut self) {
        self.emit_timer_deadline = None;
        let Some(pending) = self.pending_emit.take() else { return };
        if self.last_emit_at.elapsed() < SENSOR_EMIT_MIN_INTERVAL {
            // Still inside the window — re-arm.
            self.pending_emit = Some(pending);
            self.emit_timer_deadline = Some(self.last_emit_at + SENSOR_EMIT_MIN_INTERVAL);
            return;
        }
        let sig = readings_signature(&pending);
        if sig != self.last_emit_sig {
            self.do_emit(pending, sig);
        }
    }

    fn do_emit(&mut self, readings: Vec<SensorReading>, sig: String) {
        self.last_emit_sig = sig;
        self.last_emit_at = Instant::now();
        if !self.emit_logged {
            self.emit_logged = true;
            eprintln!("[Worker] sensor stream active");
        }
        self.emit(CoreEvent::Sensors(readings));
    }

    fn log_severity_changes(&mut self, readings: &[SensorReading]) {
        for r in readings {
            let next = r.severity.unwrap_or(Severity::None);
            let prev = self
                .last_logged_severity
                .get(&r.id)
                .copied()
                .unwrap_or(Severity::None);
            if prev != next {
                self.last_logged_severity.insert(r.id.clone(), next);
                eprintln!(
                    "[Worker] severity {}: {:?} -> {:?}",
                    r.id, prev, next
                );
            }
        }
    }

    fn current_page(&self) -> Option<DisplayPage> {
        let guard = self.cfg.lock().unwrap();
        if guard.pages.is_empty() {
            return None;
        }
        guard
            .pages
            .get(guard.current_page_index)
            .or_else(|| guard.pages.first())
            .cloned()
    }
}

// ─── Signature helpers (pure) ───────────────────────────────────────

fn readings_signature(readings: &[SensorReading]) -> String {
    readings
        .iter()
        .map(|r| {
            format!(
                "{}={}|{}|{}|{}|{:?}",
                r.id,
                r.value,
                r.unit,
                opt_str(r.min),
                opt_str(r.max),
                r.severity
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn opt_str(v: Option<f64>) -> String {
    match v {
        Some(x) => x.to_string(),
        None => "undefined".into(),
    }
}

/// Severities of the current page's selected sensors (in selection order).
fn severity_signature(readings: &[SensorReading], page: Option<&DisplayPage>) -> String {
    let Some(page) = page else { return String::new() };
    page.selected_sensor_ids
        .iter()
        .map(|id| {
            readings
                .iter()
                .find(|r| &r.id == id)
                .and_then(|r| r.severity)
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "-".into())
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// True while any sensor selected on the page is in critical condition.
fn critical_visible(readings: &[SensorReading], page: &DisplayPage) -> bool {
    page.selected_sensor_ids.iter().any(|id| {
        readings
            .iter()
            .find(|r| &r.id == id)
            .is_some_and(|r| r.severity == Some(Severity::Critical))
    })
}

/// Page config + displayed values — unchanged → skip the frame upload.
fn frame_signature(page: &DisplayPage, readings: &[SensorReading]) -> String {
    let cfg = format!(
        "{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        page.name,
        page.layout,
        page.background_color.to_hex(),
        page.selected_sensor_ids.join(","),
        page.grid_rows,
        page.grid_cols,
        page.grid_label_position,
        page.value_position,
        page.category_bar_position,
        page.value_font,
        page.value_font_style,
        page.value_weight.map(|w| w.0)
    );
    let vals = page
        .selected_sensor_ids
        .iter()
        .map(|id| match readings.iter().find(|x| &x.id == id) {
            Some(r) => format!(
                "{}|{}|{}|{}|{:?}",
                r.value,
                r.unit,
                opt_str(r.min),
                opt_str(r.max),
                r.severity
            ),
            None => String::new(),
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("{cfg}||{vals}")
}

// ─── Shared helpers used by the loop ────────────────────────────────

fn rotation_deadline(cfg: &SharedConfig, now: Instant) -> Option<Instant> {
    let interval = cfg.lock().unwrap().page_rotation_interval;
    if interval == 0 {
        None
    } else {
        Some(now + Duration::from_millis(interval))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(id: &str, sev: Severity) -> SensorReading {
        SensorReading {
            id: id.into(),
            label: id.into(),
            category: SensorCategory::Cpu,
            value: 1.0,
            unit: "%".into(),
            min: Some(0.0),
            max: Some(100.0),
            icon: "x".into(),
            severity: Some(sev),
        }
    }

    #[test]
    fn severity_signature_follows_selection_order() {
        let page = DisplayPage {
            selected_sensor_ids: vec!["b".into(), "a".into()],
            ..DisplayPage::default()
        };
        let readings = vec![reading("a", Severity::Warning), reading("b", Severity::None)];
        assert_eq!(severity_signature(&readings, Some(&page)), "None,Warning");
        // Unselected / missing ids render as "-"
        let page2 = DisplayPage {
            selected_sensor_ids: vec!["zzz".into()],
            ..DisplayPage::default()
        };
        assert_eq!(severity_signature(&readings, Some(&page2)), "-");
        assert_eq!(severity_signature(&readings, None), "");
    }

    #[test]
    fn frame_signature_changes_with_values_and_config() {
        let page = DisplayPage::default();
        let r1 = vec![reading("cpu_usage", Severity::None)];
        let mut r2 = r1.clone();
        r2[0].value = 2.0;
        let s1 = frame_signature(&page, &r1);
        assert_ne!(s1, frame_signature(&page, &r2));
        // Config change also flips it
        let mut p2 = page.clone();
        p2.grid_cols = 4;
        assert_ne!(s1, frame_signature(&p2, &r1));
        // Identical inputs → identical signature
        assert_eq!(s1, frame_signature(&page, &r1));
    }

    #[test]
    fn critical_detection() {
        let mut page = DisplayPage::default();
        page.selected_sensor_ids = vec!["cpu_usage".into()];
        let readings = vec![reading("cpu_usage", Severity::Warning)];
        assert!(!critical_visible(&readings, &page));
        let readings = vec![reading("cpu_usage", Severity::Critical)];
        assert!(critical_visible(&readings, &page));
    }
}
