//! Shared types for the Nexus display application.
//! Wire-compatible with the original Electron app's `pages.json` and IPC payloads.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Device / connection ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NexusDeviceInfo {
    pub device_id: String,
    pub model: String,
    pub firmware_version: String,
    pub serial_number: String,
    pub display_width: u16,
    pub display_height: u16,
    pub bytes_per_pixel: u8,
    pub connected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl ConnectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionStatus::Disconnected => "disconnected",
            ConnectionStatus::Connecting => "connecting",
            ConnectionStatus::Connected => "connected",
            ConnectionStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RgbColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        RgbColor { r, g, b }
    }
    /// Relative luminance used for contrast decisions (0.299R+0.587G+0.114B).
    pub fn luminance(&self) -> f32 {
        0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32
    }
    /// Black text on light backgrounds, white on dark.
    #[allow(dead_code)] // used by previews / tests
    pub fn contrast_text(&self) -> [u8; 3] {
        if self.luminance() > 128.0 {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        }
    }
    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
    #[allow(dead_code)]
    pub fn from_hex(s: &str) -> Option<RgbColor> {
        let s = s.trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(RgbColor { r, g, b })
    }
    /// Blend this color toward `other` by `t` (0..=1).
    pub fn blend(&self, other: &RgbColor, t: f32) -> RgbColor {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        RgbColor {
            r: l(self.r, other.r),
            g: l(self.g, other.g),
            b: l(self.b, other.b),
        }
    }
}

// ─── Sensors ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorCategory {
    Cpu,
    Gpu,
    Memory,
    Disk,
    Network,
    Motherboard,
    System,
}

/// Static UI definition (labels/icons for the sensor picker).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorDefinition {
    pub id: &'static str,
    pub label: &'static str,
    pub category: SensorCategory,
    pub unit: &'static str,
    pub icon: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SensorReading {
    pub id: String,
    pub label: String,
    pub category: SensorCategory,
    pub value: f64,
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub icon: String,
    /// Attached after threshold evaluation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

// ─── Threshold / severity types ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    None,
    Warning,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CriticalAnimation {
    ValueOnly,
    Tile,
    Both,
    Rainbow,
    Crazy,
    Nyan,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ThresholdLevel {
    pub enabled: bool,
    pub value: f64,
}

impl Default for ThresholdLevel {
    fn default() -> Self {
        ThresholdLevel {
            enabled: false,
            value: 0.0,
        }
    }
}

/// Per-sensor thresholds. `direction`: GreaterEq → higher is worse, LessEq → lower is worse.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SensorThresholds {
    pub direction: ThresholdDirection,
    pub warning: ThresholdLevel,
    pub serious: ThresholdLevel,
    pub critical: ThresholdLevel,
}

impl Default for SensorThresholds {
    fn default() -> Self {
        SensorThresholds {
            direction: ThresholdDirection::GreaterEq,
            warning: ThresholdLevel::default(),
            serious: ThresholdLevel::default(),
            critical: ThresholdLevel::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThresholdDirection {
    #[serde(rename = ">=")]
    GreaterEq,
    #[serde(rename = "<=")]
    LessEq,
}

pub type ThresholdMap = HashMap<String, SensorThresholds>;

/// Global alert styling: level colors + how critical animates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AlertStyle {
    pub warning_color: RgbColor,
    pub serious_color: RgbColor,
    pub critical_color: RgbColor,
    pub critical_animation: CriticalAnimation,
}

impl Default for AlertStyle {
    fn default() -> Self {
        AlertStyle {
            warning_color: RgbColor::new(255, 212, 0),
            serious_color: RgbColor::new(255, 59, 48),
            critical_color: RgbColor::new(255, 32, 32),
            critical_animation: CriticalAnimation::ValueOnly,
        }
    }
}

// ─── Display pages ──────────────────────────────────────────────────

/// Where a sensor label is placed inside its tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelPosition {
    #[serde(rename = "top")]
    Top,
    #[serde(rename = "bottom")]
    Bottom,
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-right")]
    BottomRight,
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    #[serde(rename = "hidden")]
    Hidden,
}

/// Where the value text is placed inside its tile (adds `center`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValuePosition {
    #[serde(rename = "center")]
    Center,
    #[serde(rename = "top")]
    Top,
    #[serde(rename = "bottom")]
    Bottom,
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-right")]
    BottomRight,
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    #[serde(rename = "hidden")]
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CategoryBarPosition {
    #[serde(rename = "top")]
    Top,
    #[serde(rename = "bottom")]
    Bottom,
    #[serde(rename = "hidden")]
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueFont {
    #[serde(rename = "classic")]
    Classic,
    #[serde(rename = "segment")]
    Segment,
    #[serde(rename = "retro8")]
    Retro8,
    #[serde(rename = "tiny5")]
    Tiny5,
    #[serde(rename = "classic-thin")]
    ClassicThin,
    #[serde(rename = "segment-thin")]
    SegmentThin,
    #[serde(rename = "retro8-thin")]
    Retro8Thin,
    #[serde(rename = "tiny5-thin")]
    Tiny5Thin,
}

impl Default for ValueFont {
    fn default() -> Self {
        ValueFont::Classic
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueFontStyle {
    #[serde(rename = "regular")]
    Regular,
    #[serde(rename = "bold")]
    Bold,
    #[serde(rename = "italic")]
    Italic,
    #[serde(rename = "bold-italic")]
    BoldItalic,
    #[serde(rename = "underline")]
    Underline,
    #[serde(rename = "shadow")]
    Shadow,
}

impl Default for ValueFontStyle {
    fn default() -> Self {
        ValueFontStyle::Bold // historical default
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ValueWeight(pub f32);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DisplayPage {
    // Every field carries a serde default mirroring the TS renderer's
    // `value || fallback` tolerance — legacy/partial pages (e.g. written by
    // older versions without `valuePosition`) must keep loading instead of
    // being dropped wholesale.
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "def_bg_color")]
    pub background_color: RgbColor,
    #[serde(default = "def_layout")]
    pub layout: String,
    #[serde(default)]
    pub selected_sensor_ids: Vec<String>,
    #[serde(default = "def_brightness")]
    pub brightness: u8,
    #[serde(default = "def_interval")]
    pub display_update_interval: u64,
    #[serde(default = "def_rows")]
    pub grid_rows: usize,
    #[serde(default = "def_cols")]
    pub grid_cols: usize,
    #[serde(default = "def_label_pos")]
    pub grid_label_position: LabelPosition,
    #[serde(default = "def_value_pos")]
    pub value_position: ValuePosition,
    #[serde(default = "def_bar_pos")]
    pub category_bar_position: CategoryBarPosition,
    /// Value-field typeface (embedded bitmap fonts). Default `classic`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_font: Option<ValueFont>,
    /// Value-field type style. Default `bold` (historical behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_font_style: Option<ValueFontStyle>,
    /// Extra stroke thickness in device px (−1…+3, half-steps); `None` → derived from style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_weight: Option<ValueWeight>,
}

// ── serde defaults for DisplayPage (legacy-page tolerance) ──────────────
fn def_bg_color() -> RgbColor {
    RgbColor::new(0, 0, 0)
}
fn def_layout() -> String {
    "dynamic".into()
}
fn def_brightness() -> u8 {
    50
}
fn def_interval() -> u64 {
    1000
}
fn def_rows() -> usize {
    2
}
fn def_cols() -> usize {
    3
}
fn def_label_pos() -> LabelPosition {
    LabelPosition::Top
}
fn def_value_pos() -> ValuePosition {
    ValuePosition::Center
}
fn def_bar_pos() -> CategoryBarPosition {
    CategoryBarPosition::Bottom
}

impl Default for DisplayPage {
    fn default() -> Self {
        DisplayPage {
            id: "page_default".into(),
            name: "Page 1".into(),
            background_color: def_bg_color(),
            layout: def_layout(),
            selected_sensor_ids: vec![
                "cpu_usage".into(),
                "amd_gpu_temp1".into(),
                "mem_used_pct".into(),
            ],
            brightness: 50,
            display_update_interval: 1000,
            grid_rows: 2,
            grid_cols: 3,
            grid_label_position: LabelPosition::Top,
            value_position: ValuePosition::Center,
            category_bar_position: CategoryBarPosition::Bottom,
            value_font: None,
            value_font_style: None,
            value_weight: None,
        }
    }
}

// ─── HID protocol constants ────────────────────────────────────────

pub const NEXUS_VID: u16 = 0x1b1c;
pub const NEXUS_PID: u16 = 0x1b8e;
pub const NEXUS_USAGE_PAGE: u16 = 0x0c;
pub const NEXUS_USAGE: u16 = 0x01;

pub const NEXUS_WIDTH: usize = 640;
pub const NEXUS_HEIGHT: usize = 48;
pub const NEXUS_BPP: usize = 4; // BGRA on the wire

pub const HID_REPORT_OUTPUT_IMAGE: u8 = 2;
#[allow(dead_code)] // touch protocol parity (currently disabled upstream)
pub const HID_REPORT_INPUT_TOUCH: u8 = 1;
pub const HID_REPORT_FEATURE_CMD: u8 = 3;

pub const IMAGE_BLOCK_SIZE: usize = 1024;
pub const IMAGE_BLOCK_PAYLOAD: usize = 1016;
pub const IMAGE_LAST_BLOCK_PAYLOAD: usize = 960;
pub const IMAGE_TOTAL_BLOCKS: usize = 121;
pub const IMAGE_TOTAL_BYTES: usize = NEXUS_WIDTH * NEXUS_HEIGHT * NEXUS_BPP; // 122880

/// Firmware animation ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationId(pub u8);
impl AnimationId {
    pub const IDLE: AnimationId = AnimationId(1);
    #[allow(dead_code)]
    pub const ALT: AnimationId = AnimationId(2);
    #[allow(dead_code)]
    pub const UPDATE: AnimationId = AnimationId(3);
}

// ─── Macros (K100 keyboard) ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacroEventKind {
    Key,
    Rel,
}

/// One recorded evdev event. `dt` = ms since the previous event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MacroEvent {
    pub dt: u64,
    #[serde(rename = "type")]
    pub kind: MacroEventKind,
    pub code: String,
    pub value: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GKeyAction {
    #[default]
    Once,
    Repeat,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacroKind {
    Macro,
    Combo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Macro {
    pub id: String,
    pub name: String,
    pub events: Vec<MacroEvent>,
    pub created: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<MacroKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<GKeyAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GKeyBinding {
    pub macro_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GkeyMode {
    #[default]
    Hw,
    Sw,
}

pub const GKEY_COUNT: usize = 6;
pub const DEFAULT_GBIT0: u32 = 147;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MacroStatus {
    pub running: bool,
    pub ready: bool,
    pub mode: Option<GkeyMode>,
    pub recording: bool,
    pub playing: bool,
    pub active_slot: Option<usize>,
    pub gbit0: u32,
    pub gbits_calibrated: bool,
    #[serde(default)]
    pub ifaces: HashMap<String, Option<String>>,
    pub evdev: Option<String>,
    pub last_error: Option<String>,
}

// ─── Display layouts (mirrors src/constants.ts) ────────────────────

#[derive(Debug, Clone, Copy)]
pub struct DisplayLayoutDef {
    pub value: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub max_sensors: usize,
}

pub const DISPLAY_LAYOUTS: [DisplayLayoutDef; 2] = [
    DisplayLayoutDef {
        value: "sensor-grid",
        label: "Grid",
        description: "Configurable grid of tiles",
        max_sensors: 16,
    },
    DisplayLayoutDef {
        value: "dynamic",
        label: "Dynamic",
        description: "Flexible columns sized to fit all selected sensors",
        max_sensors: 20,
    },
];

pub const CATEGORY_LABELS: [(SensorCategory, &str); 7] = [
    (SensorCategory::Cpu, "CPU"),
    (SensorCategory::Gpu, "GPU"),
    (SensorCategory::Memory, "Memory"),
    (SensorCategory::Disk, "Disk"),
    (SensorCategory::Network, "Network"),
    (SensorCategory::Motherboard, "Motherboard"),
    (SensorCategory::System, "System"),
];

pub const CATEGORY_COLORS: [(SensorCategory, [u8; 3]); 7] = [
    (SensorCategory::Cpu, [255, 152, 0]),
    (SensorCategory::Gpu, [76, 175, 80]),
    (SensorCategory::Memory, [33, 150, 243]),
    (SensorCategory::Disk, [156, 39, 176]),
    (SensorCategory::Network, [0, 188, 212]),
    (SensorCategory::Motherboard, [244, 67, 54]),
    (SensorCategory::System, [96, 125, 139]),
];

pub fn category_color(c: SensorCategory) -> [u8; 3] {
    CATEGORY_COLORS
        .iter()
        .find(|(cat, _)| *cat == c)
        .map(|(_, col)| *col)
        .unwrap_or([96, 125, 139])
}

pub const ANIMATION_LABELS: [(CriticalAnimation, &str); 6] = [
    (CriticalAnimation::ValueOnly, "Value text only"),
    (CriticalAnimation::Tile, "Whole tile"),
    (CriticalAnimation::Both, "Both"),
    (CriticalAnimation::Rainbow, "Rainbow"),
    (CriticalAnimation::Crazy, "Crazy"),
    (CriticalAnimation::Nyan, "Nyan"),
];

pub const SEVERITY_LABELS: [(Severity, &str); 4] = [
    (Severity::None, "OK"),
    (Severity::Warning, "Warning"),
    (Severity::Serious, "Serious"),
    (Severity::Critical, "Critical"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_hex_roundtrip() {
        let c = RgbColor::new(0x1b, 0x2c, 0x3d);
        assert_eq!(RgbColor::from_hex(&c.to_hex()), Some(c));
        assert_eq!(RgbColor::from_hex("#ff0000"), Some(RgbColor::new(255, 0, 0)));
        assert_eq!(RgbColor::from_hex("zzz"), None);
    }

    #[test]
    fn contrast() {
        assert_eq!(RgbColor::new(255, 255, 255).contrast_text(), [0, 0, 0]);
        assert_eq!(RgbColor::new(0, 0, 0).contrast_text(), [255, 255, 255]);
    }

    #[test]
    fn image_math() {
        assert_eq!(IMAGE_TOTAL_BYTES, 122_880);
        // Payload across all blocks: 120 full blocks + the short last block.
        assert_eq!(
            (IMAGE_TOTAL_BLOCKS - 1) * IMAGE_BLOCK_PAYLOAD + IMAGE_LAST_BLOCK_PAYLOAD,
            IMAGE_TOTAL_BYTES
        );
        // Every block is written as a full 1024-byte report (last one
        // zero-padded after its 960-byte payload), like device.ts.
        assert_eq!(IMAGE_TOTAL_BLOCKS * IMAGE_BLOCK_SIZE, 123_904);
    }
}
