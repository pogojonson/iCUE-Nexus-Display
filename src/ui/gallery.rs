//! Gallery view: page manager + macro panel papers (`App.tsx` gallery branch).
//!
//! The title, settings paper (connection/switches/rotation) are rendered by
//! `app.rs` because they're shared with the editor view — matching the
//! Settings Paper that App.tsx renders in both branches.

use crate::state::AppState;
use crate::ui::macros_ui;
use crate::ui::pages::{self, PagesUi};
use crate::ui::widgets;
use eframe::egui;

pub struct GalleryState {
    pub pages: PagesUi,
    pub macros: macros_ui::MacroPanelState,
}

impl Default for GalleryState {
    fn default() -> Self {
        GalleryState {
            pages: PagesUi::default(),
            macros: macros_ui::MacroPanelState::default(),
        }
    }
}

pub fn body(ui: &mut egui::Ui, state: &mut AppState, gal: &mut GalleryState) {
    // ── Display Pages paper ─────────────────────────────────────────
    widgets::paper(ui, |ui| {
        pages::show(ui, state, &mut gal.pages);
    });
    ui.add_space(10.0);

    // ── Macro panel paper ───────────────────────────────────────────
    widgets::paper(ui, |ui| {
        macros_ui::show(ui, state, &mut gal.macros);
    });
}
