//! Thresholds & alert-style editor (`ThresholdsSection.tsx` equivalent).
//!
//! Thresholds are GLOBAL (apply to all pages) — severity evaluation happens
//! in the worker; this UI only edits the map and the alert style.

use crate::core::definitions::SENSOR_DEFINITIONS;
use crate::core::types::*;
use crate::state::AppState;
use crate::ui::widgets::{self, ButtonKind};
use eframe::egui::{self, Color32, RichText};

pub struct ThresholdsUi {
    pub open: bool,
    pub search: String,
}

impl Default for ThresholdsUi {
    fn default() -> Self {
        ThresholdsUi {
            open: true,
            search: String::new(),
        }
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, tui: &mut ThresholdsUi, disabled: bool) {
    let mut thresholds = state.cfg.lock().unwrap().thresholds.clone();
    let mut alert_style = state.cfg.lock().unwrap().alert_style.clone();
    let configured = thresholds.len();

    // ── Header ────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(RichText::new("Thresholds").strong().size(15.0));
        let (fill, tc) = if configured > 0 {
            (
                Color32::from_rgb(255, 160, 0),
                Color32::BLACK,
            )
        } else {
            (Color32::from_gray(60), Color32::from_gray(190))
        };
        let label = format!("{configured} configured");
        widgets::chip(ui, &label, Some(fill), tc);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let glyph = if tui.open { "▴" } else { "▾" };
            if ui.add(egui::Button::new(glyph).frame(false)).clicked() {
                tui.open = !tui.open;
            }
        });
    });

    if !tui.open {
        return;
    }
    ui.add_space(6.0);

    // ── Global alert style box ────────────────────────────────────────
    widgets::paper(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Alert style").strong().size(13.0));
            ui.add_space(6.0);

            let mut style_changed = false;
            style_changed |= color_field(ui, "Warning", &mut alert_style.warning_color);
            style_changed |= color_field(ui, "Serious", &mut alert_style.serious_color);
            style_changed |= color_field(ui, "Critical", &mut alert_style.critical_color);

            ui.add_space(6.0);
            ui.label(RichText::new("Critical animation").size(12.5));
            egui::ComboBox::from_id_salt("critical-animation")
                .selected_text(animation_label(alert_style.critical_animation))
                .show_ui(ui, |ui| {
                    for (anim, label) in ANIMATION_LABELS {
                        ui.selectable_value(
                            &mut alert_style.critical_animation,
                            anim,
                            label,
                        );
                    }
                });

            if configured > 0 {
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if widgets::button(ui, "Clear all", ButtonKind::Outlined, true).clicked() {
                            thresholds.clear();
                        }
                    },
                );
            }

            if style_changed {
                let mut guard = state.cfg.lock().unwrap();
                guard.alert_style = alert_style.clone();
                crate::core::save_locked(&guard);
                drop(guard);
                let _ = state.worker_tx.send(crate::core::events::WorkerMsg::Reload);
            }
        });
    });
    ui.add_space(6.0);

    // ── Search ─────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(RichText::new("🔍").size(13.0));
        ui.add(
            egui::TextEdit::singleline(&mut tui.search)
                .hint_text("Search sensors…")
                .desired_width(220.0),
        );
    });
    ui.add_space(4.0);

    let query = tui.search.to_lowercase();
    let readings_snapshot: Vec<SensorReading> = state.sensors.clone();

    // Grid columns shared by every threshold row (and by all three level
    // lines within a row) — measured once so icon/label/value/chip/field
    // columns start at the same x regardless of label length or emoji width.
    let cols = measure_cols(ui, &readings_snapshot);

    // ── Per-sensor rows grouped by category (excluding `time`) ────────
    let mut categories: Vec<SensorCategory> = Vec::new();
    for def in SENSOR_DEFINITIONS.iter() {
        if def.category != SensorCategory::System || def.id != "time" {
            if !categories.contains(&def.category) {
                categories.push(def.category);
            }
        }
    }

    let mut commit: Option<(String, Option<SensorThresholds>)> = None;

    // Thresholds list: the largest scrolling container in the editor —
    // ≈85% of viewport, clamped so small windows stay usable.
    // min_scrolled_height: see the note in sensors_sel.rs — without it the
    // outer editor ScrollArea collapses this to 64px.
    let scroll_h = (ui.input(|i| i.screen_rect.height()) * 0.85).clamp(640.0, 1100.0);
    egui::ScrollArea::vertical()
        .id_salt("thresholds-scroll")
        .max_height(scroll_h)
        .min_scrolled_height(scroll_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for cat in categories {
                let defs: Vec<_> = SENSOR_DEFINITIONS
                    .iter()
                    .filter(|d| {
                        d.id != "time"
                            && d.category == cat
                            && (query.is_empty()
                                || d.label.to_lowercase().contains(&query)
                                || d.id.contains(&query))
                    })
                    .collect();
                if defs.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(category_label(cat))
                        .strong()
                        .size(12.5)
                        .color(widgets::category_rgb(cat)),
                );

                for def in defs {
                    let reading = readings_snapshot.iter().find(|r| r.id == def.id).cloned();
                    let cfg = thresholds.get(def.id).cloned();
                    let is_configured = cfg.as_ref().is_some_and(|t| {
                        t.warning.enabled || t.serious.enabled || t.critical.enabled
                    });
                    threshold_row(
                        ui,
                        state,
                        def,
                        reading.as_ref(),
                        cfg,
                        &alert_style,
                        is_configured,
                        disabled,
                        &cols,
                        &mut commit,
                    );
                }
            }
        });

    if let Some((id, value)) = commit {
        let mut guard = state.cfg.lock().unwrap();
        match value {
            Some(t) => {
                guard.thresholds.insert(id, t);
            }
            None => {
                guard.thresholds.remove(&id);
            }
        }
        crate::core::save_locked(&guard);
        drop(guard);
        let _ = state.worker_tx.send(crate::core::events::WorkerMsg::Reload);
    }
}

/// Column widths shared by every threshold row so they read as a grid.
#[derive(Clone, Copy)]
struct GridCols {
    icon_w: f32,
    label_w: f32,
    value_w: f32,
    chip_w: f32,
    x_w: f32,
    sw_w: f32,
    level_w: f32,
    check_w: f32,
    field_w: f32,
    unit_w: f32,
}

/// Measure all grid columns once per frame (memoized font layout — cheap).
fn measure_cols(ui: &egui::Ui, readings: &[SensorReading]) -> GridCols {
    let widest = |items: &mut dyn Iterator<Item = f32>| items.fold(0.0, f32::max);
    GridCols {
        icon_w: widest(&mut SENSOR_DEFINITIONS
            .iter()
            .map(|d| widgets::text_width(ui, d.icon, 13.0)))
            + 2.0,
        label_w: widest(&mut SENSOR_DEFINITIONS
            .iter()
            .map(|d| widgets::text_width(ui, d.label, 13.0)))
            + 6.0,
        value_w: widest(&mut readings
            .iter()
            .map(|r| widgets::text_width(ui, &format!(" {}", fmt_num(r.value)), 12.5)))
            + 8.0,
        // chip = text + inner_margin symmetric(9,3) + border
        chip_w: widest(
            &mut SEVERITY_LABELS
                .iter()
                .map(|(_, l)| widgets::text_width(ui, l, 11.0))
                .chain(std::iter::once(widgets::text_width(ui, "ACTIVE", 11.0))),
        ) + 20.0,
        x_w: widgets::text_width(ui, "×", 12.0) + 24.0, // button frame padding
        sw_w: 16.0,
        level_w: widest(&mut SEVERITY_LABELS.iter().map(|(_, l)| {
            widgets::text_width(ui, l, 12.0)
        })) + 4.0,
        check_w: 24.0,
        field_w: 70.0,
        unit_w: widest(
            &mut SENSOR_DEFINITIONS
                .iter()
                .map(|d| widgets::text_width(ui, &unit_suffix(d), 11.0)),
        ) + 6.0,
    }
}

#[allow(clippy::too_many_arguments)]
fn threshold_row(
    ui: &mut egui::Ui,
    _state: &AppState,
    def: &SensorDefinition,
    reading: Option<&SensorReading>,
    cfg: Option<SensorThresholds>,
    alert_style: &AlertStyle,
    is_configured: bool,
    disabled: bool,
    cols: &GridCols,
    commit: &mut Option<(String, Option<SensorThresholds>)>,
) {
    let mut local = cfg.clone().unwrap_or_default();
    let severity = reading.and_then(|r| r.severity).unwrap_or(Severity::None);

    egui::Frame::none()
        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
        .rounding(egui::Rounding::same(3.0))
        .fill(if is_configured {
            Color32::from_gray(45)
        } else {
            Color32::TRANSPARENT
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            // ── Row 1: icon | label | value | severity chip | … | dir | ×
            // Left cells are fixed-width columns (same x on every row);
            // values right-align inside their cell so all value right edges
            // match too.
            ui.horizontal(|ui| {
                widgets::col(ui, cols.icon_w, egui::Align::Min, |ui| {
                    ui.label(RichText::new(def.icon).size(13.0));
                });
                widgets::col(ui, cols.label_w, egui::Align::Min, |ui| {
                    ui.label(RichText::new(def.label).size(13.0));
                });

                // Live value (cell always allocated so the chip column
                // below doesn't jump when a sensor has no reading)
                widgets::col(ui, cols.value_w, egui::Align::Max, |ui| {
                    if let Some(r) = reading {
                        let color = match severity {
                            Severity::None => Color32::from_gray(200),
                            Severity::Warning => widgets::rgb(alert_style.warning_color),
                            Severity::Serious => widgets::rgb(alert_style.serious_color),
                            Severity::Critical => widgets::rgb(alert_style.critical_color),
                        };
                        ui.label(
                            RichText::new(format!(" {}", fmt_num(r.value)))
                                .size(12.5)
                                .color(color)
                                .strong(),
                        );
                    }
                });

                // Severity chip cell (empty unless currently in severity)
                widgets::col(ui, cols.chip_w, egui::Align::Min, |ui| {
                    if reading.is_some() && severity != Severity::None {
                        let (fill, tc) = match severity {
                            Severity::Warning => (
                                widgets::rgb(alert_style.warning_color),
                                Color32::BLACK,
                            ),
                            Severity::Serious => (
                                widgets::rgb(alert_style.serious_color),
                                Color32::WHITE,
                            ),
                            _ => (widgets::rgb(alert_style.critical_color), Color32::WHITE),
                        };
                        let name = severity_label(severity);
                        widgets::chip(ui, name, Some(fill), tc);
                    }
                });

                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Delete (×) — fixed-width cell, empty when the row
                        // isn't configured, so the direction toggles always
                        // sit at the same x.
                        widgets::col(ui, cols.x_w, egui::Align::Max, |ui| {
                            if is_configured
                                && ui
                                    .add_enabled(
                                        !disabled,
                                        egui::Button::new(RichText::new("×").size(12.0)),
                                    )
                                    .on_hover_text("Remove thresholds")
                                    .clicked()
                            {
                                *commit = Some((def.id.to_string(), None));
                            }
                        });
                        // Direction toggles — allocated right-to-left
                        // (this Ui is RTL): rightmost-first gives the visual
                        // order [When:][≥][≤][×], matching MUI's ≥-before-≤
                        // toggle order (plain ui.horizontal would inherit the
                        // RTL layout and reverse the reading order).
                        let r_le = ui.selectable_label(
                            local.direction == ThresholdDirection::LessEq,
                            "≤ worse low",
                        );
                        let r_ge = ui.selectable_label(
                            local.direction == ThresholdDirection::GreaterEq,
                            "≥ worse high",
                        );
                        if r_ge.clicked() {
                            local.direction = ThresholdDirection::GreaterEq;
                        }
                        if r_le.clicked() {
                            local.direction = ThresholdDirection::LessEq;
                        }
                        ui.label(
                            RichText::new("When:").size(11.5).color(Color32::from_gray(170)),
                        );
                    },
                );
            });

            // ── Level editors: warning / serious / critical ─────────────
            let levels: [(Severity, &mut ThresholdLevel); 3] = [
                (Severity::Warning, &mut local.warning),
                (Severity::Serious, &mut local.serious),
                (Severity::Critical, &mut local.critical),
            ];
            let mut level_changed = false;
            for (level_sev, level) in levels {
                let level_color = match level_sev {
                    Severity::Warning => alert_style.warning_color,
                    Severity::Serious => alert_style.serious_color,
                    _ => alert_style.critical_color,
                };
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    // Color swatch (from alert style) — fixed cell
                    widgets::col(ui, cols.sw_w, egui::Align::Min, |ui| {
                        egui::Frame::none()
                            .fill(widgets::rgb(level_color))
                            .rounding(egui::Rounding::same(3.0))
                            .inner_margin(egui::Margin::same(3.0))
                            .show(ui, |_| {});
                    });
                    // Level name — fixed cell so Warning/Serious/Critical
                    // don't shift the checkbox and field between lines
                    widgets::col(ui, cols.level_w, egui::Align::Min, |ui| {
                        ui.label(RichText::new(severity_label(level_sev)).size(12.0));
                    });

                    widgets::col(ui, cols.check_w, egui::Align::Min, |ui| {
                        let mut enabled = level.enabled;
                        if ui
                            .add_enabled(!disabled, egui::Checkbox::new(&mut enabled, ""))
                            .changed()
                        {
                            level.enabled = enabled;
                            level_changed = true;
                        }
                    });

                    // Numeric threshold field (fixed 70px cell)
                    widgets::col(ui, cols.field_w, egui::Align::Min, |ui| {
                        let mut text = if level.enabled {
                            format_value_field(level.value)
                        } else {
                            String::new()
                        };
                        let resp = ui.add_enabled(
                            level.enabled && !disabled,
                            egui::TextEdit::singleline(&mut text)
                                .desired_width(cols.field_w)
                                .hint_text("value"),
                        );
                        if resp.changed() {
                            let cleaned = text.trim().trim_end_matches('%');
                            if cleaned.is_empty() {
                                level.value = 0.0;
                                level_changed = true;
                            } else if let Ok(v) = cleaned.parse::<f64>() {
                                level.value = v;
                                level_changed = true;
                            }
                        }
                    });
                    widgets::col(ui, cols.unit_w, egui::Align::Min, |ui| {
                        ui.label(
                            RichText::new(unit_suffix(def))
                                .size(11.0)
                                .color(Color32::from_gray(150)),
                        );
                    });

                    // Caption + ACTIVE chip (derived text, flows after the
                    // fixed columns — its left edge still lines up)
                    if level.enabled {
                        let dir = match local.direction {
                            ThresholdDirection::GreaterEq => "≥",
                            ThresholdDirection::LessEq => "≤",
                        };
                        ui.label(
                            RichText::new(format!("{dir} {} {}", fmt_num(level.value), def.unit))
                                .size(10.5)
                                .color(Color32::from_gray(140)),
                        );
                        if severity == level_sev {
                            widgets::chip(ui, "ACTIVE", Some(Color32::from_gray(90)), Color32::WHITE);
                        }
                    }
                });
            }

            if level_changed {
                *commit = Some((def.id.to_string(), Some(local)));
            }
        });
}

// ─── Small helpers ────────────────────────────────────────────────────

fn unit_suffix(def: &SensorDefinition) -> String {
    if def.unit.is_empty() {
        String::new()
    } else {
        def.unit.to_string()
    }
}

fn color_field(ui: &mut egui::Ui, label: &str, value: &mut RgbColor) -> bool {
    let mut changed = false;
    ui.label(RichText::new(label).size(12.5));
    let mut color = [Color32::from_rgb(value.r, value.g, value.b)];
    let resp = ui.color_edit_button_srgba(&mut color[0]);
    if resp.changed() {
        *value = RgbColor::new(color[0].r(), color[0].g(), color[0].b());
        changed = true;
    }
    changed
}

fn animation_label(a: CriticalAnimation) -> &'static str {
    ANIMATION_LABELS
        .iter()
        .find(|(anim, _)| *anim == a)
        .map(|(_, l)| *l)
        .unwrap_or("?")
}

fn severity_label(s: Severity) -> &'static str {
    SEVERITY_LABELS
        .iter()
        .find(|(sev, _)| *sev == s)
        .map(|(_, l)| *l)
        .unwrap_or("?")
}

fn category_label(c: SensorCategory) -> &'static str {
    CATEGORY_LABELS
        .iter()
        .find(|(cat, _)| *cat == c)
        .map(|(_, l)| *l)
        .unwrap_or("?")
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.1}")
    }
}

fn format_value_field(v: f64) -> String {
    fmt_num(v)
}
