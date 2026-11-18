//! Display frame composer for the Corsair iCUE Nexus (640×48).
//!
//! Buffer convention: **RGBA** (R at byte 0, G at 1, B at 2, A at 3).
//! The font module writes BGRA, so each text draw renders into a scratch
//! BGRA buffer and only lit (non-zero-RGB) pixels are copied over — which
//! means **pure black text is treated as transparent**. Shadow/halo uses
//! level 4: the darkest value that survives 6-bit dithering as "black".
//!
//! The final frame is handed to `NexusDevice::upload_with_dithering` for
//! the wire format, or straight to the egui preview as RGBA.

use crate::core::font::{
    measure_text, measure_text_styled, render_text, render_text_styled, TextStyleOptions,
};
use crate::core::thresholds::DEFAULT_ALERT_STYLE;
use crate::core::types::*;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RenderConfig<'a> {
    pub background_color: RgbColor,
    pub sensors: &'a [SensorReading],
    pub layout: &'a str,
    pub selected_sensor_ids: &'a [String],
    pub grid_rows: usize,
    pub grid_cols: usize,
    pub grid_label_position: LabelPosition,
    pub value_position: ValuePosition,
    pub category_bar_position: CategoryBarPosition,
    pub value_font: Option<ValueFont>,
    pub value_font_style: Option<ValueFontStyle>,
    pub value_weight: Option<f32>,
    pub alert_style: Option<&'a AlertStyle>,
    /// Animation phase counter — advances each animationPeriodMs tick.
    pub anim_step: u64,
}

impl<'a> RenderConfig<'a> {
    pub fn from_page(page: &'a DisplayPage, sensors: &'a [SensorReading], alert_style: Option<&'a AlertStyle>) -> Self {
        RenderConfig {
            background_color: page.background_color,
            sensors,
            layout: &page.layout,
            selected_sensor_ids: &page.selected_sensor_ids,
            grid_rows: page.grid_rows,
            grid_cols: page.grid_cols,
            grid_label_position: page.grid_label_position,
            value_position: page.value_position,
            category_bar_position: page.category_bar_position,
            value_font: page.value_font,
            value_font_style: page.value_font_style,
            value_weight: page.value_weight.map(|w| w.0),
            alert_style,
            anim_step: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Constants (see renderer.ts)
// ---------------------------------------------------------------------------

pub const PIXEL_COUNT: usize = NEXUS_WIDTH * NEXUS_HEIGHT;
pub const BUF_SIZE: usize = PIXEL_COUNT * 4;
const GLYPH_W: i32 = 5;

const PAD_X: i32 = 3;
const DIVIDER_W: i32 = 1;

/// Slack (device px) allowed between the value's height and the free region's
/// height before the value auto-shrinks. The shipped fits rely on it: at grid
/// rows 1–2 the free region is 12 px tall while Classic (14 px) and Retro8
/// (16 px) render at scale 2 — shrinking those would change existing pages.
/// The taller Tiny5 Mono (18 px at scale 2) exceeds 12 + 4 and shrinks instead.
const REGION_H_SLACK: i32 = 4;

const VALUE_SCALE_GRID: i32 = 2;
const VALUE_SCALE_DYNAMIC: i32 = 3;

// ---------------------------------------------------------------------------
// Rendering entry points
// ---------------------------------------------------------------------------

/// Compose one frame for the given config (pure — no device I/O).
pub fn render_frame(config: &RenderConfig) -> Vec<u8> {
    match config.layout {
        "sensor-grid" => render_grid(config),
        _ => render_dynamic(config), // "dynamic" and anything unknown
    }
}

/// Solid color frame (parity with TS renderSolidColor).
#[allow(dead_code)]
pub fn render_solid_color(color: RgbColor) -> Vec<u8> {
    let mut buffer = vec![0u8; BUF_SIZE];
    fill_background(&mut buffer, color);
    buffer
}

// -----------------------------------------------------------------------
// Layout: Grid  (tileW×tileH tiles, cols×rows max)
// -----------------------------------------------------------------------

fn render_grid(config: &RenderConfig) -> Vec<u8> {
    let mut buffer = vec![0u8; BUF_SIZE];
    fill_background(&mut buffer, config.background_color);
    let tc = contrast_color(config.background_color);

    let filtered = filter_selected(config);
    if filtered.is_empty() {
        draw_centered_message(&mut buffer, "No sensors selected", tc, 1);
        return buffer;
    }

    let cols = if config.grid_cols == 0 { 8 } else { config.grid_cols };
    let rows = if config.grid_rows == 0 { 2 } else { config.grid_rows };
    let tile_w = (NEXUS_WIDTH as i32 / cols as i32) as i32;
    let tile_h = (NEXUS_HEIGHT as i32 / rows as i32) as i32;
    let tile_count = cols * rows;

    for (i, sensor) in filtered.iter().enumerate().take(tile_count) {
        let col = i % cols;
        let row = i / cols;
        render_tile(
            &mut buffer,
            sensor,
            col as i32 * tile_w,
            row as i32 * tile_h,
            tile_w,
            tile_h,
            tc,
            config.background_color,
            VALUE_SCALE_GRID,
            config,
        );
    }

    let dim = ((tc.r as f32 * 0.2) as u8, (tc.g as f32 * 0.2) as u8, (tc.b as f32 * 0.2) as u8);
    for c in 1..cols {
        fill_region(&mut buffer, c as i32 * tile_w, 0, DIVIDER_W, NEXUS_HEIGHT as i32, dim);
    }
    for r in 1..rows {
        fill_region(&mut buffer, 0, r as i32 * tile_h, NEXUS_WIDTH as i32, 1, dim);
    }
    buffer
}

// -----------------------------------------------------------------------
// Layout: Dynamic  (flexible columns, 1 row)
// -----------------------------------------------------------------------

fn render_dynamic(config: &RenderConfig) -> Vec<u8> {
    let mut buffer = vec![0u8; BUF_SIZE];
    fill_background(&mut buffer, config.background_color);
    let tc = contrast_color(config.background_color);

    let filtered = filter_selected(config);
    if filtered.is_empty() {
        draw_centered_message(&mut buffer, "No sensors selected", tc, 1);
        return buffer;
    }

    let count = filtered.len() as i32;
    let total_dividers = (count - 1) * DIVIDER_W;
    let col_w = (NEXUS_WIDTH as i32 - total_dividers) / count;

    for (i, sensor) in filtered.iter().enumerate() {
        let tx = i as i32 * (col_w + DIVIDER_W);
        render_tile(
            &mut buffer,
            sensor,
            tx,
            0,
            col_w,
            NEXUS_HEIGHT as i32,
            tc,
            config.background_color,
            VALUE_SCALE_DYNAMIC,
            config,
        );
    }

    let dim = ((tc.r as f32 * 0.2) as u8, (tc.g as f32 * 0.2) as u8, (tc.b as f32 * 0.2) as u8);
    for c in 1..count {
        let x = c * (col_w + DIVIDER_W) - DIVIDER_W / 2;
        fill_region(&mut buffer, x, 0, DIVIDER_W, NEXUS_HEIGHT as i32, dim);
    }
    buffer
}

// -----------------------------------------------------------------------
// Unified tile renderer
// -----------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_tile(
    buffer: &mut [u8],
    sensor: &SensorReading,
    tx: i32,
    ty: i32,
    tile_w: i32,
    tile_h: i32,
    tc: RgbColor,
    bg_color: RgbColor,
    value_scale: i32,
    config: &RenderConfig,
) {
    let inner_w = tile_w - PAD_X * 2;
    let pos = config.grid_label_position;
    let vpos = config.value_position;
    let v_opts = TextStyleOptions {
        font: config.value_font,
        style: Some(config.value_font_style.unwrap_or(ValueFontStyle::Bold)),
        weight: config.value_weight,
    };

    // --- Severity styling ------------------------------------------------
    // warning/serious → static level color on the value text.
    // critical → per alertStyle.criticalAnimation:
    //   value-only / tile / both blink at 500ms, rainbow cycles the value
    //   hue at 250ms, crazy cycles value + tile background at 125ms,
    //   nyan fills the tile with rippling rainbow bands at 125ms and keeps
    //   the value readable with a dark halo.
    let sev = sensor.severity.unwrap_or(Severity::None);
    let mut value_color = tc;
    let mut flash_bg: Option<RgbColor> = None;
    let mut nyan_bg = false;
    if sev != Severity::None {
        if let Some(style) = config.alert_style {
            let sev_color = match sev {
                Severity::Warning => style.warning_color,
                Severity::Serious => style.serious_color,
                _ => style.critical_color,
            };
            if sev == Severity::Critical {
                let step = config.anim_step;
                let on = step % 2 == 0;
                match style.critical_animation {
                    CriticalAnimation::ValueOnly => {
                        value_color = if on { sev_color } else { tc };
                    }
                    CriticalAnimation::Tile => {
                        value_color = sev_color;
                        if on {
                            flash_bg = Some(blend(bg_color, sev_color, 0.4));
                        }
                    }
                    CriticalAnimation::Both => {
                        value_color = if on { sev_color } else { tc };
                        if on {
                            flash_bg = Some(blend(bg_color, sev_color, 0.4));
                        }
                    }
                    CriticalAnimation::Rainbow => {
                        value_color = rainbow_color(step);
                    }
                    CriticalAnimation::Crazy => {
                        value_color = rainbow_color(step);
                        flash_bg = Some(blend(bg_color, rainbow_color(step.wrapping_add(4)), 0.5));
                    }
                    CriticalAnimation::Nyan => {
                        value_color = RgbColor::new(255, 255, 255);
                        nyan_bg = true;
                    }
                }
            } else {
                value_color = sev_color;
            }
        }
    }
    if let Some(flash) = flash_bg {
        fill_region(buffer, tx, ty, tile_w, tile_h, (flash.r, flash.g, flash.b));
    }
    if nyan_bg {
        fill_nyan_bg(buffer, tx, ty, tile_w, tile_h, config.anim_step);
    }

    // Free region inside the tile: excludes the visible label strip and the
    // category-bar strip so the value never collides with either.
    const LABEL_STRIP: i32 = 10; // 2px margin + 7px glyph + 1px gap
    const BAR_STRIP: i32 = 5; // 4px bar + 1px gap
    let mut region_l = tx + PAD_X;
    let mut region_r = tx + tile_w - PAD_X;
    let mut region_t = ty + 2;
    let mut region_b = ty + tile_h - 2;
    if pos != LabelPosition::Hidden {
        match pos {
            LabelPosition::Left => region_l = tx + LABEL_STRIP,
            LabelPosition::Right => region_r = tx + tile_w - LABEL_STRIP,
            LabelPosition::Top
            | LabelPosition::TopLeft
            | LabelPosition::TopRight => region_t = ty + LABEL_STRIP,
            LabelPosition::Bottom
            | LabelPosition::BottomLeft
            | LabelPosition::BottomRight => region_b = ty + tile_h - LABEL_STRIP,
            LabelPosition::Hidden => {}
        }
    }
    match config.category_bar_position {
        CategoryBarPosition::Top => region_t = region_t.max(ty + BAR_STRIP),
        CategoryBarPosition::Bottom => region_b = region_b.min(ty + tile_h - BAR_STRIP),
        CategoryBarPosition::Hidden => {}
    }

    // --- Value: bitmap scaled + styled ---
    let value = format_value(sensor);
    let mut scale = value_scale;
    let mut dim = measure_text_styled(&value, scale, &v_opts);
    // Auto-shrink if too wide for the tile or too tall for the free region
    // (REGION_H_SLACK preserves the shipped fits — see the const docs).
    while (dim.width > inner_w as f32 || dim.height > (region_b - region_t + REGION_H_SLACK) as f32)
        && scale > 1
    {
        scale -= 1;
        dim = measure_text_styled(&value, scale, &v_opts);
    }
    let bold_w = dim.width;
    let bold_h = dim.height;

    // --- Value placement (independent of label position) ---
    if vpos != ValuePosition::Hidden {
        let cx = region_l as f32 + ((region_r - region_l) as f32 - bold_w).floor() / 2.0;
        let cy = region_t as f32 + (((region_b - region_t) as f32 - bold_h) / 2.0).floor();
        let (mut vx, mut vy) = (cx, cy);
        match vpos {
            ValuePosition::Top => vy = region_t as f32,
            ValuePosition::Bottom => vy = region_b as f32 - bold_h,
            ValuePosition::Left => vx = region_l as f32,
            ValuePosition::Right => vx = region_r as f32 - bold_w,
            ValuePosition::TopLeft => {
                vx = region_l as f32;
                vy = region_t as f32;
            }
            ValuePosition::TopRight => {
                vx = region_r as f32 - bold_w;
                vy = region_t as f32;
            }
            ValuePosition::BottomLeft => {
                vx = region_l as f32;
                vy = region_b as f32 - bold_h;
            }
            ValuePosition::BottomRight => {
                vx = region_r as f32 - bold_w;
                vy = region_b as f32 - bold_h;
            }
            _ => {} // "center" already centred in the free region
        }
        // Never spill outside the tile
        vx = vx
            .max(tx as f32)
            .min((tx + tile_w) as f32 - bold_w);
        vy = vy
            .max(ty as f32)
            .min((ty + tile_h) as f32 - bold_h);
        if nyan_bg {
            draw_text_outlined(
                buffer,
                &value,
                vx,
                vy,
                value_color.r,
                value_color.g,
                value_color.b,
                scale,
                &v_opts,
            );
        } else {
            draw_text_styled(
                buffer,
                &value,
                vx,
                vy,
                value_color.r,
                value_color.g,
                value_color.b,
                scale,
                &v_opts,
            );
        }
    }

    // --- Label ---
    if pos != LabelPosition::Hidden {
        let label = abbreviate_label(&sensor.label, 10);
        let label_dim = measure_text(&label, 1);
        let lw = label_dim.width as i32;
        let lh = label_dim.height as i32;
        let (lx, ly) = match pos {
            LabelPosition::Left => (tx + 2, ty + (tile_h - lh) / 2),
            LabelPosition::Right => (tx + tile_w - lw - 2, ty + (tile_h - lh) / 2),
            LabelPosition::TopLeft => (tx + 2, ty + 2),
            LabelPosition::TopRight => (tx + tile_w - lw - 2, ty + 2),
            LabelPosition::BottomLeft => (tx + 2, ty + tile_h - lh - 2),
            LabelPosition::BottomRight => (tx + tile_w - lw - 2, ty + tile_h - lh - 2),
            LabelPosition::Bottom => (
                tx + PAD_X + (inner_w - lw) / 2,
                ty + tile_h - lh - 2,
            ),
            _ => (tx + PAD_X + (inner_w - lw) / 2, ty + 2), // top (default)
        };
        if nyan_bg {
            // White with a dark halo — readable on any rainbow band
            draw_text_outlined(buffer, &label, lx as f32, ly as f32, 255, 255, 255, 1, &TextStyleOptions::default());
        } else {
            draw_text_rgba(
                buffer,
                &label,
                lx,
                ly,
                (tc.r as f32 * 0.45) as u8,
                (tc.g as f32 * 0.45) as u8,
                (tc.b as f32 * 0.45) as u8,
                1,
            );
        }
    }

    // --- Colour-coded bar ---
    if config.category_bar_position != CategoryBarPosition::Hidden {
        let bar_y = if config.category_bar_position == CategoryBarPosition::Top {
            ty
        } else {
            ty + tile_h - 4
        };
        draw_category_bar(buffer, sensor, tx + PAD_X, bar_y, inner_w);
    }
}

fn filter_selected<'a>(config: &'a RenderConfig) -> Vec<&'a SensorReading> {
    config
        .sensors
        .iter()
        .filter(|s| config.selected_sensor_ids.contains(&s.id))
        .collect()
}

// -----------------------------------------------------------------------
// Value formatting / labels
// -----------------------------------------------------------------------

/// Format a sensor value for display. `time` shows its unit (HH:MM),
/// `updates_available` gets the "#" prefix, speeds always one decimal.
pub fn format_value(sensor: &SensorReading) -> String {
    if sensor.id == "time" {
        return sensor.unit.clone();
    }
    if sensor.id == "updates_available" {
        // JS template interpolation of the raw value: integers print bare.
        return format!("{}{}", sensor.unit, sensor.value);
    }
    let speed_ids = [
        "net_rx_speed",
        "net_tx_speed",
        "net_total_speed",
        "disk_read_speed",
        "disk_write_speed",
    ];
    if speed_ids.contains(&sensor.id.as_str()) {
        return format!("{}{}", trim_f64(sensor.value, 1), sensor.unit);
    }
    let num = sensor.value;
    let display = if sensor.unit == "°C" {
        js_to_fixed(num, 1)
    } else if num.fract() == 0.0 && num.abs() < 1e15 {
        format!("{}", num as i64)
    } else {
        js_to_fixed(num, 1)
    };
    format!("{display}{}", sensor.unit)
}

/// JavaScript `Number.prototype.toFixed` parity: round half away from zero
/// (Rust's `{:.1}` uses banker's rounding — 55.25 would print 55.2 instead
/// of JS's 55.3).
fn js_to_fixed(v: f64, decimals: u32) -> String {
    let factor = 10f64.powi(decimals as i32);
    // Scale, round half away from zero, scale back, then format exactly.
    let scaled = v * factor;
    let rounded = scaled.abs().round() * scaled.signum() / factor;
    let d = decimals as usize;
    format!("{rounded:.d$}")
}

/// `toFixed(1)` parity for the speed sensors (JS semantics).
fn trim_f64(v: f64, decimals: usize) -> String {
    js_to_fixed(v, decimals as u32)
}

/// Exact port of the TS abbreviation table; falls back to
/// `text[..maxChars-1] + "…"`.
pub fn abbreviate_label(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let abbr: [(&str, &str); 30] = [
        ("CPU Usage", "CPU"),
        ("CPU Temp", "C.Tmp"),
        ("CPU Clock", "C.Clk"),
        ("CPU Power", "C.Pwr"),
        ("GPU Usage", "GPU"),
        ("GPU Temp", "G.Tmp"),
        ("GPU Clock", "G.Clk"),
        ("GPU Power", "G.Pwr"),
        ("VRAM Used", "VRAM"),
        ("GPU Edge (AMD)", "G.Edge"),
        ("GPU Clock (AMD)", "G.Clk"),
        ("VRAM Clock (AMD)", "V.Clk"),
        ("GPU Power (AMD)", "G.Pwr"),
        ("GPU Fan (AMD)", "G.Fan"),
        ("GPU VRAM (AMD)", "VRAM"),
        ("RAM Usage", "RAM"),
        ("RAM Used", "R.Usd"),
        ("RAM Free", "R.Fre"),
        ("Swap Usage", "Swap"),
        ("Disk Read", "D.Rd"),
        ("Disk Write", "D.Wr"),
        ("Disk Usage", "Disk"),
        ("Net Download", "Net↓"),
        ("Net Upload", "Net↑"),
        ("Net Traffic", "Net↕"),
        ("Load Avg", "Load"),
        ("Processes", "Proc"),
        ("Uptime", "Up"),
        ("Time", "Time"),
        ("Updates", "Upd"),
    ];
    for (from, to) in abbr {
        if text == from {
            return to.to_string();
        }
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

// -----------------------------------------------------------------------
// Nyan mode
// -----------------------------------------------------------------------

/// Nyan Cat background: horizontal rainbow bands with a travelling sine
/// ripple. Phase comes from anim_step (one full ripple cycle every 8 steps
/// = 1 s at 125 ms).
fn fill_nyan_bg(buffer: &mut [u8], tx: i32, ty: i32, tile_w: i32, tile_h: i32, step: u64) {
    let palette: Vec<RgbColor> = (0..8).map(|i| rainbow_color(i)).collect();
    let stripe_h = 3.max(tile_h / 8);
    let wave_len = 96.0f32; // px per ripple wavelength (absolute)
    let amp = stripe_h as f32 * 0.8;
    let phase = (step % 8) as f32 / 8.0;

    for x in tx..tx + tile_w {
        let shift = amp * (((x as f32 / wave_len) + phase) * std::f32::consts::TAU).sin();
        for y in ty..ty + tile_h {
            let mut idx = (((y - ty) as f32 + shift) / stripe_h as f32).floor() as i64 % 8;
            if idx < 0 {
                idx += 8;
            }
            let c = palette[idx as usize];
            let off = (y as usize * NEXUS_WIDTH + x as usize) * 4;
            buffer[off] = c.r;
            buffer[off + 1] = c.g;
            buffer[off + 2] = c.b;
            buffer[off + 3] = 255;
        }
    }
}

// -----------------------------------------------------------------------
// Text drawing helpers (RGBA frame ← BGRA font scratch)
// -----------------------------------------------------------------------

/// Styled value-field text: renders into a scratch BGRA buffer with the
/// font/type-style options applied and copies the lit pixels over the frame.
fn draw_text_styled(
    buffer: &mut [u8],
    text: &str,
    x: f32,
    y: f32,
    r: u8,
    g: u8,
    b: u8,
    scale: i32,
    opts: &TextStyleOptions,
) {
    // Fractional weights → fractional measure → fractional placement; the
    // copy loop needs integer coordinates, so round once here (TS does the
    // same inside renderTextStyled).
    let x = x.round();
    let y = y.round();
    let mut bgra = vec![0u8; BUF_SIZE];
    render_text_styled(&mut bgra, text, x, y, r, g, b, scale, opts, NEXUS_WIDTH, NEXUS_HEIGHT);

    let dim = measure_text_styled(text, scale, opts);
    let min_x = 0.max(x as i32);
    let max_x = (NEXUS_WIDTH as i32).min((x + dim.width + scale as f32) as i32);
    let min_y = 0.max(y as i32);
    let max_y = (NEXUS_HEIGHT as i32).min((y + dim.height) as i32);

    copy_lit(&bgra, buffer, min_x, max_x, min_y, max_y, false);
}

/// Draw text with a 1-pixel dark halo so it stays readable on busy
/// backgrounds (Nyan mode). The font pass treats pure black as transparent
/// and the 6-bit dither would collapse values < 4, so the halo uses level 4.
fn draw_text_outlined(
    buffer: &mut [u8],
    text: &str,
    x: f32,
    y: f32,
    r: u8,
    g: u8,
    b: u8,
    scale: i32,
    opts: &TextStyleOptions,
) {
    let x = x.round();
    let y = y.round();
    let mut bgra = vec![0u8; BUF_SIZE];
    render_text_styled(&mut bgra, text, x, y, r, g, b, scale, opts, NEXUS_WIDTH, NEXUS_HEIGHT);

    let dim = measure_text_styled(text, scale, opts);
    let min_x = 0.max(x as i32);
    let max_x = (NEXUS_WIDTH as i32).min((x + dim.width + scale as f32) as i32);
    let min_y = 0.max(y as i32);
    let max_y = (NEXUS_HEIGHT as i32).min((y + dim.height) as i32);

    // Halo first — the glyph pass below overwrites its own pixels.
    const HALO: [(i32, i32); 8] = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    const OUT: u8 = 4;
    for py in min_y..max_y {
        for px in min_x..max_x {
            let off = (py as usize * NEXUS_WIDTH + px as usize) * 4;
            if bgra[off] == 0 && bgra[off + 1] == 0 && bgra[off + 2] == 0 {
                continue;
            }
            for (dx, dy) in HALO {
                let hx = px + dx;
                let hy = py + dy;
                if hx < 0 || hx >= NEXUS_WIDTH as i32 || hy < 0 || hy >= NEXUS_HEIGHT as i32 {
                    continue;
                }
                let ho = (hy as usize * NEXUS_WIDTH + hx as usize) * 4;
                buffer[ho] = OUT;
                buffer[ho + 1] = OUT;
                buffer[ho + 2] = OUT;
            }
        }
    }
    copy_lit(&bgra, buffer, min_x, max_x, min_y, max_y, false);
}

/// Classic (unstyled) label text via renderText.
fn draw_text_rgba(buffer: &mut [u8], text: &str, x: i32, y: i32, r: u8, g: u8, b: u8, scale: i32) {
    let mut bgra = vec![0u8; BUF_SIZE];
    render_text(&mut bgra, text, x, y, r, g, b, scale, NEXUS_WIDTH, NEXUS_HEIGHT);

    let dim = measure_text(text, scale);
    let min_x = 0.max(x);
    let max_x = (NEXUS_WIDTH as i32).min(x + dim.width as i32 + scale);
    let min_y = 0.max(y);
    let max_y = (NEXUS_HEIGHT as i32).min(y + dim.height as i32);

    copy_lit(&bgra, buffer, min_x, max_x, min_y, max_y, false);
}

/// Copy lit pixels from a BGRA scratch buffer onto the RGBA frame.
/// `halo_only` is unused by current callers but kept for symmetry.
fn copy_lit(
    bgra: &[u8],
    frame: &mut [u8],
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    _halo_only: bool,
) {
    for py in min_y..max_y {
        for px in min_x..max_x {
            let off = (py as usize * NEXUS_WIDTH + px as usize) * 4;
            let fr = bgra[off + 2];
            let fg = bgra[off + 1];
            let fb = bgra[off];
            if fr != 0 || fg != 0 || fb != 0 {
                frame[off] = fr;
                frame[off + 1] = fg;
                frame[off + 2] = fb;
            }
        }
    }
}

fn draw_centered_message(buffer: &mut [u8], text: &str, tc: RgbColor, scale: i32) {
    let dim = measure_text(text, scale);
    let x = ((NEXUS_WIDTH as f32 - dim.width) / 2.0).floor() as i32;
    let y = ((NEXUS_HEIGHT as f32 - dim.height) / 2.0).floor() as i32;
    draw_text_rgba(buffer, text, x, y, tc.r, tc.g, tc.b, scale);
}

// -----------------------------------------------------------------------
// Category bar
// -----------------------------------------------------------------------

fn draw_category_bar(buffer: &mut [u8], sensor: &SensorReading, x: i32, y: i32, width: i32) {
    // No bar for time, updates, or sensors without min/max
    if sensor.id == "time" || sensor.id == "updates_available" {
        return;
    }
    let (Some(min), Some(max)) = (sensor.min, sensor.max) else { return };
    if max <= min {
        return;
    }
    let bar_h = 2;
    let ratio = ((sensor.value - min) / (max - min)).clamp(0.0, 1.0);
    fill_region(buffer, x, y, width, bar_h, (40, 40, 40));
    let c = category_color(sensor.category);
    fill_region(buffer, x, y, (width as f64 * ratio) as i32, bar_h, (c[0], c[1], c[2]));
}

// -----------------------------------------------------------------------
// Color helpers
// -----------------------------------------------------------------------

fn fill_background(buffer: &mut [u8], color: RgbColor) {
    for px in buffer.chunks_exact_mut(4) {
        px[0] = color.r;
        px[1] = color.g;
        px[2] = color.b;
        px[3] = 0;
    }
}

pub fn contrast_color(bg: RgbColor) -> RgbColor {
    if bg.luminance() > 128.0 {
        RgbColor::new(0, 0, 0)
    } else {
        RgbColor::new(255, 255, 255)
    }
}

/// Linear blend: a toward b by t (0..1). Used for tile-flash backgrounds.
fn blend(a: RgbColor, b: RgbColor, t: f32) -> RgbColor {
    a.blend(&b, t)
}

/// 8-step full-saturation hue wheel for the rainbow/crazy animations.
pub fn rainbow_color(step: u64) -> RgbColor {
    let h = ((step as i64 % 8 + 8) % 8) * 45; // 0..315°
    let (c, x, m) = (1.0f32, {
        let hf = h as f32 / 60.0;
        1.0 - (hf % 2.0 - 1.0).abs()
    }, 0.0f32);
    let (mut r, mut g, mut b) = (0.0f32, 0.0f32, 0.0f32);
    match h {
        0..=59 => {
            r = c;
            g = x;
        }
        60..=119 => {
            r = x;
            g = c;
        }
        120..=179 => {
            g = c;
            b = x;
        }
        180..=239 => {
            g = x;
            b = c;
        }
        240..=299 => {
            r = x;
            b = c;
        }
        _ => {
            r = c;
            b = x;
        }
    }
    RgbColor::new(
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

fn fill_region(buffer: &mut [u8], x: i32, y: i32, width: i32, height: i32, color: (u8, u8, u8)) {
    for py in y..y + height {
        if py < 0 || py >= NEXUS_HEIGHT as i32 {
            continue;
        }
        for px in x..x + width {
            if px < 0 || px >= NEXUS_WIDTH as i32 {
                continue;
            }
            let off = (py as usize * NEXUS_WIDTH + px as usize) * 4;
            buffer[off] = color.0;
            buffer[off + 1] = color.1;
            buffer[off + 2] = color.2;
            buffer[off + 3] = 0;
        }
    }
}

// Silence "unused" for the glyph width constant used only in tests.
#[allow(dead_code)]
const GLYPH_W_UNUSED: i32 = GLYPH_W;

/// Default alert style for previews (mirrors thresholds::DEFAULT_ALERT_STYLE
/// but exposed here so the UI can preview without importing thresholds).
#[allow(dead_code)]
pub fn default_alert_style() -> AlertStyle {
    DEFAULT_ALERT_STYLE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(id: &str, value: f64, unit: &str) -> SensorReading {
        SensorReading {
            id: id.into(),
            label: id.into(),
            category: SensorCategory::Cpu,
            value,
            unit: unit.into(),
            min: Some(0.0),
            max: Some(100.0),
            icon: "x".into(),
            severity: Some(Severity::None),
        }
    }

    fn page_cfg<'a>(page: &'a DisplayPage, sensors: &'a [SensorReading]) -> RenderConfig<'a> {
        RenderConfig::from_page(page, sensors, None)
    }

    #[test]
    fn format_rules_match_ts() {
        let mut t = reading("time", 0.0, "14:30");
        assert_eq!(format_value(&t), "14:30");
        t = reading("updates_available", 3.0, "#");
        assert_eq!(format_value(&t), "#3");
        let s = reading("net_rx_speed", 12.34, " KB/s");
        assert_eq!(format_value(&s), "12.3 KB/s");
        let s = reading("disk_read_speed", 0.0, " B/s");
        assert_eq!(format_value(&s), "0.0 B/s");
        let s = reading("cpu_usage", 42.0, "%");
        assert_eq!(format_value(&s), "42%");
        let s = reading("cpu_usage", 42.5, "%");
        assert_eq!(format_value(&s), "42.5%");
        let s = reading("cpu_temp_core1", 55.25, "°C");
        assert_eq!(format_value(&s), "55.3°C"); // °C always 1 decimal
    }

    #[test]
    fn label_abbreviation() {
        // TS rule: only labels LONGER than maxChars go through the table.
        assert_eq!(abbreviate_label("CPU Usage", 10), "CPU Usage"); // 9 ≤ 10
        assert_eq!(abbreviate_label("Net Download", 10), "Net↓");
        assert_eq!(abbreviate_label("Short", 10), "Short");
        // Long unknown label → 9 chars + ellipsis
        let long = "Some Very Long Label";
        let out = abbreviate_label(long, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn empty_selection_renders_message() {
        let page = DisplayPage::default();
        let sensors = vec![];
        let frame = render_frame(&page_cfg(&page, &sensors));
        assert_eq!(frame.len(), BUF_SIZE);
        // Background stays page color (black by default) — the message is
        // drawn centered, so at least some non-background pixel exists.
        assert!(frame.chunks_exact(4).any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0));
    }

    #[test]
    fn grid_renders_with_real_reading() {
        let page = DisplayPage {
            layout: "sensor-grid".into(),
            selected_sensor_ids: vec!["cpu_usage".into()],
            ..DisplayPage::default()
        };
        let mut sensors = vec![reading("cpu_usage", 42.0, "%")];
        sensors[0].label = "CPU Usage".into();
        let cfg = page_cfg(&page, &sensors);
        let frame = render_frame(&cfg);
        assert_eq!(frame.len(), BUF_SIZE);
        // Some pixels lit beyond pure background
        let lit = frame
            .chunks_exact(4)
            .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
            .count();
        assert!(lit > 10, "expected lit text pixels, got {lit}");
    }

    #[test]
    fn dynamic_layout_renders_all_selected() {
        let page = DisplayPage {
            layout: "dynamic".into(),
            selected_sensor_ids: vec!["a".into(), "b".into(), "c".into()],
            ..DisplayPage::default()
        };
        let sensors = vec![
            reading("a", 1.0, "%"),
            reading("b", 2.0, "%"),
            reading("c", 3.0, "%"),
        ];
        let frame = render_frame(&page_cfg(&page, &sensors));
        assert_eq!(frame.len(), BUF_SIZE);
    }

    #[test]
    fn critical_value_only_blinks() {
        let page = DisplayPage {
            layout: "dynamic".into(),
            selected_sensor_ids: vec!["cpu".into()],
            ..DisplayPage::default()
        };
        let mut s = vec![reading("cpu", 99.0, "%")];
        s[0].severity = Some(Severity::Critical);
        let mut style = default_alert_style();
        style.critical_animation = CriticalAnimation::ValueOnly;
        let mut c0 = page_cfg(&page, &s);
        c0.alert_style = Some(&style);
        c0.anim_step = 0;
        let mut c1 = c0.clone();
        c1.anim_step = 1;
        let f0 = render_frame(&c0);
        let f1 = render_frame(&c1);
        // Blink: even/odd frames must differ (red on vs contrast color).
        assert_ne!(f0, f1);
    }

    #[test]
    fn nyan_mode_renders_rainbow_bands() {
        let page = DisplayPage {
            layout: "dynamic".into(),
            selected_sensor_ids: vec!["cpu".into()],
            ..DisplayPage::default()
        };
        let mut s = vec![reading("cpu", 99.0, "%")];
        s[0].severity = Some(Severity::Critical);
        let mut style = default_alert_style();
        style.critical_animation = CriticalAnimation::Nyan;
        let mut cfg = page_cfg(&page, &s);
        cfg.alert_style = Some(&style);
        cfg.anim_step = 0;
        let frame = render_frame(&cfg);
        // Nyan background covers the tile with saturated colors.
        let reds = frame.chunks_exact(4).filter(|p| p[0] > 200 && p[2] < 60).count();
        assert!(reds > 0, "expected nyan red band pixels");
    }

    #[test]
    fn rainbow_wheel_has_8_hues() {
        let colors: Vec<_> = (0..8).map(rainbow_color).collect();
        assert_eq!(colors[0], RgbColor::new(255, 0, 0)); // 0°
        // 90° sits between red and green on the 8-step wheel → (128,255,0)
        assert_eq!(colors[2], RgbColor::new(128, 255, 0));
        // 180° is cyan on the TS wheel (green→blue midpoint crossed)
        assert_eq!(colors[4], RgbColor::new(0, 255, 255));
        // All distinct
        for i in 0..8 {
            for j in i + 1..8 {
                assert_ne!(colors[i], colors[j]);
            }
        }
    }

    #[test]
    fn contrast_and_solid() {
        let f = render_solid_color(RgbColor::new(255, 255, 255));
        assert!(f.chunks_exact(4).all(|p| p[0] == 255 && p[1] == 255 && p[2] == 255));
        assert_eq!(contrast_color(RgbColor::new(250, 250, 250)), RgbColor::new(0, 0, 0));
        assert_eq!(contrast_color(RgbColor::new(10, 10, 10)), RgbColor::new(255, 255, 255));
    }
}
