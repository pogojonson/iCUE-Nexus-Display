//! Application state (the Rust equivalent of the Redux store + App.tsx glue).
//!
//! Unlike the Electron app, the persisted config lives in `SharedConfig`
//! (`Arc<Mutex<AppConfig>>)`, so the UI edits it in place and pushes
//! `WorkerMsg::Reload`. This module only holds *transient* UI state: live
//! sensor readings, connection status, macro runtime status, dialog event
//! queues, and navigation.

use crate::core::events::{CoreEvent, LogLevel, MacroCmd, MacroState, WorkerMsg};
use crate::core::types::*;
use crossbeam_channel::{Receiver, Sender};
use std::time::Duration;

/// Payload of a `CoreEvent::RecStopped`.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // count/reason mirror the IPC payload for debugging
pub struct RecStoppedPayload {
    pub ok: bool,
    pub count: usize,
    pub err: Option<String>,
    pub reason: Option<String>,
    pub events: Vec<MacroEvent>,
}

pub struct AppState {
    pub cfg: crate::core::SharedConfig,

    // ── channels ──
    pub worker_tx: Sender<WorkerMsg>,
    pub macro_tx: Sender<MacroCmd>,
    pub event_rx: Receiver<CoreEvent>,

    // ── connection ──
    pub connection: ConnectionStatus,
    pub device_info: Option<NexusDeviceInfo>,
    pub connect_error: Option<String>,

    // ── sensors ──
    pub sensors: Vec<SensorReading>,

    // ── navigation (Redux deviceSlice parity) ──
    pub editing_page_id: Option<String>,
    pub previous_active_page_index: Option<usize>,

    // ── macro runtime (persisted parts are read from `cfg`) ──
    pub macro_state: MacroState,
    pub macro_status: Option<MacroStatus>,
    pub pressed_g: [bool; GKEY_COUNT],
    pub macro_last_error: Option<String>,

    // ── dialog event queues (consumed by the open dialog) ──
    pub rec_started: Option<(bool, Option<String>)>,
    pub rec_events: Vec<MacroEvent>,
    pub rec_stopped: Option<RecStoppedPayload>,
    pub ifc2_packets: Vec<(f64, String)>,
    pub macro_logs: Vec<(LogLevel, String)>,

    // ── lifecycle ──
    pub quitting: bool,
    pub auto_connect_sent: bool,
}

impl AppState {
    pub fn new(
        cfg: crate::core::SharedConfig,
        worker_tx: Sender<WorkerMsg>,
        macro_tx: Sender<MacroCmd>,
        event_rx: Receiver<CoreEvent>,
    ) -> Self {
        AppState {
            cfg,
            worker_tx,
            macro_tx,
            event_rx,
            connection: ConnectionStatus::Disconnected,
            device_info: None,
            connect_error: None,
            sensors: Vec::new(),
            editing_page_id: None,
            previous_active_page_index: None,
            macro_state: MacroState::default(),
            macro_status: None,
            pressed_g: [false; GKEY_COUNT],
            macro_last_error: None,
            rec_started: None,
            rec_events: Vec::new(),
            rec_stopped: None,
            ifc2_packets: Vec::new(),
            macro_logs: Vec::new(),
            quitting: false,
            auto_connect_sent: false,
        }
    }

    // ─── Event pump (called every frame) ────────────────────────────

    /// Drain pending worker/macro events into transient state.
    /// Returns true when anything changed (caller may skip repaint logic).
    pub fn pump_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.event_rx.try_recv() {
            changed = true;
            match ev {
                CoreEvent::Connection(s) => {
                    self.connection = s;
                    if s != ConnectionStatus::Error {
                        self.connect_error = None;
                    }
                    if s == ConnectionStatus::Disconnected || s == ConnectionStatus::Error {
                        self.device_info = None;
                    }
                }
                CoreEvent::DeviceInfo(info) => {
                    if info.is_none() && self.connection == ConnectionStatus::Error {
                        self.connect_error =
                            Some("Device not found — is it plugged in and the udev rule installed?".into());
                    }
                    self.device_info = info;
                }
                CoreEvent::Sensors(readings) => self.sensors = readings,
                CoreEvent::PageChange {
                    current_page_index: _,
                } => {
                    // The worker already wrote index/brightness/interval into
                    // SharedConfig — this event only triggers a repaint.
                }
                CoreEvent::WorkerError(e) => {
                    eprintln!("[worker] {e}");
                    self.connect_error = Some(e);
                }
                CoreEvent::MacroState(state) => self.macro_state = *state,
                CoreEvent::MacroStatus(status) => {
                    if status.running {
                        self.pressed_g = [false; GKEY_COUNT];
                    }
                    self.macro_last_error = status.last_error.clone();
                    self.macro_status = Some(*status);
                }
                CoreEvent::GKey { g, down } => {
                    if (1..=GKEY_COUNT).contains(&g) {
                        self.pressed_g[g - 1] = down;
                    }
                }
                CoreEvent::RecStarted { ok, err } => {
                    self.rec_started = Some((ok, err));
                }
                CoreEvent::RecEvent(e) => self.rec_events.push(e),
                CoreEvent::RecStopped {
                    ok,
                    count,
                    err,
                    reason,
                    events,
                } => {
                    self.rec_stopped = Some(RecStoppedPayload {
                        ok,
                        count,
                        err,
                        reason,
                        events,
                    });
                }
                CoreEvent::Ifc2 { t, hex } => self.ifc2_packets.push((t, hex)),
                CoreEvent::MacroLog { level, msg } => {
                    self.macro_logs.push((level, msg));
                    if self.macro_logs.len() > 200 {
                        self.macro_logs.drain(0..100);
                    }
                }
                CoreEvent::MacroNotice { ok, msg } => {
                    if !ok {
                        eprintln!("[macro] {msg}");
                    }
                }
            }
        }
        changed
    }

    // ─── Config mutation helpers ────────────────────────────────────

    pub fn send_macro(&self, cmd: MacroCmd) {
        let _ = self.macro_tx.send(cmd);
    }

    // ─── Page helpers (PageManager / PageEditor parity) ─────────────

    pub fn page_index_by_id(&self, id: &str) -> Option<usize> {
        let guard = self.cfg.lock().unwrap();
        guard.pages.iter().position(|p| p.id == id)
    }

    /// Add a page cloning page 0's settings (TS handleAddPage).
    pub fn add_page(&self) {
        let mut guard = self.cfg.lock().unwrap();
        if guard.pages.is_empty() {
            return;
        }
        let template = guard.pages[0].clone();
        let n = guard.pages.len() + 1;
        let mut page = template;
        page.id = format!("page_{}", crate::core::config::now_ms());
        page.name = format!("Page {n}");
        guard.pages.push(page);
        guard.current_page_index = guard.pages.len() - 1;
        crate::core::save_locked(&guard);
        drop(guard);
        let _ = self.worker_tx.send(WorkerMsg::Reload);
    }

    /// Remove a page; blocked when it's the last one. Returns success.
    pub fn remove_page(&self, id: &str) -> bool {
        let mut guard = self.cfg.lock().unwrap();
        if guard.pages.len() <= 1 {
            return false;
        }
        let Some(idx) = guard.pages.iter().position(|p| p.id == id) else {
            return false;
        };
        let removed = guard.pages[idx].name.clone();
        guard.pages.remove(idx);
        eprintln!("[config] page removed: \"{removed}\" ({})", guard.pages.len());
        if guard.current_page_index >= guard.pages.len() {
            guard.current_page_index = guard.pages.len() - 1;
        } else if guard.current_page_index > idx {
            guard.current_page_index -= 1;
        }
        crate::core::save_locked(&guard);
        drop(guard);
        let _ = self.worker_tx.send(WorkerMsg::Reload);
        true
    }

    /// Move a page up/down (TS reorderPages(from, to)).
    pub fn reorder_pages(&self, from: usize, to: usize) {
        let mut guard = self.cfg.lock().unwrap();
        let len = guard.pages.len();
        if from >= len || to >= len {
            return;
        }
        eprintln!("[config] page reordered: {from} -> {to}");
        let page = guard.pages.remove(from);
        guard.pages.insert(to, page);
        if guard.current_page_index == from {
            guard.current_page_index = to;
        } else if from < guard.current_page_index && to >= guard.current_page_index {
            guard.current_page_index -= 1;
        } else if from > guard.current_page_index && to <= guard.current_page_index {
            guard.current_page_index += 1;
        }
        crate::core::save_locked(&guard);
        drop(guard);
        let _ = self.worker_tx.send(WorkerMsg::Reload);
    }

    /// Make `index` the active page, adopting its brightness/interval
    /// (TS nexus:set-current-page semantics).
    pub fn set_current_page(&self, index: usize) {
        {
            let mut guard = self.cfg.lock().unwrap();
            if index >= guard.pages.len() {
                return;
            }
            guard.current_page_index = index;
            let page = guard.pages[index].clone();
            guard.brightness = page.brightness;
            guard.display_update_interval = page.display_update_interval;
            crate::core::save_locked(&guard);
        }
        let _ = self.worker_tx.send(WorkerMsg::Reload);
    }

    /// Edit a page by id (TS updatePage) — returns the updated page.
    pub fn update_page<F>(&self, id: &str, f: F) -> Option<DisplayPage>
    where
        F: FnOnce(&mut DisplayPage),
    {
        let mut guard = self.cfg.lock().unwrap();
        let page = guard.pages.iter_mut().find(|p| p.id == id)?;
        f(page);
        let updated = page.clone();
        crate::core::save_locked(&guard);
        drop(guard);
        let _ = self.worker_tx.send(WorkerMsg::Reload);
        Some(updated)
    }

    // ─── Enter / leave the editor (App.tsx handleEditPage/Back) ─────

    pub fn enter_editor(&mut self, page_id: String) {
        let target = self.page_index_by_id(&page_id);
        let current = self.cfg.lock().unwrap().current_page_index;
        if let Some(target) = target {
            if target != current {
                self.previous_active_page_index = Some(current);
                self.set_current_page(target);
            }
            self.editing_page_id = Some(page_id);
        }
    }

    pub fn leave_editor(&mut self) {
        if let Some(prev) = self.previous_active_page_index.take() {
            self.set_current_page(prev);
        }
        self.editing_page_id = None;
    }

    // ─── Derived helpers for the UI ─────────────────────────────────

    pub fn is_connected(&self) -> bool {
        self.connection == ConnectionStatus::Connected
    }

    pub fn reading(&self, id: &str) -> Option<&SensorReading> {
        self.sensors.iter().find(|r| r.id == id)
    }

    /// Readings for a page in its selection order (what the display shows).
    pub fn selected_readings(&self, page: &DisplayPage) -> Vec<SensorReading> {
        page.selected_sensor_ids
            .iter()
            .filter_map(|id| self.reading(id).cloned())
            .collect()
    }

    pub fn macro_by_id(&self, id: &str) -> Option<&Macro> {
        self.macro_state.macros.get(id)
    }

    pub fn has_bindings(&self) -> bool {
        !self.macro_state.gkeys.is_empty()
    }

    pub fn helper_offline(&self) -> bool {
        self.macro_status.as_ref().is_some_and(|s| !s.running)
    }

    pub fn recording(&self) -> bool {
        self.macro_status.as_ref().is_some_and(|s| s.recording)
    }

    pub fn playing(&self) -> bool {
        self.macro_status.as_ref().is_some_and(|s| s.playing)
    }

    /// Clear one-shot dialog queues (called when a dialog closes).
    pub fn clear_rec_queues(&mut self) {
        self.rec_started = None;
        self.rec_events.clear();
        self.rec_stopped = None;
    }

    pub fn clear_ifc2_queue(&mut self) {
        self.ifc2_packets.clear();
    }
}

/// Request-repaint cadence: keeps the event pump alive while idle so sensor
/// updates arrive promptly (the renderer never polls the UI).
pub const REPAINT_INTERVAL: Duration = Duration::from_millis(100);
