//! Shared egui widgets styled after the MUI components of the original app
//! (chips, switches, paper cards, slider rows, icon buttons).

use crate::core::types::{RgbColor, SensorCategory};
use crate::ui::theme;
use eframe::egui::{self, Color32, FontId, RichText, Rounding, Stroke, Vec2};

/// MUI Paper equivalent: elevated rounded card (shadow + divider border).
/// MUI Papers are full-width blocks; egui Frames hug their content — force
/// the content width (minus our own margins) so every card spans the
/// container identically. Both bounds are pinned: `set_min_width` alone
/// doesn't stop over-wide content (e.g. sliders sized from
/// `ui.available_width()`) from pushing a card 20px past its siblings.
pub fn paper(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    const MARGIN: f32 = 12.0;
    let inner_w = (ui.available_width() - MARGIN * 2.0).max(0.0);
    egui::Frame::none()
        .fill(theme::PAPER)
        .stroke(Stroke::new(1.0_f32, theme::DIVIDER))
        .rounding(Rounding::same(8.0))
        .shadow(theme::paper_shadow())
        .inner_margin(egui::Margin::same(MARGIN))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            add(ui);
        });
}

/// Section heading inside a paper — pink accent bar (MUI overline rhythm).
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(4.0, 16.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, Rounding::same(2.0), theme::PINK);
        ui.label(RichText::new(text).strong().size(15.0));
    });
    ui.add_space(3.0);
}

/// MUI Chip: rounded pill; outlined variants get a divider border.
pub fn chip(ui: &mut egui::Ui, text: &str, fill: Option<Color32>, text_color: Color32) {
    let frame = egui::Frame::none()
        .fill(fill.unwrap_or(Color32::from_rgb(0x2a, 0x2a, 0x2a)))
        .stroke(match fill {
            Some(_) => Stroke::NONE,
            None => Stroke::new(1.0_f32, theme::DIVIDER),
        })
        .rounding(Rounding::same(16.0))
        .inner_margin(egui::Margin::symmetric(9.0, 3.0));
    frame.show(ui, |ui| {
        ui.label(RichText::new(text).color(text_color).size(11.0).strong());
    });
}

/// Connection status chip colors (MUI severities).
pub fn status_colors(status: &str) -> (Option<Color32>, Color32) {
    match status {
        "connected" => (Some(theme::GREEN), Color32::WHITE),
        "error" => (Some(theme::RED), Color32::WHITE),
        "connecting" => (Some(Color32::from_rgb(255, 160, 0)), Color32::BLACK),
        _ => (None, Color32::from_gray(180)),
    }
}

/// MUI Switch → a compact toggle built on `toggle_value`.
pub fn switch(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let text = egui::WidgetText::from(label);
    let resp = ui.toggle_value(on, text);
    resp
}


/// Icon-ish button (MUI IconButton) with a tooltip.
pub fn icon_button(
    ui: &mut egui::Ui,
    glyph: &str,
    tooltip: &str,
    enabled: bool,
) -> egui::Response {
    let btn = egui::Button::new(RichText::new(glyph).size(14.0))
        .min_size(Vec2::new(28.0, 28.0));
    let resp = ui.add_enabled(enabled, btn);
    resp.clone().on_hover_text(tooltip);
    resp
}

/// Icon button in the primary accent (pink glyph + pink border) — e.g. the
/// editor's Back arrow, which MUI rendered with `IconButton` over the
/// primary-tinted header.
pub fn accent_icon_button(
    ui: &mut egui::Ui,
    glyph: &str,
    tooltip: &str,
    enabled: bool,
) -> egui::Response {
    let btn = egui::Button::new(RichText::new(glyph).size(14.0).color(theme::PINK))
        .min_size(Vec2::new(28.0, 28.0))
        .fill(theme::PINK_TINT)
        .stroke(Stroke::new(1.0_f32, theme::PINK))
        .rounding(Rounding::same(6.0));
    let resp = ui.add_enabled(enabled, btn);
    resp.clone().on_hover_text(tooltip);
    resp
}

/// Full-width slider sizing: egui renders the track at `slider_width` PLUS a
/// value label after it, so `slider_width = ui.available_width()` overflows
/// the row — which grows the whole Paper, which cascades into the next
/// Paper's available width (papers ended up 50–270px apart). Reserve the
/// value-text width (and its spacing) so the row fits exactly.
///
/// `reserve_max` caps the reservation (worst-case text like "10000 ms").
pub fn slider_fill_width(ui: &egui::Ui, reserve_max: f32) -> f32 {
    (ui.available_width() - reserve_max).max(100.0)
}

/// Width of `text` rendered at `size` (no wrap) — for sizing grid columns.
/// egui memoizes glyph layout, so measuring a few dozen strings per frame
/// is cheap. Uses the same font/fallback chain a `RichText` label would,
/// so emoji icons measure exactly what they render as.
pub fn text_width(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::WHITE)
            .size()
            .x
    })
}

/// One fixed-width grid column: content laid out top-down aligned to `align`
/// (Min = left, Max = right) with the width pinned (min AND max), so the
/// column starts at the same x on every row regardless of what the row's
/// other cells contain. This is what turns a flow of `ui.horizontal` rows
/// into an aligned table.
pub fn col<R>(
    ui: &mut egui::Ui,
    width: f32,
    align: egui::Align,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.allocate_ui_with_layout(
        Vec2::new(width, ui.spacing().interact_size.y),
        egui::Layout::top_down(align),
        |ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            add(ui)
        },
    )
}

/// MUI contained/ outlined button pair used across the app.
pub fn button(ui: &mut egui::Ui, label: &str, kind: ButtonKind, enabled: bool) -> egui::Response {
    match kind {
        ButtonKind::Contained(color) => {
            let base = egui::Button::new(RichText::new(label).strong().size(13.0))
                .fill(color)
                .rounding(Rounding::same(6.0));
            ui.add_enabled(enabled, base)
        }
        // MUI outlined buttons take the palette color for border AND text.
        ButtonKind::Outlined | ButtonKind::OutlinedSecondary => {
            let accent = match kind {
                ButtonKind::OutlinedSecondary => theme::PURPLE,
                _ => theme::PINK,
            };
            let base = egui::Button::new(
                RichText::new(label).strong().size(13.0).color(accent),
            )
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0_f32, accent))
            .rounding(Rounding::same(6.0));
            ui.add_enabled(enabled, base)
        }
    }
}

pub enum ButtonKind {
    /// Filled button (any palette color; callers pass MUI colors).
    Contained(Color32),
    /// Outlined in primary pink (MUI `variant="outlined"`).
    Outlined,
    /// Outlined in secondary purple (MUI `color="secondary"`).
    OutlinedSecondary,
}

/// MUI Alert box.
pub fn alert(ui: &mut egui::Ui, severity: AlertKind, text: &str) {
    let (bg, accent) = match severity {
        AlertKind::Info => (Color32::from_rgb(13, 71, 161), Color32::from_rgb(66, 165, 245)),
        AlertKind::Warning => (Color32::from_rgb(116, 66, 0), Color32::from_rgb(255, 160, 0)),
        AlertKind::Error => (Color32::from_rgb(114, 20, 20), Color32::from_rgb(244, 67, 54)),
    };
    egui::Frame::none()
        .fill(bg)
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .stroke(Stroke::new(1.0_f32, accent))
        .show(ui, |ui| {
            ui.set_max_width(f32::INFINITY);
            ui.label(RichText::new(text).size(13.0));
        });
}

pub enum AlertKind {
    Info,
    Warning,
    Error,
}





pub fn rgb(c: RgbColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

pub fn category_rgb(c: SensorCategory) -> Color32 {
    let [r, g, b] = crate::core::types::category_color(c);
    Color32::from_rgb(r, g, b)
}

/// MUI `text.secondary` caption.
pub fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(Color32::from_gray(179)));
}


#[allow(dead_code)]
pub fn font_id_small() -> FontId {
    FontId::proportional(12.0)
}
