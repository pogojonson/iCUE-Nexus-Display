//! Live 640×48 display preview — the Rust equivalent of `NexusDisplay.tsx`.
//!
//! Unlike the DOM approximation in the React app, this renders through the
//! SAME `core::renderer` that produces device frames, so the preview is
//! pixel-accurate (fonts, weights, severity animations, nyan mode, …).

use crate::core::renderer::{render_frame, RenderConfig};
use crate::core::types::*;
use crate::state::AppState;
use crate::ui::widgets;
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, Vec2};

pub struct PreviewState {
    texture: Option<TextureHandle>,
    last_key: String,
}

impl PreviewState {
    pub fn new() -> Self {
        PreviewState {
            texture: None,
            last_key: String::new(),
        }
    }
}

impl Default for PreviewState {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one preview. `show_chips` mirrors the `hideChips` prop inverted.
/// Returns when the user clicks (unused for now, kept for parity).
pub fn preview(
    ui: &mut egui::Ui,
    state: &mut AppState,
    tx: &mut PreviewState,
    page: &DisplayPage,
    show_chips: bool,
    connected: bool,
) {
    // -- frame render ------------------------------------------------
    let readings = state.selected_readings(page);
    let alert_style = state.cfg.lock().unwrap().alert_style.clone();
    let anim_step = if ui.input(|i| i.time.is_finite()) {
        (ui.input(|i| i.time) * 1000.0) as u64
            / crate::core::thresholds::animation_period_ms(&alert_style).max(1)
    } else {
        0
    };
    let connected_now = connected;

    // Always render the frame — even offline the preview shows what the
    // page would look like (the border color signals connection state).
    {
        let mut cfg = RenderConfig::from_page(page, &readings, Some(&alert_style));
        cfg.anim_step = anim_step;
        let frame = render_frame(&cfg);
        let img = frame_to_image(&frame);
        let key = frame_key(page, &readings, anim_step, &alert_style);
        if key != tx.last_key || tx.texture.is_none() {
            tx.last_key = key;
            match &mut tx.texture {
                Some(handle) => handle.set(img, egui::TextureOptions::NEAREST),
                None => {
                    tx.texture = Some(ui.ctx().load_texture(
                        "nexus-preview",
                        img,
                        egui::TextureOptions::NEAREST,
                    ));
                }
            }
        }
    }

    // -- frame + image ------------------------------------------------
    // MUI NexusDisplay: `border: 2px solid` primary.main when connected,
    // divider otherwise.
    let (border_color, border_w) = if connected_now {
        (crate::ui::theme::PINK, 2.0_f32)
    } else {
        (crate::ui::theme::DIVIDER, 1.0_f32)
    };

    let preview_size = Vec2::new(640.0 / 2.0, 48.0 / 2.0);
    egui::Frame::none()
        .fill(Color32::BLACK)
        .stroke(eframe::egui::Stroke::new(border_w, border_color))
        .rounding(eframe::egui::Rounding::same(4.0))
        .inner_margin(eframe::egui::Margin::same(1.0))
        .show(ui, |ui| {
            // Texture is always rendered above, but guard anyway.
            let Some(tex) = &tx.texture else {
                ui.allocate_exact_size(preview_size, egui::Sense::hover());
                return;
            };
            ui.set_min_size(preview_size + Vec2::new(2.0, 2.0));
            let (rect, resp) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
            ui.painter().image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            // Touch feedback overlay (parity with NexusDisplay)
            if let (Some(touch_x), true) = (touch_x_of(state), touch_active_of(state)) {
                let x = touch_x as f32 / 2.0 - 6.0;
                let center = egui::pos2(rect.left() + x + 6.0, rect.center().y);
                ui.painter().circle_filled(center, 6.0, Color32::from_white_alpha(90));
            }
            let _ = resp;
        });

    // -- chips ---------------------------------------------------------
    if show_chips {
        ui.add_space(4.0);
        egui::Frame::none()
            .inner_margin(egui::Margin::same(0.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                        // RGB chip with color swatch
                        let bg = page.background_color;
                        egui::Frame::none()
                            .fill(Color32::from_rgb(bg.r, bg.g, bg.b))
                            .rounding(eframe::egui::Rounding::same(10.0))
                            .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                            .stroke(eframe::egui::Stroke::new(
                                1.0_f32,
                                ui.visuals().widgets.noninteractive.bg_stroke.color,
                            ))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "RGB ({}, {}, {})",
                                        bg.r, bg.g, bg.b
                                    ))
                                    .size(10.5)
                                    .color(Color32::from_gray(210)),
                                );
                            });
                        if let Some(info) = &state.device_info {
                            let text = format!("{}×{}", info.display_width, info.display_height);
                            widgets::chip(ui, &text, None, Color32::from_gray(210));
                        }
                        let layout_name = match page.layout.as_str() {
                            "sensor-grid" => "Grid",
                            "dynamic" => "Dynamic",
                            other => other,
                        };
                        widgets::chip(ui, layout_name, None, Color32::from_gray(210));
                        let n = page.selected_sensor_ids.len();
                        widgets::chip(ui, &format!("{n} Sensors"), None, Color32::from_gray(210));
                        widgets::chip(
                            ui,
                            &format!("Brightness {}%", page.brightness),
                            None,
                            Color32::from_gray(210),
                        );
                        let secs = page.display_update_interval as f64 / 1000.0;
                        widgets::chip(
                            ui,
                            &format!("Update {secs:.1}s"),
                            None,
                            Color32::from_gray(210),
                        );
                });
            });
    }
}

/// Build the preview image from a raw device frame.
///
/// The renderer follows the device convention: alpha is ignored on the wire,
/// so it writes 0 on every pixel. egui's unmultiplied conversion maps `a==0`
/// to `Color32::TRANSPARENT` (discarding the RGB entirely!) — force opaque
/// FIRST so the ColorImage keeps the actual colors. (Regression: this was
/// the "preview renders as a solid black box" bug.)
fn frame_to_image(frame: &[u8]) -> ColorImage {
    debug_assert_eq!(frame.len(), NEXUS_WIDTH * NEXUS_HEIGHT * 4);
    let mut rgba = frame.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    ColorImage::from_rgba_unmultiplied([NEXUS_WIDTH, NEXUS_HEIGHT], &rgba)
}

fn frame_key(
    page: &DisplayPage,
    readings: &[SensorReading],
    anim_step: u64,
    style: &AlertStyle,
) -> String {
    let vals: String = readings
        .iter()
        .map(|r| format!("{}={}({:?});", r.id, r.value, r.severity))
        .collect();
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}",
        page.layout,
        page.background_color,
        page.grid_rows,
        page.grid_cols,
        page.grid_label_position,
        page.value_position,
        page.category_bar_position,
        page.value_font,
        page.value_weight.map(|w| w.0),
        vals,
        // Critical animations change with phase; others are static.
        if vals.contains("Critical") {
            format!("{anim_step}|{style:?}")
        } else {
            String::new()
        }
    )
}

/// Touch is currently disabled upstream (USB contention); keep hooks so the
/// overlay lights up if it's ever re-enabled.
fn touch_x_of(_state: &AppState) -> Option<u16> {
    None
}
fn touch_active_of(_state: &AppState) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Device frames carry alpha=0 everywhere; the preview conversion must
    /// keep the RGB (otherwise every preview pixel renders black).
    #[test]
    fn frame_to_image_keeps_rgb_of_zero_alpha_pixels() {
        // Background (page bg 96,53,0) and a white text pixel, both a=0.
        let mut frame = vec![0u8; NEXUS_WIDTH * NEXUS_HEIGHT * 4];
        frame[0..4].copy_from_slice(&[96, 53, 0, 0]);
        frame[4..8].copy_from_slice(&[255, 255, 255, 0]);

        let img = frame_to_image(&frame);

        assert_eq!(img.pixels[0], Color32::from_rgb(96, 53, 0));
        assert_eq!(img.pixels[1], Color32::from_rgb(255, 255, 255));
        assert_eq!(img.pixels[0].a(), 255);
    }

    /// A fully-rendered grid frame must produce a non-black image end to end.
    #[test]
    fn rendered_frame_survives_preview_conversion() {
        let page = DisplayPage::default();
        let mut readings = vec![];
        // Default page has no sensors → renderer draws "No sensors selected"
        // in the contrast color over the page background.
        let cfg = RenderConfig::from_page(&page, &readings, None);
        let frame = render_frame(&cfg);
        let img = frame_to_image(&frame);

        let bg = page.background_color;
        assert_eq!(img.pixels[0], Color32::from_rgb(bg.r, bg.g, bg.b));
        assert!(
            img.pixels.iter().any(|p| *p != img.pixels[0]),
            "expected lit text pixels over the background"
        );

        // And with a reading present, still no all-black output.
        readings.push(SensorReading {
            id: "cpu_usage".into(),
            label: "CPU".into(),
            category: crate::core::types::SensorCategory::Cpu,
            value: 42.0,
            unit: "%".into(),
            min: Some(0.0),
            max: Some(100.0),
            icon: String::new(),
            severity: None,
        });
        let cfg = RenderConfig::from_page(&page, &readings, None);
        let img = frame_to_image(&render_frame(&cfg));
        assert_eq!(img.pixels[0], Color32::from_rgb(bg.r, bg.g, bg.b));
    }
}
