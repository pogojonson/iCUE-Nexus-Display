//! MUI-dark theme port — the Electron app used
//! `createTheme({ palette: { mode: "dark", primary: "#e91e63",
//! secondary: "#9c27b0" } })` with `CssBaseline`:
//!
//! * `background.default` `#121212` — app canvas
//! * `background.paper`   `#1e1e1e` — Paper cards (elevation via shadow)
//! * primary   `#e91e63` pink   — contained buttons, selection, focus
//! * secondary `#9c27b0` purple — outlined-secondary buttons, accents
//! * divider   `rgba(255,255,255,0.12)` — 1px borders everywhere
//!
//! Applied once at startup from `egui_extras_install`.

use eframe::egui::{self, Color32, Rounding, Stroke, Vec2};

/// MUI `primary.main`
pub const PINK: Color32 = Color32::from_rgb(0xe9, 0x1e, 0x63);
/// MUI `secondary.main`
pub const PURPLE: Color32 = Color32::from_rgb(0x9c, 0x27, 0xb0);
/// MUI dark `background.default`
pub const BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
/// MUI dark `background.paper`
pub const PAPER: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
/// MUI `divider` composited over paper
pub const DIVIDER: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
/// primary @ 20% over paper — MUI selected/toggle background
pub const PINK_TINT: Color32 = Color32::from_rgb(0x46, 0x1e, 0x2c);
/// MUI `success.main`
pub const GREEN: Color32 = Color32::from_rgb(46, 125, 50);
/// MUI `error.main`
pub const RED: Color32 = Color32::from_rgb(211, 47, 47);

/// Install the theme + spacing (call once from `eframe::CreationContext`).
pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();

    v.panel_fill = BG;
    v.window_fill = PAPER;
    v.window_stroke = Stroke::new(1.0_f32, DIVIDER);
    v.window_rounding = Rounding::same(8.0);
    v.menu_rounding = Rounding::same(6.0);

    // Text-edit backgrounds & scrollbars.
    v.extreme_bg_color = Color32::from_rgb(0x0c, 0x0c, 0x0c);
    v.faint_bg_color = Color32::from_rgb(0x18, 0x18, 0x18);
    v.code_bg_color = Color32::from_rgb(0x2a, 0x2a, 0x2a);

    // Text / links / selection.
    // NOTE: egui's `interact_selectable` uses these for SELECTED labels
    // (toggle_value, layout option pills): bg_fill becomes the pill color and
    // stroke.color becomes the text color — MUI parity: primary-tinted pill
    // with pink text (and a pink text-cursor/selection stroke in edits).
    v.hyperlink_color = PINK;
    v.selection.bg_fill = PINK_TINT;
    v.selection.stroke = Stroke::new(1.0_f32, PINK);
    v.warn_fg_color = Color32::from_rgb(255, 160, 0);
    v.error_fg_color = Color32::from_rgb(244, 67, 54);

    // ── Widget states ────────────────────────────────────────────────
    // Outlined look: divider borders + 6px corners everywhere (MUI uses
    // 4px shape radius; 6 reads better at this font size).
    let r6 = Rounding::same(6.0);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, DIVIDER);
    w.noninteractive.rounding = r6;
    w.noninteractive.fg_stroke.color = Color32::from_gray(175); // text.secondary

    w.inactive.bg_fill = Color32::from_rgb(0x2a, 0x2a, 0x2a);
    w.inactive.weak_bg_fill = Color32::from_rgb(0x2a, 0x2a, 0x2a);
    w.inactive.bg_stroke = Stroke::new(1.0_f32, DIVIDER);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(225));
    w.inactive.rounding = r6;

    // Hover: pink border glow — the "flashy" tell on every button/chip.
    w.hovered.bg_fill = Color32::from_rgb(0x34, 0x34, 0x34);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x34, 0x34, 0x34);
    w.hovered.bg_stroke = Stroke::new(1.0_f32, PINK);
    w.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.hovered.rounding = r6;
    w.hovered.expansion = 1.0;

    // Pressed: pink flash.
    w.active.bg_fill = Color32::from_rgb(0x8e, 0x14, 0x40);
    w.active.bg_stroke = Stroke::new(1.0_f32, PINK);
    w.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.active.rounding = r6;

    // "open" = selected toggle / selectable label — primary @ 20%.
    w.open.bg_fill = PINK_TINT;
    w.open.weak_bg_fill = PINK_TINT;
    w.open.bg_stroke = Stroke::new(1.0_f32, PINK);
    w.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.open.rounding = r6;

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    // MUI-ish rhythm: roomier rows than stock egui.
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 4.0);
    style.spacing.indent = 16.0;
    ctx.set_style(style);
}

/// MUI elevation-1 Paper shadow (0 2px 4px -1px rgba(0,0,0,0.28)).
pub fn paper_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: Vec2::new(0.0, 2.0),
        blur: 6.0,
        spread: 0.0,
        color: Color32::from_black_alpha(70),
    }
}
