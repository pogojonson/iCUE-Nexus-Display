//! Page editor (`PageEditor.tsx` equivalent). Split into a pinned `head()`
//! (back button + name + live Display Preview — outside the scroll area so
//! edits show instantly) and `show()`, the scrolling body: brightness,
//! update rate, background color, layout + positions + value style, sensor
//! selection, thresholds.

use crate::core::types::*;
use crate::state::AppState;
use crate::ui::preview::{preview, PreviewState};
use crate::ui::sensors_sel::{self, SensorSelectorState};
use crate::ui::thresholds_ui::{self, ThresholdsUi};
use crate::ui::widgets::{self, ButtonKind};
use eframe::egui::{self, Color32, RichText, Rounding};

pub struct EditorState {
    pub preview: PreviewState,
    pub sensors: SensorSelectorState,
    pub thresholds: ThresholdsUi,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            preview: PreviewState::default(),
            sensors: SensorSelectorState::default(),
            thresholds: ThresholdsUi::default(),
        }
    }
}

/// Snapshot the page under edit. Returns `None` (after bouncing back to the
/// gallery) when the page vanished — deleted from another view.
fn page_snapshot(state: &mut AppState, page_id: &str) -> Option<DisplayPage> {
    if state.page_index_by_id(page_id).is_none() {
        state.leave_editor();
        return None;
    }
    Some(
        state
            .cfg
            .lock()
            .unwrap()
            .pages
            .iter()
            .find(|p| p.id == page_id)
            .cloned()
            .unwrap_or_default(),
    )
}

/// Pinned head of the editor: back button + page name and the live Display
/// Preview. Rendered OUTSIDE the scroll area (see the app.rs editor branch)
/// so the preview stays put while the sections below scroll — every edit the
/// user makes is instantly visible without losing the preview off-screen.
pub fn head(ui: &mut egui::Ui, state: &mut AppState, ed: &mut EditorState) {
    let Some(page_id) = state.editing_page_id.clone() else {
        return;
    };
    let disabled = !state.is_connected();
    let Some(page) = page_snapshot(state, &page_id) else {
        return;
    };

    // ── Header: back + name ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        if widgets::accent_icon_button(ui, "←", "Back", true).clicked() {
            state.leave_editor();
            return;
        }
        let mut name = page.name.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(260.0)
                .font(egui::FontId::proportional(18.0)),
        );
        if resp.changed() && !name.is_empty() {
            let id = page_id.clone();
            state.update_page(&id, |p| p.name = name);
        }
    });
    ui.add_space(8.0);

    // ── Display Preview (pinned — stays visible while editing) ──────────
    widgets::paper(ui, |ui| {
        widgets::section_title(ui, "Display Preview");
        ui.add_space(4.0);
        preview(ui, state, &mut ed.preview, &page, true, !disabled);
    });
    ui.add_space(8.0);
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ed: &mut EditorState) {
    let Some(page_id) = state.editing_page_id.clone() else {
        return;
    };
    let disabled = !state.is_connected();

    // Snapshot the page under edit (head() shows the pinned part; this is
    // the scrolling body from Screen Brightness onward).
    let Some(page) = page_snapshot(state, &page_id) else {
        return;
    };

    // Dev hook: ICUENEXUS_SHOT_SCROLL=<px> pins the editor scroll offset so
    // scripted screenshots (ICUENEXUS_SHOT) can capture below-the-fold
    // panels. Unset during normal use — never touches the state otherwise.
    let shot_scroll: Option<f32> = std::env::var("ICUENEXUS_SHOT_SCROLL")
        .ok()
        .and_then(|v| v.parse().ok());

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("page-editor-scroll")
        .auto_shrink([false, true]);
    if let Some(px) = shot_scroll {
        scroll = scroll.vertical_scroll_offset(px);
    }
    scroll.show(ui, |ui| {
            // ── Compact 60/40 settings block ───────────────────────────
            // Left: Brightness + Update Rate + Background Color stacked.
            // Right: Display Layout, stretched flush to the left column's
            // height. (The 0.60 is the split knob.)
            let gap = ui.spacing().item_spacing.x;
            let total = (ui.available_width() - gap).max(400.0);
            let left_w = total * 0.60;
            let right_w = total - left_w;

            // One explicit left-to-right row: allocate_ui_with_layout only
            // sets the CHILD ui's layout — placement still follows this
            // (vertical) parent, so both cells live inside an LTR row with
            // cross-align Min (top). ui.horizontal would cross-align Center
            // (the old macro-card stagger).
            ui.allocate_ui_with_layout(
                ui.available_size_before_wrap(),
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
                    let left = ui.allocate_ui_with_layout(
                        egui::vec2(left_w, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(left_w);
                            ui.set_max_width(left_w);

                            // ── Screen Brightness ──────────────────────────────
                            widgets::paper(ui, |ui| {
                                widgets::section_title(ui, "Screen Brightness");
                                let mut brightness = page.brightness as f64;
                                // MUI sliders fill their Paper (egui's default is 100px);
                                // reserve room for the "100%" value label after the track.
                                let prev_sw = ui.spacing().slider_width;
                                ui.spacing_mut().slider_width = widgets::slider_fill_width(ui, 56.0);
                                ui.add_enabled(
                                    !disabled,
                                    egui::Slider::new(&mut brightness, 0.0..=100.0)
                                        .fixed_decimals(0)
                                        .suffix("%"),
                                );
                                ui.spacing_mut().slider_width = prev_sw;
                                ui.horizontal(|ui| {
                                    widgets::caption(ui, "Off (0%)");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            widgets::caption(ui, "Max (100%)");
                                        },
                                    );
                                });
                                let new_b = brightness.round() as u8;
                                if new_b != page.brightness {
                                    let id = page_id.clone();
                                    state.update_page(&id, |p| p.brightness = new_b);
                                    // Global brightness mirrors the edited page (TS parity).
                                    let mut guard = state.cfg.lock().unwrap();
                                    guard.brightness = new_b;
                                    crate::core::save_locked(&guard);
                                    drop(guard);
                                }
                            });
                            ui.add_space(8.0);

                            // ── Display Update Rate ────────────────────────────
                            widgets::paper(ui, |ui| {
                                ui.horizontal(|ui| {
                                    widgets::section_title(ui, "Display Update Rate");
                                    let secs = page.display_update_interval as f64 / 1000.0;
                                    widgets::chip(ui, &format!("{secs:.1}s"), None, Color32::from_gray(210));
                                });
                                let mut interval = page.display_update_interval as f64;
                                let prev_sw = ui.spacing().slider_width;
                                // reserve for "10000 ms" value label after the track
                                ui.spacing_mut().slider_width = widgets::slider_fill_width(ui, 78.0);
                                ui.add_enabled(
                                    !disabled,
                                    egui::Slider::new(&mut interval, 100.0..=10000.0)
                                        .step_by(100.0)
                                        .fixed_decimals(0)
                                        .suffix(" ms"),
                                );
                                ui.spacing_mut().slider_width = prev_sw;
                                ui.horizontal(|ui| {
                                    for (v, label) in [(100.0, "0.1s"), (1000.0, "1s"), (5000.0, "5s"), (10000.0, "10s")] {
                                        if ui
                                            .add_enabled(!disabled, egui::Button::new(label).frame(false))
                                            .clicked()
                                        {
                                            interval = v;
                                        }
                                    }
                                });
                                let new_i = (interval.round() as u64).clamp(100, 10000);
                                if new_i != page.display_update_interval {
                                    let id = page_id.clone();
                                    state.update_page(&id, |p| p.display_update_interval = new_i);
                                    let mut guard = state.cfg.lock().unwrap();
                                    guard.display_update_interval = new_i;
                                    crate::core::save_locked(&guard);
                                    drop(guard);
                                }
                            });
                            ui.add_space(8.0);

                            // ── Background Color ───────────────────────────────
                            widgets::paper(ui, |ui| {
                                widgets::section_title(ui, "Background Color");
                                bg_color_picker(ui, state, &page_id, &page, disabled);
                            });
                        },
                    );
                    // Used height of the left cell (allocate_new_ui returns the
                    // child's min_rect — i.e. what the content actually took).
                    let left_h = left.response.rect.height();

                    ui.allocate_ui_with_layout(
                        egui::vec2(right_w, left_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(right_w);
                            ui.set_max_width(right_w);
                            // ── Display Layout ─────────────────────────────────
                            widgets::paper(ui, |ui| {
                                // Stretch flush with the left column (paper has a
                                // 12px inner margin on each side).
                                ui.set_min_height((left_h - 24.0).max(0.0));
                                widgets::section_title(ui, "Display Layout");
                                layout_section(ui, state, &page_id, &page, disabled);
                            });
                        },
                    );
                },
            );
            ui.add_space(8.0);

            // ── Sensors & Data Points ──────────────────────────────────
            widgets::paper(ui, |ui| {
                widgets::section_title(ui, "Sensors & Data Points");
                let page_id2 = page_id.clone();
                let selected = page.selected_sensor_ids.clone();
                // Cap by layout maxSensors
                let max = DISPLAY_LAYOUTS
                    .iter()
                    .find(|l| l.value == page.layout)
                    .map(|l| l.max_sensors)
                    .unwrap_or(20);
                sensors_sel::show(
                    ui,
                    state,
                    &mut ed.sensors,
                    &selected,
                    disabled,
                    &mut move |_state, mut next| {
                        if next.len() > max {
                            next.truncate(max);
                        }
                        let id = page_id2.clone();
                        _state.update_page(&id, |p| p.selected_sensor_ids = next);
                    },
                );
            });
            ui.add_space(8.0);

            // ── Thresholds ─────────────────────────────────────────────
            widgets::paper(ui, |ui| {
                thresholds_ui::show(ui, state, &mut ed.thresholds, disabled);
            });
        });
}

// ─── Background color picker ───────────────────────────────────────────

fn bg_color_picker(
    ui: &mut egui::Ui,
    state: &mut AppState,
    page_id: &str,
    page: &DisplayPage,
    disabled: bool,
) {
    let color = [
        Color32::from_rgb(page.background_color.r, page.background_color.g, page.background_color.b),
    ];
    ui.horizontal(|ui| {
        // 48px swatch
        let (rect, _) = ui.allocate_exact_size(
            eframe::egui::vec2(48.0, 48.0),
            egui::Sense::click(),
        );
        ui.painter().rect_filled(rect, 4.0, color[0]);
        ui.painter().rect_stroke(
            rect,
            Rounding::same(4.0),
            egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );

        ui.add_space(10.0);
        ui.vertical(|ui| {
            let mut changed = false;
            changed |= channel_slider(ui, "R", &mut color[0].r(), disabled, Color32::RED);
            changed |= channel_slider(ui, "G", &mut color[0].g(), disabled, Color32::GREEN);
            changed |= channel_slider(ui, "B", &mut color[0].b(), disabled, Color32::from_rgb(80, 160, 255));

            ui.horizontal(|ui| {
                ui.label(RichText::new("HEX").size(12.5));
                let hex = format!(
                    "#{:02x}{:02x}{:02x}",
                    color[0].r(),
                    color[0].g(),
                    color[0].b()
                );
                ui.add(
                    egui::TextEdit::singleline(&mut hex.clone())
                        .desired_width(90.0)
                        .interactive(false),
                );
                // Live preview button: apply current color
                if widgets::button(ui, "Apply", ButtonKind::Outlined, !disabled).clicked() {
                    changed = true;
                }
            });

            if changed {
                let rgbv = RgbColor::new(color[0].r(), color[0].g(), color[0].b());
                state.update_page(page_id, |p| p.background_color = rgbv);
            }
        });
    });
}

fn channel_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u8,
    disabled: bool,
    tint: Color32,
) -> bool {
    let mut v = *value as f64;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong().size(13.0).color(tint));
        let prev_sw = ui.spacing().slider_width;
        // reserve for the "255" value label after the track + the R/G/B
        // label and spacing before it
        ui.spacing_mut().slider_width = crate::ui::widgets::slider_fill_width(ui, 64.0);
        let resp = ui.add_enabled(
            !disabled,
            egui::Slider::new(&mut v, 0.0..=255.0).fixed_decimals(0),
        );
        ui.spacing_mut().slider_width = prev_sw;
        if resp.changed() {
            *value = v.round() as u8;
            changed = true;
        }
    });
    changed
}

// ─── Layout section ────────────────────────────────────────────────────

/// Settings shown in the Display Layout grid, in display order.
#[derive(Clone, Copy)]
enum LayoutItem {
    Rows,
    Columns,
    LabelPos,
    ValuePos,
    Category,
    Style,
    Font,
    Weight,
}

fn layout_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    page_id: &str,
    page: &DisplayPage,
    disabled: bool,
) {
    // Mode pills — wrapped: this section lives in the narrower right column
    // and the pills must never overflow it.
    ui.horizontal_wrapped(|ui| {
        for layout in DISPLAY_LAYOUTS.iter() {
            let selected = page.layout == layout.value;
            let resp = ui.add_enabled(
                !disabled,
                egui::SelectableLabel::new(selected, layout.label),
            );
            if resp.clicked() {
                state.update_page(page_id, |p| p.layout = layout.value.to_string());
            }
            resp.on_hover_text(layout.description);
        }
    });
    ui.add_space(4.0);
    let current_layout = DISPLAY_LAYOUTS
        .iter()
        .find(|l| l.value == page.layout)
        .unwrap_or(&DISPLAY_LAYOUTS[1]);
    widgets::caption(ui, current_layout.description);
    ui.add_space(6.0);

    // ── Settings grid ────────────────────────────────────────────────
    // One label | control pair per line, in the fixed order: Rows,
    // Columns, Label Position, Value Position, Value Style, Value Font,
    // Category Bar, Line Strength (first three grid-layout only).
    // egui::Grid sizes each column to its widest cell, so labels and
    // controls align in strict columns across every row.
    let mut items = vec![
        LayoutItem::ValuePos,
        LayoutItem::Style,
        LayoutItem::Font,
        LayoutItem::Category,
        LayoutItem::Weight,
    ];
    if page.layout == "sensor-grid" {
        items.splice(
            0..0,
            [LayoutItem::Rows, LayoutItem::Columns, LayoutItem::LabelPos],
        );
    }

    egui::Grid::new("layout-settings-grid")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            for item in items {
                layout_item(ui, item, state, page_id, page, disabled);
                ui.end_row();
            }
        });
}

/// One grid pair: a label cell followed by its control cell. Row pairing /
/// `end_row()` is the caller's job (see `layout_section`).
#[allow(clippy::too_many_arguments)]
fn layout_item(
    ui: &mut egui::Ui,
    item: LayoutItem,
    state: &mut AppState,
    page_id: &str,
    page: &DisplayPage,
    disabled: bool,
) {
    match item {
        LayoutItem::Rows => {
            ui.label(RichText::new("Rows").size(13.0));
            let mut rows = page.grid_rows as u64;
            if ui
                .add_enabled(!disabled, egui::Slider::new(&mut rows, 1..=4).fixed_decimals(0))
                .changed()
            {
                let r = rows as usize;
                state.update_page(page_id, |p| p.grid_rows = r);
            }
        }
        LayoutItem::Columns => {
            ui.label(RichText::new("Columns").size(13.0));
            let mut cols = page.grid_cols as u64;
            if ui
                .add_enabled(!disabled, egui::Slider::new(&mut cols, 1..=16).fixed_decimals(0))
                .changed()
            {
                let c = cols as usize;
                state.update_page(page_id, |p| p.grid_cols = c);
            }
        }
        LayoutItem::LabelPos => position_cell(
            ui,
            state,
            page_id,
            "Label Position",
            page.grid_label_position as u8,
            &LABEL_OPTIONS,
            disabled,
            |page, v| page.grid_label_position = label_position_from(v),
        ),
        LayoutItem::ValuePos => position_cell(
            ui,
            state,
            page_id,
            "Value Position",
            page.value_position as u8,
            &VALUE_OPTIONS,
            disabled,
            |page, v| page.value_position = value_position_from(v),
        ),
        LayoutItem::Category => position_cell(
            ui,
            state,
            page_id,
            "Category Bar",
            page.category_bar_position as u8,
            &BAR_OPTIONS,
            disabled,
            |page, v| page.category_bar_position = bar_position_from(v),
        ),
        LayoutItem::Style => {
            ui.label(RichText::new("Value Style").size(13.0));
            let style = page.value_font_style.unwrap_or(ValueFontStyle::Bold);
            egui::ComboBox::from_id_salt("value-style")
                .selected_text(style_label(style))
                .show_ui(ui, |ui| {
                    for s in [
                        ValueFontStyle::Regular,
                        ValueFontStyle::Bold,
                        ValueFontStyle::Italic,
                        ValueFontStyle::BoldItalic,
                        ValueFontStyle::Underline,
                        ValueFontStyle::Shadow,
                    ] {
                        let selected = style == s;
                        if ui.selectable_label(selected, style_label(s)).clicked() && !disabled {
                            state.update_page(page_id, |p| p.value_font_style = Some(s));
                        }
                    }
                });
        }
        LayoutItem::Font => {
            ui.label(RichText::new("Value Font").size(13.0));
            let font = page.value_font.unwrap_or(ValueFont::Classic);
            egui::ComboBox::from_id_salt("value-font")
                .selected_text(font_label(font))
                .show_ui(ui, |ui| {
                    for f in [
                        ValueFont::Classic,
                        ValueFont::Segment,
                        ValueFont::Retro8,
                        ValueFont::Tiny5,
                        ValueFont::ClassicThin,
                        ValueFont::SegmentThin,
                        ValueFont::Retro8Thin,
                        ValueFont::Tiny5Thin,
                    ] {
                        let selected = font == f;
                        if ui.selectable_label(selected, font_label(f)).clicked() && !disabled {
                            state.update_page(page_id, |p| p.value_font = Some(f));
                        }
                    }
                });
        }
        LayoutItem::Weight => {
            ui.label(RichText::new("Line Strength").size(13.0));
            let weight = page.value_weight;
            let weight_text: String = match weight {
                None => "auto".to_string(),
                Some(w) => {
                    if w.0.fract() == 0.0 {
                        match w.0 as i32 {
                            0 => "0".to_string(),
                            1 => "+1".to_string(),
                            2 => "+2".to_string(),
                            3 => "+3".to_string(),
                            other => format!("{other} (legacy)"),
                        }
                    } else {
                        format!("{:+}", w.0)
                    }
                }
            };
            egui::ComboBox::from_id_salt("value-weight")
                .selected_text(weight_text)
                .show_ui(ui, |ui| {
                    let entries: [(&str, Option<f32>); 5] =
                        [("auto", None), ("0", Some(0.0)), ("1", Some(1.0)), ("2", Some(2.0)), ("3", Some(3.0))];
                    for (name, v) in entries {
                        let selected = match (weight, v) {
                            (None, None) => true,
                            (Some(a), Some(b)) => (a.0 - b).abs() < f32::EPSILON,
                            _ => false,
                        };
                        let label = if v.map_or(false, |x| x > 0.0) && name != "auto" {
                            format!("+{name}")
                        } else {
                            name.to_string()
                        };
                        if ui.selectable_label(selected, label).clicked() && !disabled {
                            state.update_page(page_id, |p| p.value_weight = v.map(ValueWeight));
                        }
                    }
                    // Legacy out-of-set values shown read-only (TS parity)
                    if let Some(w) = weight {
                        if !entries.iter().any(|(_, v)| v.map_or(false, |x| (x - w.0).abs() < f32::EPSILON))
                        {
                            ui.separator();
                            ui.label(
                                RichText::new(format!("{} px (legacy)", w.0))
                                    .size(11.0)
                                    .color(Color32::from_gray(150)),
                            );
                        }
                    }
                });
        }
    }
}

/// Position option tables (index-matched with the enum discriminants we use).
const LABEL_OPTIONS: [(&str, u8); 9] = [
    ("Top", 0),
    ("Bottom", 1),
    ("Top-Left", 2),
    ("Top-Right", 3),
    ("Bottom-Left", 4),
    ("Bottom-Right", 5),
    ("Left", 6),
    ("Right", 7),
    ("Hidden", 8),
];
const VALUE_OPTIONS: [(&str, u8); 10] = [
    ("Center", 0),
    ("Top", 1),
    ("Bottom", 2),
    ("Top-Left", 3),
    ("Top-Right", 4),
    ("Bottom-Left", 5),
    ("Bottom-Right", 6),
    ("Left", 7),
    ("Right", 8),
    ("Hidden", 9),
];
const BAR_OPTIONS: [(&str, u8); 3] = [("Top", 0), ("Bottom", 1), ("Hidden", 2)];

// Discriminant mapping (order matches the enum definitions in types.rs —
// note the TS/serde strings, this is only for the combo indexing).
fn label_position_from(v: u8) -> LabelPosition {
    match v {
        1 => LabelPosition::Bottom,
        2 => LabelPosition::TopLeft,
        3 => LabelPosition::TopRight,
        4 => LabelPosition::BottomLeft,
        5 => LabelPosition::BottomRight,
        6 => LabelPosition::Left,
        7 => LabelPosition::Right,
        8 => LabelPosition::Hidden,
        _ => LabelPosition::Top,
    }
}
fn value_position_from(v: u8) -> ValuePosition {
    match v {
        1 => ValuePosition::Top,
        2 => ValuePosition::Bottom,
        3 => ValuePosition::TopLeft,
        4 => ValuePosition::TopRight,
        5 => ValuePosition::BottomLeft,
        6 => ValuePosition::BottomRight,
        7 => ValuePosition::Left,
        8 => ValuePosition::Right,
        9 => ValuePosition::Hidden,
        _ => ValuePosition::Center,
    }
}
fn bar_position_from(v: u8) -> CategoryBarPosition {
    match v {
        0 => CategoryBarPosition::Top,
        2 => CategoryBarPosition::Hidden,
        _ => CategoryBarPosition::Bottom,
    }
}

/// Label + dropdown cells for the layout settings grid (the TS app used
/// MUI `<Select>` here — the pill rows wrapped and made the panel bulky).
/// Row pairing / `end_row()` is the caller's job.
#[allow(clippy::too_many_arguments)]
fn position_cell(
    ui: &mut egui::Ui,
    state: &mut AppState,
    page_id: &str,
    label: &str,
    current: u8,
    options: &[(&str, u8)],
    disabled: bool,
    apply: fn(&mut DisplayPage, u8),
) {
    ui.label(RichText::new(label).size(13.0));
    let current_name = options
        .iter()
        .find(|(_, v)| *v == current)
        .map(|(n, _)| *n)
        .unwrap_or("?");
    ui.add_enabled_ui(!disabled, |ui| {
        egui::ComboBox::from_id_salt(("position-cell", label))
            .selected_text(current_name)
            .show_ui(ui, |ui| {
                for (name, v) in options {
                    if ui.selectable_label(current == *v, *name).clicked()
                        && *v != current
                    {
                        state.update_page(page_id, |p| apply(p, *v));
                    }
                }
            });
    });
}

fn style_label(s: ValueFontStyle) -> &'static str {
    match s {
        ValueFontStyle::Regular => "Regular",
        ValueFontStyle::Bold => "Bold",
        ValueFontStyle::Italic => "Italic",
        ValueFontStyle::BoldItalic => "Bold Italic",
        ValueFontStyle::Underline => "Underline",
        ValueFontStyle::Shadow => "Shadow",
    }
}

fn font_label(f: ValueFont) -> &'static str {
    match f {
        ValueFont::Classic => "Classic 5×7",
        ValueFont::Segment => "7-Segment",
        ValueFont::Retro8 => "Retro 8×8",
        ValueFont::Tiny5 => "Tiny5 Mono",
        ValueFont::ClassicThin => "Classic 5×7 (thin)",
        ValueFont::SegmentThin => "7-Segment (thin)",
        ValueFont::Retro8Thin => "Retro 8×8 (thin)",
        ValueFont::Tiny5Thin => "Tiny5 Mono (thin)",
    }
}
