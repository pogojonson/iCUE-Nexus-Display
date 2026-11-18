//! Persistence: `~/.config/icue-nexus/pages.json`.
//! Wire-compatible with the Electron app — same keys, same schema.

use crate::core::thresholds::DEFAULT_ALERT_STYLE;
use crate::core::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Full persisted application config (mirrors the Electron `currentConfig`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub pages: Vec<DisplayPage>,
    pub current_page_index: usize,
    pub brightness: u8,
    pub display_update_interval: u64,
    pub auto_connect: bool,
    pub page_rotation_interval: u64,
    pub hide_to_tray_on_startup: bool,
    pub minimize_to_tray: bool,
    pub thresholds: ThresholdMap,
    pub alert_style: AlertStyle,
    pub macros: HashMap<String, Macro>,
    pub gkeys: HashMap<String, GKeyBinding>,
    pub gkey_mode: GkeyMode,
    pub gbit0: u32,
    pub gbits_calibrated: bool,

    /// Not persisted (matches original): always 2000 on restart.
    #[serde(skip)]
    pub sensor_poll_interval: u64,
}

/// Legacy GKeyBinding that may carry per-slot playback settings.
/// `action`/`speed` are accepted here (and stripped on rewrite) so old files
/// load unchanged; the values themselves are migrated in `migrate_legacy_bindings`.
#[derive(Deserialize)]
struct RawBinding {
    #[serde(rename = "macroId")]
    macro_id: String,
    #[allow(dead_code)]
    #[serde(default)]
    action: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    speed: Option<f64>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            pages: vec![DisplayPage {
                id: format!("page_{}", now_ms()),
                ..DisplayPage::default()
            }],
            current_page_index: 0,
            brightness: 50,
            display_update_interval: 1000,
            auto_connect: false,
            page_rotation_interval: 0,
            hide_to_tray_on_startup: false,
            minimize_to_tray: false,
            thresholds: HashMap::new(),
            alert_style: DEFAULT_ALERT_STYLE,
            macros: HashMap::new(),
            gkeys: HashMap::new(),
            gkey_mode: GkeyMode::Hw,
            gbit0: DEFAULT_GBIT0,
            gbits_calibrated: false,
            sensor_poll_interval: 2000,
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Config file location: `~/.config/icue-nexus/pages.json`.
///
/// Files written by the pre-rebrand build (`~/.config/k100display/pages.json`)
/// are migrated to this path on first load — see [`AppConfig::load`].
pub fn config_path() -> PathBuf {
    config_dir().join("pages.json")
}

/// `~/.config/icue-nexus`
fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config").join("icue-nexus")
}

/// `~/.config/k100display` — the pre-rebrand location, read once for migration.
fn legacy_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home)
        .join(".config")
        .join("k100display")
        .join("pages.json")
}

impl AppConfig {
    /// Load `pages.json` the way the TS loader did: parse the top level as
    /// untyped JSON, then extract each field independently with its own
    /// fallible conversion — one bad field never discards the rest of the
    /// file. Unknown/extra fields are ignored.
    pub fn load() -> AppConfig {
        let path = config_path();
        let mut raw = std::fs::read_to_string(&path);
        // Pre-rebrand builds persisted to ~/.config/k100display/pages.json:
        // if the new path is still empty, migrate the old file over (best
        // effort — even if the copy fails we keep reading the old bytes).
        if raw.is_err() {
            if let Ok(legacy) = std::fs::read_to_string(legacy_config_path()) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, &legacy);
                raw = Ok(legacy);
            }
        }
        let Ok(raw) = raw else {
            return AppConfig::default();
        };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return AppConfig::default();
        };
        let Some(data) = data.as_object() else {
            return AppConfig::default();
        };
        let mut cfg = AppConfig::default();

        // pages: keep only entries that parse; tolerate legacy pages that
        // miss newer fields (per-field serde defaults handle that now).
        if let Some(pages) = data.get("pages").and_then(|v| v.as_array()) {
            let parsed: Vec<DisplayPage> = pages
                .iter()
                .enumerate()
                .filter_map(|(i, v)| match serde_json::from_value::<DisplayPage>(v.clone()) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        // Never drop a page silently: a later save would
                        // persist the loss. Make it loud and attributable.
                        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        eprintln!(
                            "[config] FAILED to parse page #{i} ({name}): {e} — it will be missing until the file is fixed"
                        );
                        None
                    }
                })
                .collect();
            if !parsed.is_empty() && parsed.len() != pages.len() {
                eprintln!(
                    "[config] loaded {}/{} pages — the rest failed to parse",
                    parsed.len(),
                    pages.len()
                );
            }
            if !parsed.is_empty() {
                let idx = (data
                    .get("currentPageIndex")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize)
                    .min(parsed.len() - 1);
                cfg.current_page_index = idx;
                cfg.pages = parsed;
            }
        }
        if let Some(v) = data.get("brightness").and_then(|v| v.as_u64()) {
            cfg.brightness = (v as u8).min(100);
        }
        if let Some(v) = data.get("displayUpdateInterval").and_then(|v| v.as_u64()) {
            cfg.display_update_interval = v;
        }
        if let Some(v) = data.get("autoConnect").and_then(|v| v.as_bool()) {
            cfg.auto_connect = v;
        }
        if let Some(v) = data.get("pageRotationInterval").and_then(|v| v.as_u64()) {
            cfg.page_rotation_interval = v;
        }
        if let Some(v) = data.get("hideToTrayOnStartup").and_then(|v| v.as_bool()) {
            cfg.hide_to_tray_on_startup = v;
        }
        if let Some(v) = data.get("minimizeToTray").and_then(|v| v.as_bool()) {
            cfg.minimize_to_tray = v;
        }
        // thresholds: per-entry, keep what parses (broken sensor entries are
        // dropped individually, not the whole map).
        if let Some(obj) = data.get("thresholds").and_then(|v| v.as_object()) {
            for (id, v) in obj {
                if let Ok(t) = serde_json::from_value::<SensorThresholds>(v.clone()) {
                    cfg.thresholds.insert(id.clone(), t);
                }
            }
        }
        // alertStyle: merge over defaults so a partially-written object can't
        // lose a field (same as `{...DEFAULT_ALERT_STYLE, ...data.alertStyle}`).
        if let Some(obj) = data.get("alertStyle").and_then(|v| v.as_object()) {
            for (key, v) in obj {
                match key.as_str() {
                    "warningColor" => {
                        if let Ok(c) = serde_json::from_value::<RgbColor>(v.clone()) {
                            cfg.alert_style.warning_color = c;
                        }
                    }
                    "seriousColor" => {
                        if let Ok(c) = serde_json::from_value::<RgbColor>(v.clone()) {
                            cfg.alert_style.serious_color = c;
                        }
                    }
                    "criticalColor" => {
                        if let Ok(c) = serde_json::from_value::<RgbColor>(v.clone()) {
                            cfg.alert_style.critical_color = c;
                        }
                    }
                    "criticalAnimation" => {
                        if let Ok(a) = serde_json::from_value::<CriticalAnimation>(v.clone()) {
                            cfg.alert_style.critical_animation = a;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(macros) = data.get("macros").and_then(|v| v.as_object()) {
            for (id, m) in macros {
                if let Ok(parsed) = serde_json::from_value::<Macro>(m.clone()) {
                    if !parsed.events.is_empty() || parsed.kind == Some(MacroKind::Combo) {
                        cfg.macros.insert(id.clone(), parsed);
                    }
                }
            }
        }
        if let Some(gkeys) = data.get("gkeys").and_then(|v| v.as_object()) {
            for (slot, b) in gkeys {
                if let Ok(parsed) = serde_json::from_value::<RawBinding>(b.clone()) {
                    if cfg.macros.contains_key(&parsed.macro_id) {
                        cfg.gkeys
                            .insert(slot.clone(), GKeyBinding { macro_id: parsed.macro_id });
                    }
                }
            }
        }
        if let Some(m) = data
            .get("gkeyMode")
            .and_then(|v| serde_json::from_value::<GkeyMode>(v.clone()).ok())
        {
            cfg.gkey_mode = m;
        }
        if let Some(v) = data.get("gbit0").and_then(|v| v.as_u64()) {
            cfg.gbit0 = v as u32;
        }
        if let Some(v) = data.get("gbitsCalibrated").and_then(|v| v.as_bool()) {
            cfg.gbits_calibrated = v;
        }
        cfg
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
    }

    /// Migrate legacy per-slot playback settings (binding.action/speed) onto the
    /// macro itself — first binding wins when one macro is bound to several slots.
    /// Returns true if anything changed (caller saves).
    pub fn migrate_legacy_bindings(&mut self) -> bool {
        // Legacy fields were already dropped during load; if the raw file still
        // has them we re-read it to salvage the values onto macros.
        let Ok(raw) = std::fs::read_to_string(config_path()) else {
            return false;
        };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return false;
        };
        let Some(gkeys) = data.get("gkeys").and_then(|v| v.as_object()) else {
            return false;
        };
        let mut changed = false;
        for (_slot, b) in gkeys {
            let Some(macro_id) = b.get("macroId").and_then(|v| v.as_str()) else { continue };
            if let Some(action) = b.get("action").and_then(|v| v.as_str()) {
                if let Some(m) = self.macros.get_mut(macro_id) {
                    if m.action.is_none() {
                        m.action = Some(match action {
                            "repeat" | "toggle" => serde_json::from_str(&format!("\"{action}\""))
                                .unwrap_or(GKeyAction::Once),
                            _ => GKeyAction::Once,
                        });
                    }
                }
                changed = true;
            }
            if let Some(speed) = b.get("speed").and_then(|v| v.as_f64()) {
                if let Some(m) = self.macros.get_mut(macro_id) {
                    if m.speed.is_none() {
                        m.speed = Some(speed.clamp(0.25, 4.0));
                    }
                }
                changed = true;
            }
        }
        if changed {
            // Strip legacy keys from the file by re-saving without them.
            let _ = self.save();
        }
        changed
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let cfg = AppConfig::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        // Keys must match the Electron writer exactly.
        for key in [
            "pages",
            "currentPageIndex",
            "brightness",
            "displayUpdateInterval",
            "autoConnect",
            "pageRotationInterval",
            "hideToTrayOnStartup",
            "minimizeToTray",
            "thresholds",
            "alertStyle",
            "macros",
            "gkeys",
            "gkeyMode",
            "gbit0",
            "gbitsCalibrated",
        ] {
            assert!(json.contains(key), "missing key {key}");
        }
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pages.len(), 1);
        assert_eq!(back.gbit0, DEFAULT_GBIT0);
        assert_eq!(back.alert_style, DEFAULT_ALERT_STYLE);
    }

    #[test]
    fn page_serde_matches_electron_shape() {
        let page = DisplayPage::default();
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(v["backgroundColor"]["r"], 0);
        assert_eq!(v["layout"], "dynamic");
        assert_eq!(v["gridLabelPosition"], "top");
        assert_eq!(v["valuePosition"], "center");
        assert_eq!(v["categoryBarPosition"], "bottom");
        // Optional fields omitted when None (as JSON.stringify would)
        assert!(v.get("valueFont").is_none());
    }

    #[test]
    fn loads_legacy_pages_missing_newer_fields() {
        // Real-world shape: pages written before valuePosition/valueFont
        // existed (and one carrying the stale showCategoryBar key).
        let sample = r#"{
  "pages": [
    {
      "id": "page_1790011581430",
      "name": "General",
      "backgroundColor": { "r": 0, "g": 0, "b": 0 },
      "layout": "sensor-grid",
      "selectedSensorIds": ["cpu_usage"],
      "brightness": 40,
      "displayUpdateInterval": 1000,
      "gridRows": 2,
      "gridCols": 3,
      "gridLabelPosition": "top",
      "categoryBarPosition": "bottom",
      "showCategoryBar": true
    },
    {
      "id": "page_1790025755881",
      "name": "Temps",
      "backgroundColor": { "r": 10, "g": 20, "b": 30 },
      "layout": "dynamic",
      "selectedSensorIds": ["cpu_temp_core1"],
      "brightness": 80,
      "displayUpdateInterval": 2000,
      "gridRows": 2,
      "gridCols": 4,
      "gridLabelPosition": "bottom",
      "categoryBarPosition": "top"
    }
  ],
  "currentPageIndex": 1,
  "brightness": 80,
  "displayUpdateInterval": 2000,
  "autoConnect": true,
  "pageRotationInterval": 0,
  "hideToTrayOnStartup": false,
  "minimizeToTray": false,
  "thresholds": { "cpu_usage": { "direction": ">=" } },
  "alertStyle": { "criticalAnimation": "nyan" },
  "macros": {},
  "gkeys": {},
  "gkeyMode": "hw",
  "gbit0": 147,
  "gbitsCalibrated": false
}"#;
        // Write to a temp config path by round-tripping through the same
        // per-field loader logic: parse like load() does (Value-based).
        let data: serde_json::Value = serde_json::from_str(sample).unwrap();
        let obj = data.as_object().unwrap();
        let pages = obj["pages"].as_array().unwrap();
        let parsed: Vec<DisplayPage> = pages
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        assert_eq!(parsed.len(), 2, "legacy pages must not be dropped");
        // Missing valuePosition → renderer default "center"
        assert_eq!(parsed[0].value_position, ValuePosition::Center);
        // Missing valueFont/style/weight → None (style defaults to bold at render)
        assert_eq!(parsed[0].value_font, None);
        assert_eq!(parsed[0].value_font_style, None);
        // Stale extra key ignored, present fields intact
        assert_eq!(parsed[0].name, "General");
        assert_eq!(parsed[0].brightness, 40);
        assert_eq!(parsed[1].value_position, ValuePosition::Center);
        // Partial alertStyle merges over defaults
        let anim = serde_json::from_value::<CriticalAnimation>(
            obj["alertStyle"]["criticalAnimation"].clone(),
        )
        .unwrap();
        assert_eq!(anim, CriticalAnimation::Nyan);
        // Broken threshold entry shape → only that entry dropped
        let t = serde_json::from_value::<SensorThresholds>(
            obj["thresholds"]["cpu_usage"].clone(),
        );
        assert!(t.is_err(), "incomplete threshold entry must not parse");
    }

    #[test]
    fn parses_sample_electron_file() {
        let sample = r#"{
  "pages": [    {
      "id": "page_1700000000000",
      "name": "Main",
      "backgroundColor": { "r": 10, "g": 20, "b": 30 },
      "layout": "sensor-grid",
      "selectedSensorIds": ["cpu_usage", "gpu_temp"],
      "brightness": 80,
      "displayUpdateInterval": 2000,
      "gridRows": 2,
      "gridCols": 2,
      "gridLabelPosition": "top-left",
      "valuePosition": "center",
      "categoryBarPosition": "bottom",
      "valueFont": "tiny5-thin",
      "valueFontStyle": "italic",
      "valueWeight": 1.5
    }
  ],
  "currentPageIndex": 0,
  "brightness": 80,
  "displayUpdateInterval": 2000,
  "autoConnect": true,
  "pageRotationInterval": 5000,
  "hideToTrayOnStartup": false,
  "minimizeToTray": true,
  "thresholds": {
    "cpu_usage": {
      "direction": ">=",
      "warning": { "enabled": true, "value": 70 },
      "serious": { "enabled": false, "value": 0 },
      "critical": { "enabled": true, "value": 95 }
    }
  },
  "alertStyle": {
    "warningColor": { "r": 255, "g": 212, "b": 0 },
    "seriousColor": { "r": 255, "g": 59, "b": 48 },
    "criticalColor": { "r": 255, "g": 32, "b": 32 },
    "criticalAnimation": "nyan"
  },
  "macros": {
    "macro_1": {
      "id": "macro_1",
      "name": "Copy",
      "created": 1700000000000,
      "events": [ { "dt": 0, "type": "key", "code": "KEY_LEFTCTRL", "value": 1 } ],
      "action": "once",
      "speed": 1.5,
      "kind": "macro"
    }
  },
  "gkeys": {
    "1": { "macroId": "macro_1" },
    "2": { "macroId": "does_not_exist" }
  },
  "gkeyMode": "sw",
  "gbit0": 150,
  "gbitsCalibrated": true
}"#;
        let raw: serde_json::Value = serde_json::from_str(sample).unwrap();
        // Simulate load() — top-level Value parse, then per-field extraction
        let obj = raw.as_object().unwrap();
        let pages_arr = obj["pages"].as_array().unwrap();
        assert_eq!(pages_arr.len(), 1);

        // Full structural parse
        let cfg_json = serde_json::from_str::<serde_json::Value>(sample).unwrap();
        let pages: Vec<DisplayPage> = cfg_json["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| serde_json::from_value(v.clone()).unwrap())
            .collect();
        assert_eq!(pages[0].value_font, Some(ValueFont::Tiny5Thin));
        assert_eq!(pages[0].value_font_style, Some(ValueFontStyle::Italic));
        assert_eq!(pages[0].value_weight, Some(ValueWeight(1.5)));
        assert_eq!(pages[0].grid_label_position, LabelPosition::TopLeft);
        let th: ThresholdMap =
            serde_json::from_value(cfg_json["thresholds"].clone()).unwrap();
        assert!(th["cpu_usage"].critical.enabled);
        let alert: AlertStyle = serde_json::from_value(cfg_json["alertStyle"].clone()).unwrap();
        assert_eq!(alert.critical_animation, CriticalAnimation::Nyan);
        let macros: HashMap<String, Macro> =
            serde_json::from_value(cfg_json["macros"].clone()).unwrap();
        assert_eq!(macros["macro_1"].speed, Some(1.5));
        assert_eq!(macros["macro_1"].kind, Some(MacroKind::Macro));
        assert_eq!(macros["macro_1"].events[0].kind, MacroEventKind::Key);
        let mode: GkeyMode = serde_json::from_value(cfg_json["gkeyMode"].clone()).unwrap();
        assert_eq!(mode, GkeyMode::Sw);
    }
}
