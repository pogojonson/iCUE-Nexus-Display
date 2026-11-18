pub mod config;
pub mod definitions;
pub mod device;
pub mod events;
pub mod font;
pub mod hidraw;
pub mod renderer;
pub mod sensors;
pub mod thresholds;
pub mod types;
pub mod worker;

pub use config::AppConfig;

use std::sync::{Arc, Mutex};

/// Application config shared between the UI thread, the display worker and
/// the macro manager. Always lock → mutate → save → unlock so persistence
/// stays serialized (saves are tiny JSON writes).
pub type SharedConfig = Arc<Mutex<AppConfig>>;

pub fn new_shared_config(cfg: AppConfig) -> SharedConfig {
    Arc::new(Mutex::new(cfg))
}

/// Persist while holding the lock. Pass the guard you already hold.
pub fn save_locked(cfg: &AppConfig) {
    // One line per mutation — page-count changes must never be silent
    // (this is how the "pages vanished" mystery gets tracked down).
    eprintln!("[config] save ({} pages)", cfg.pages.len());
    if let Err(e) = cfg.save() {
        eprintln!("[config] save failed: {e}");
    }
}
