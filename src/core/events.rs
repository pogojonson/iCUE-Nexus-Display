//! Shared message contract between the three execution contexts:
//! UI thread (eframe) ← events — worker thread (HID + sensors)
//!                            ← events — macro manager thread (K100 keyboard)

use crate::core::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Macro subsystem state (mirrors the Electron `macro:state` payload).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MacroState {
    pub macros: HashMap<String, Macro>,
    pub gkeys: HashMap<String, GKeyBinding>,
    pub gkey_mode: GkeyMode,
    pub gbit0: u32,
    pub gbits_calibrated: bool,
}

impl Default for MacroState {
    fn default() -> Self {
        MacroState {
            macros: HashMap::new(),
            gkeys: HashMap::new(),
            gkey_mode: GkeyMode::Hw,
            gbit0: DEFAULT_GBIT0,
            gbits_calibrated: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Error,
}

/// Events flowing toward the UI thread.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    // ── display worker ──
    Connection(ConnectionStatus),
    DeviceInfo(Option<NexusDeviceInfo>),
    Sensors(Vec<SensorReading>),
    /// Rotation or explicit page switch — the worker already wrote the new
    /// index/brightness/interval into SharedConfig; this is a repaint trigger.
    PageChange {
        #[allow(dead_code)]
        current_page_index: usize,
    },
    WorkerError(String),

    // ── macro manager ──
    MacroState(Box<MacroState>),
    MacroStatus(Box<MacroStatus>),
    GKey { g: usize, down: bool },
    /// Result of `MacroCmd::RecordStart`.
    RecStarted { ok: bool, err: Option<String> },
    RecEvent(MacroEvent),
    /// Result of `MacroCmd::RecordStop` (also fires if the recording ends
    /// on its own, e.g. the recorder died or the watchdog tripped).
    RecStopped {
        ok: bool,
        count: usize,
        err: Option<String>,
        reason: Option<String>,
        events: Vec<MacroEvent>,
    },
    /// Raw IFC2 packet stream during G-key calibration (`hex` skips the first
    /// two header bytes, exactly like the Python helper emitted).
    Ifc2 { t: f64, hex: String },
    MacroLog { level: LogLevel, msg: String },
    /// Result of `MacroCmd::Play` / device (re)discovery, informational.
    MacroNotice { ok: bool, msg: String },
}

/// Commands UI → display worker.
#[derive(Debug, Clone)]
#[allow(dead_code)] // PlayAnimation/StopAnimation: firmware-animation parity
                     // (the original bridge exposed them; no UI control yet)
pub enum WorkerMsg {
    /// Config changed on disk/in UI: re-read shared config, re-apply
    /// brightness if changed, reschedule timers, re-evaluate thresholds,
    /// schedule a prompt render.
    Reload,
    /// Discover + connect (single step; emits Connection + DeviceInfo).
    Connect,
    Disconnect,
    PlayAnimation { anim: u8, run_loop: bool },
    StopAnimation,
    Shutdown,
}

/// Commands UI → macro manager thread.
#[derive(Debug, Clone)]
pub enum MacroCmd {
    SaveMacro(Macro),
    DeleteMacro(String),
    SetBinding(usize, Option<GKeyBinding>),
    ReleaseToHw,
    SetCalibrating(bool),
    RecordStart,
    RecordStop,
    Play {
        events: Vec<MacroEvent>,
        speed: f64,
        repeat: bool,
    },
    StopPlay,
    /// Stream raw IFC2 packets for N seconds (0 = stop).
    Watch(u64),
    SetGbit0(u32),
    #[allow(dead_code)] // TS macro:rescan parity (UI relies on the monitor)
    Rescan,
    Shutdown,
}
