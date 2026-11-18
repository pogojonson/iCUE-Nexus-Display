//! Sensor multi-select with live values (`SensorSelector.tsx` equivalent).

use crate::core::definitions::SENSOR_DEFINITIONS;
use crate::core::types::*;
use crate::state::AppState;
use crate::ui::widgets;
use eframe::egui::{self, Color32, RichText};

pub struct SensorSelectorState {
    pub search: String,
}

impl Default for SensorSelectorState {
    fn default() -> Self {
        SensorSelectorState {
            search: String::new(),
        }
    }
}

/// Renders the selector. Selection edits go through `on_change` so the
/// editor can apply them to the page + config.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    sel: &mut SensorSelectorState,
    selected: &[String],
    disabled: bool,
    on_change: &mut dyn FnMut(&mut AppState, Vec<String>),
) {
    let alert_style = state.cfg.lock().unwrap().alert_style.clone();

    // -- selected chips (deletable) -----------------------------------
    if !selected.is_empty() {
        ui.horizontal_wrapped(|ui| {
            let mut remove: Option<String> = None;
            for id in selected {
                let Some(def) = SENSOR_DEFINITIONS.iter().find(|d| d.id == id) else {
                    continue;
                };
                let color = widgets::category_rgb(def.category);
                let text_color = contrast_on(color);
                egui::Frame::none()
                    .fill(color)
                    .rounding(egui::Rounding::same(16.0))
                    .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{} {}", def.icon, def.label))
                                    .size(11.0)
                                    .color(text_color),
                            );
                            if !disabled
                                && ui
                                    .add(
                                        egui::Button::new(RichText::new("×").size(10.0).color(text_color))
                                            .frame(false),
                                    )
                                    .clicked()
                            {
                                remove = Some(id.clone());
                            }
                        });
                    });
            }
            if let Some(id) = remove {
                let next: Vec<String> = selected
                    .iter()
                    .filter(|x| *x != &id)
                    .cloned()
                    .collect();
                on_change(state, next);
            }
        });
        ui.add_space(6.0);
    }

    // -- search box ----------------------------------------------------
    ui.horizontal(|ui| {
        ui.label(RichText::new("🔍").size(13.0));
        ui.add(
            egui::TextEdit::singleline(&mut sel.search)
                .hint_text("Search sensors…")
                .desired_width(220.0),
        );
        if !sel.search.is_empty() && ui.button("×").clicked() {
            sel.search.clear();
        }
    });
    ui.add_space(4.0);

    let query = sel.search.to_lowercase();

    // Grid columns: measured once over ALL definitions (not just the
    // filtered visible ones) so the columns don't reflow while searching
    // and every row — across categories — starts its icon/label at the
    // same x. Previous flow layout shifted each label by its icon's width
    // (emoji glyphs vary) and by label length.
    let icon_w = SENSOR_DEFINITIONS
        .iter()
        .map(|d| widgets::text_width(ui, d.icon, 13.0))
        .fold(0.0, f32::max)
        + 2.0;
    let label_w = SENSOR_DEFINITIONS
        .iter()
        .map(|d| widgets::text_width(ui, d.label, 13.0))
        .fold(0.0, f32::max)
        + 6.0;
    // checkbox column width (box + margin)
    const CHECK_W: f32 = 24.0;

    // Grouped by category, in definition order.
    let mut categories: Vec<SensorCategory> = Vec::new();
    for def in SENSOR_DEFINITIONS.iter() {
        if !categories.contains(&def.category) {
            categories.push(def.category);
        }
    }

    let mut changed = false;
    let mut next = selected.to_vec();

    // Data-points list: tall enough to browse categories without much
    // scrolling — scales with the window (≈70% of viewport, clamped).
    //
    // min_scrolled_height is essential: inside the editor's outer
    // ScrollArea, available height at this cursor is ~0 (content below the
    // fold), and ScrollArea would collapse to its 64px default floor
    // regardless of max_height.
    let scroll_h = (ui.input(|i| i.screen_rect.height()) * 0.70).clamp(520.0, 900.0);
    egui::ScrollArea::vertical()
        .id_salt("sensor-selector-scroll")
        .max_height(scroll_h)
        .min_scrolled_height(scroll_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for cat in categories {
                let defs: Vec<_> = SENSOR_DEFINITIONS
                    .iter()
                    .filter(|d| d.category == cat && query_matches(d, &query))
                    .collect();
                if defs.is_empty() {
                    continue;
                }
                // Category caption colored per category
                let cat_label = category_label(cat);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(cat_label)
                        .strong()
                        .size(12.5)
                        .color(widgets::category_rgb(cat)),
                );
                for def in defs {
                    let is_selected = next.iter().any(|x| x == def.id);
                    let live = state.reading(def.id).cloned();

                    ui.horizontal(|ui| {
                        // Fixed columns: checkbox | icon | label | …value right
                        let mut checked = is_selected;
                        widgets::col(ui, CHECK_W, egui::Align::Min, |ui| {
                            ui.add_enabled(!disabled, egui::Checkbox::new(&mut checked, ""))
                                .on_hover_text(def.label);
                        });
                        widgets::col(ui, icon_w, egui::Align::Min, |ui| {
                            ui.label(RichText::new(def.icon).size(13.0));
                        });
                        widgets::col(ui, label_w, egui::Align::Min, |ui| {
                            ui.label(RichText::new(def.label).size(13.0));
                        });

                        // Right-aligned live value (right edge shared by all rows)
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if let Some(r) = &live {
                                    let sev_color = r
                                        .severity
                                        .and_then(|s| severity_color(s, &alert_style));
                                    let value_text = if r.id == "time" {
                                        r.unit.clone()
                                    } else {
                                        format!("{} {}", fmt_val(r.value), r.unit)
                                    };
                                    let mut text = RichText::new(value_text).size(12.0);
                                    if let Some(c) = sev_color {
                                        text = text.color(c).strong();
                                    } else if r.severity == Some(Severity::Warning)
                                        || r.severity == Some(Severity::Serious)
                                        || r.severity == Some(Severity::Critical)
                                    {
                                        text = text.color(Color32::WHITE);
                                    }
                                    ui.label(text);
                                }
                            },
                        );

                        if checked != is_selected {
                            changed = true;
                            if checked {
                                next.push(def.id.to_string());
                            } else {
                                next.retain(|x| x != def.id);
                            }
                        }
                    });
                }
            }
        });

    if changed {
        // Preserve SENSOR_DEFINITIONS order in the page's selection list.
        let ordered: Vec<String> = SENSOR_DEFINITIONS
            .iter()
            .filter(|d| next.iter().any(|x| x == d.id))
            .map(|d| d.id.to_string())
            .collect();
        on_change(state, ordered);
    }
}

fn query_matches(def: &SensorDefinition, query: &str) -> bool {
    query.is_empty()
        || def.label.to_lowercase().contains(query)
        || def.id.contains(query)
}

fn fmt_val(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.1}")
    }
}

fn severity_color(sev: Severity, alert: &AlertStyle) -> Option<Color32> {
    match sev {
        Severity::None => None,
        Severity::Warning => Some(widgets::rgb(alert.warning_color)),
        Severity::Serious => Some(widgets::rgb(alert.serious_color)),
        Severity::Critical => Some(widgets::rgb(alert.critical_color)),
    }
}

fn contrast_on(bg: Color32) -> Color32 {
    let lum = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if lum > 128.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn category_label(c: SensorCategory) -> &'static str {
    crate::core::types::CATEGORY_LABELS
        .iter()
        .find(|(cat, _)| *cat == c)
        .map(|(_, l)| *l)
        .unwrap_or("?")
}

/// Icon fallback: egui's bundled fonts cover every icon we use except a few.
/// Returns a substitute glyph when the platform fonts can't render the emoji.
#[allow(dead_code)]
pub fn icon_fallback(icon: &str) -> &str {
    match icon {
        "🌡️" => "♨", // thermometer — missing from NotoEmoji/emoji-icon-font
        other => other,
    }
}
