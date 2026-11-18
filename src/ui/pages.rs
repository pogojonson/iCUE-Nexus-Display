//! Page gallery: card list with mini previews + activate/edit/reorder/delete
//! (the `PageManager.tsx` equivalent).

use crate::core::types::*;
use crate::state::AppState;
use crate::ui::preview::{preview, PreviewState};
use crate::ui::widgets::{self, ButtonKind};
use eframe::egui::{self, Color32, RichText};

pub struct PagesUi {
    pub preview: PreviewState,
}

impl PagesUi {
    pub fn new() -> Self {
        PagesUi {
            preview: PreviewState::new(),
        }
    }
}

impl Default for PagesUi {
    fn default() -> Self {
        Self::new()
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, pages_ui: &mut PagesUi) {
    let connected = state.is_connected();

    // Header row: hint + Add Page
    ui.horizontal(|ui| {
        widgets::caption(ui, "Click a page to edit it");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::button(
                ui,
                "＋ Add Page",
                ButtonKind::Contained(crate::ui::theme::PINK),
                connected,
            )
            .clicked()
            {
                state.add_page();
            }
        });
    });
    ui.add_space(6.0);

    // Snapshot page list to avoid holding the lock during rendering.
    let (pages, current_index) = {
        let guard = state.cfg.lock().unwrap();
        (guard.pages.clone(), guard.current_page_index)
    };

    egui::ScrollArea::vertical()
        .id_salt("page-manager-scroll")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, page) in pages.iter().enumerate() {
                page_card(ui, state, pages_ui, page, i, current_index, connected);
                ui.add_space(8.0);
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn page_card(
    ui: &mut egui::Ui,
    state: &mut AppState,
    pages_ui: &mut PagesUi,
    page: &DisplayPage,
    index: usize,
    current_index: usize,
    connected: bool,
) {
    let is_current = index == current_index;
    let border = if is_current {
        crate::ui::theme::PINK
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };

    egui::Frame::none()
        .fill(crate::ui::theme::PAPER)
        .stroke(egui::Stroke::new(if is_current { 2.0 } else { 1.0_f32 }, border))
        .rounding(egui::Rounding::same(8.0))
        .shadow(crate::ui::theme::paper_shadow())
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            // Row 1: preview + name/chips + actions
            ui.horizontal(|ui| {
                // Mini preview (half scale, no chips)
                preview(ui, state, &mut pages_ui.preview, page, false, connected);

                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&page.name).strong().size(14.0));
                        if is_current {
                            let (fill, tc) = (Color32::from_rgb(46, 125, 50), Color32::WHITE);
                            widgets::chip(ui, "active", Some(fill), tc);
                        }
                    });
                    ui.add_space(3.0);
                    // Config chips row
                    ui.horizontal_wrapped(|ui| {
                        let layout_chip = if page.layout == "sensor-grid" {
                            format!("Grid R×C {}×{}", page.grid_rows, page.grid_cols)
                        } else {
                            "Dynamic".to_string()
                        };
                        widgets::chip(ui, &layout_chip, None, Color32::from_gray(190));
                        let n = page.selected_sensor_ids.len();
                        widgets::chip(ui, &format!("{n} Sensors"), None, Color32::from_gray(190));
                        let bg = page.background_color;
                        widgets::chip(
                            ui,
                            &format!("RGB ({}, {}, {})", bg.r, bg.g, bg.b),
                            None,
                            Color32::from_gray(190),
                        );
                        widgets::chip(
                            ui,
                            &format!("Brightness {}%", page.brightness),
                            None,
                            Color32::from_gray(190),
                        );
                        let secs = page.display_update_interval as f64 / 1000.0;
                        widgets::chip(
                            ui,
                            &format!("Update {secs:.1}s"),
                            None,
                            Color32::from_gray(190),
                        );
                    });
                });

                // Action buttons (right side)
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(false),
                    |ui| {
                        let page_id = page.id.clone();
                        let page_count = state.cfg.lock().unwrap().pages.len();

                        if widgets::icon_button(ui, "🗑", "Delete", connected && page_count > 1)
                            .clicked()
                        {
                            state.remove_page(&page_id);
                        }
                        if widgets::icon_button(ui, "▼", "Move down", connected && index + 1 < page_count)
                            .clicked()
                        {
                            state.reorder_pages(index, index + 1);
                        }
                        if widgets::icon_button(ui, "▲", "Move up", connected && index > 0).clicked() {
                            state.reorder_pages(index, index - 1);
                        }
                        if widgets::icon_button(ui, "✏", "Edit", connected).clicked() {
                            state.enter_editor(page_id);
                        }
                        if !is_current
                            && widgets::icon_button(ui, "✔", "Activate", connected)
                                .clicked()
                        {
                            state.set_current_page(index);
                        }
                    },
                );
            });
        });
}
