//! eframe application shell: view switching (gallery ⇄ editor), event pump,
//! tray-driven window management, close-to-tray semantics.

use crate::core::events::WorkerMsg;
use crate::core::SharedConfig;
use crate::state::{AppState, REPAINT_INTERVAL};
use crate::ui::editor::{self, EditorState};
use crate::ui::gallery::{self, GalleryState};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Color32, ViewportCommand, WindowLevel};

/// Commands from the tray (ksni) thread to the UI.
#[derive(Debug, Clone, Copy)]
pub enum TrayCmd {
    Show,
    Hide,
    Quit,
}

pub struct NexusApp {
    pub state: AppState,
    pub gallery: GalleryState,
    pub editor: EditorState,
    pub tray_rx: Receiver<TrayCmd>,
    /// Timestamp when the always-on-top raise pulse should end
    /// (KDE/Wayland focus-steal workaround, see TrayCmd::Show).
    always_on_top_until: Option<u64>,
    /// Dev hook: request one self-screenshot ~1.5 s after the first frame
    /// (lets background threads push their initial status first).
    shot_pending: bool,
    shot_done: bool,
    shot_requested_at: Option<std::time::Instant>,
    /// Close-to-tray disabled: shutdown messages went out, re-issue the
    /// window close after this deadline (CancelClose keeps the frame alive
    /// while the worker/macro threads wind down).
    close_after: Option<std::time::Instant>,
    /// eframe force-shows the window after its first frame (white-flash
    /// fix in `epi_integration::post_rendering`), unconditionally — so
    /// hide-to-tray-on-startup re-asserts `Visible(false)` for the first
    /// few frames; once eframe's one-shot force-show is spent, our last
    /// command wins. Never re-asserted after a tray Show.
    hide_assert_frames: u8,
    shown_via_tray: bool,
}

impl NexusApp {
    pub fn new(
        cfg: SharedConfig,
        worker_tx: Sender<WorkerMsg>,
        macro_tx: Sender<crate::core::events::MacroCmd>,
        event_rx: Receiver<crate::core::events::CoreEvent>,
        tray_rx: Receiver<TrayCmd>,
    ) -> Self {
        let mut app = NexusApp {
            state: AppState::new(cfg, worker_tx, macro_tx, event_rx),
            gallery: GalleryState::default(),
            editor: EditorState::default(),
            tray_rx,
            always_on_top_until: None,
            shot_pending: std::env::var("ICUENEXUS_SHOT").is_ok(),
            shot_done: false,
            shot_requested_at: None,
            close_after: None,
            hide_assert_frames: 0,
            shown_via_tray: false,
        };
        // Startup-hide: prime the re-assert window (eframe force-shows the
        // frame after painting it — see the field docs in `NexusApp`).
        app.hide_assert_frames = if app.state.cfg.lock().unwrap().hide_to_tray_on_startup {
            5
        } else {
            0
        };
        // Dev/testing aid: ICUENEXUS_EDITOR=<page id|1> opens straight into
        // the page editor (handy for scripted screenshots without input
        // synthesis on Wayland).
        if let Ok(target) = std::env::var("ICUENEXUS_EDITOR") {
            let id = if target == "1" {
                app.state
                    .cfg
                    .lock()
                    .unwrap()
                    .pages
                    .first()
                    .map(|p| p.id.clone())
            } else {
                Some(target)
            };
            if let Some(id) = id {
                app.state.enter_editor(id);
            }
        }
        app
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.tray_rx.try_recv() {
            match cmd {
                TrayCmd::Show => {
                    self.shown_via_tray = true;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    // KDE/Wayland focus-steal workaround: brief always-on-top
                    // pulse forces the compositor to raise the window.
                    ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
                    self.always_on_top_until = Some(now_ms() + 80);
                    ctx.request_repaint_after(std::time::Duration::from_millis(90));
                }
                TrayCmd::Hide => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                }
                TrayCmd::Quit => {
                    self.begin_quit(ctx);
                }
            }
        }

        if let Some(until) = self.always_on_top_until {
            if now_ms() >= until {
                self.always_on_top_until = None;
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }
    }

    fn begin_quit(&mut self, ctx: &egui::Context) {
        if self.state.quitting {
            return;
        }
        self.state.quitting = true;
        // Stop hardware side first (mirrors Electron before-quit).
        let _ = self.state.worker_tx.send(WorkerMsg::Shutdown);
        let _ = self
            .state
            .macro_tx
            .send(crate::core::events::MacroCmd::Shutdown);
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

impl eframe::App for NexusApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the event pump alive while idle.
        ctx.request_repaint_after(REPAINT_INTERVAL);

        // Hide-to-tray on startup: re-assert through eframe's one-shot
        // force-show (first painted frame), then stop for good.
        if self.hide_assert_frames > 0 && !self.shown_via_tray {
            if self.state.cfg.lock().unwrap().hide_to_tray_on_startup {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
            self.hide_assert_frames -= 1;
        }

        // Dev hook: ICUENEXUS_SHOT=<path> saves one self-screenshot of the
        // first fully-painted frame (works even when the physical display is
        // asleep — reads our own framebuffer instead of the compositor's).
        if self.shot_pending && !self.shot_done {
            let at = *self.shot_requested_at.get_or_insert_with(std::time::Instant::now);
            if at.elapsed() >= std::time::Duration::from_millis(1500) {
                self.shot_pending = false;
                ctx.send_viewport_cmd(ViewportCommand::Screenshot);
            } else {
                // keep painting until the shot moment
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        let shot_path = std::env::var("ICUENEXUS_SHOT").ok();
        if let Some(path) = shot_path {
            let events: Vec<egui::Event> = ctx.input(|i| i.raw.events.clone());
            for ev in events {
                if let egui::Event::Screenshot { image, .. } = ev {
                    let (w, h) = (image.width(), image.height());
                    let mut rgba = Vec::with_capacity(w * h * 4);
                    for p in &image.pixels {
                        rgba.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
                    }
                    match std::fs::File::create(&path) {
                        Ok(file) => {
                            let mut encoder = png::Encoder::new(file, w as u32, h as u32);
                            encoder.set_color(png::ColorType::Rgba);
                            encoder.set_depth(png::BitDepth::Eight);
                            if let Ok(mut writer) = encoder.write_header() {
                                let _ = writer.write_image_data(&rgba);
                            }
                            eprintln!("[shot] saved {path} ({w}x{h})");
                        }
                        Err(e) => eprintln!("[shot] create failed: {e}"),
                    }
                    self.shot_done = true;
                }
            }
        }

        // Tray commands (show/hide/quit).
        self.handle_tray(ctx);

        // Worker/macro events.
        self.state.pump_events();

        // Close request → hide to tray instead of quitting (unless quitting).
        if ctx.input(|i| i.viewport().close_requested()) && !self.state.quitting {
            let minimize_to_tray = self.state.cfg.lock().unwrap().minimize_to_tray;
            if minimize_to_tray {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            } else {
                // Real quit: shut the hardware threads down first, then
                // close once they had time to wind down.
                self.begin_quit(ctx);
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.close_after =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
            }
        }
        if let Some(deadline) = self.close_after {
            if std::time::Instant::now() >= deadline {
                self.close_after = None;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        // No top bar: the native title bar already carries the window title
        // and OS controls; Quit/Hide live on the native ✕ and the tray menu
        // (TrayCmd::{Show,Hide,Quit}).

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.state.editing_page_id.is_some() {
                // ── Editor mode ─────────────────────────────────────────
                // Connection settings panel hidden while editing; the pinned
                // head (title + back/name + live Display Preview) stays put
                // and only the editor sections scroll beneath it — every
                // edit is instantly visible in the preview.
                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(12.0, 0.0))
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        // MUI Container-like centered column: the edit view
                        // gets a slightly narrower cap than the gallery
                        // (EDITOR_CAP vs 1200) — once the top block was
                        // compacted into 60/40, the full-width sections
                        // below read as too wide. Center it so the extra
                        // room splits into equal margins.
                        let avail = ui.available_width();
                        let w = avail.min(EDITOR_CAP);
                        let x = ui.cursor().left() + ((avail - w) * 0.5).max(0.0);
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(x, ui.cursor().top()),
                            egui::vec2(w, ui.available_height()),
                        );
                        ui.allocate_new_ui(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| {
                                app_header(ui);
                                editor::head(ui, &mut self.state, &mut self.editor);
                                editor::show(ui, &mut self.state, &mut self.editor);
                            },
                        );
                    });
            } else {
                // ── Gallery ─────────────────────────────────────────────
                // Same centered MUI-Container column as the editor, but at
                // the full 1200 gallery cap (the editor narrows to 1100 —
                // see EDITOR_CAP). The scroll area lives inside the column
                // so its scrollbar sits on the column's right edge too.
                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(12.0, 0.0))
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        let avail = ui.available_width();
                        let w = avail.min(GALLERY_CAP);
                        let x = ui.cursor().left() + ((avail - w) * 0.5).max(0.0);
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(x, ui.cursor().top()),
                            egui::vec2(w, ui.available_height()),
                        );
                        ui.allocate_new_ui(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("main-scroll")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.add_space(4.0);
                                        app_header(ui);

                                        // ── Settings paper (connection details) ──
                                        crate::ui::widgets::paper(ui, |ui| {
                                            shared_settings(ui, &mut self.state, ctx);
                                        });
                                        ui.add_space(10.0);

                                        gallery::body(ui, &mut self.state, &mut self.gallery);
                                    });
                            },
                        );
                    });
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Final shutdown safety net (before-quit parity).
        if !self.state.quitting {
            self.state.quitting = true;
            let _ = self.state.worker_tx.send(WorkerMsg::Shutdown);
            let _ = self
                .state
                .macro_tx
                .send(crate::core::events::MacroCmd::Shutdown);
        }
    }
}

/// MUI `Container maxWidth` for the gallery view.
const GALLERY_CAP: f32 = 1200.0;
/// Narrower cap for the edit view (see the editor branch in `update()`).
const EDITOR_CAP: f32 = 1100.0;

/// App title + pink→purple gradient accent strip — shared by the gallery
/// and editor views. Also applies the MUI Container maxWidth:1200 cap to
/// `ui` (set_max_width WIDENS from min.x, which would overflow the window
/// if applied blindly — take the min with available width first).
fn app_header(ui: &mut egui::Ui) {
    let cap = ui.available_width().min(GALLERY_CAP);
    ui.set_max_width(cap);
    ui.add_space(6.0);
    egui::Frame::none().show(ui, |ui| {
        ui.label(
            egui::RichText::new("iCUE Nexus Display Control")
                .strong()
                .size(22.0),
        );
    });
    // Pink→purple gradient accent under the title (MUI app felt flat —
    // this is the gloss).
    gradient_strip(ui);
    ui.add_space(10.0);
}

/// Pink→purple gradient bar (title accent) — painted, cheap.
fn gradient_strip(ui: &mut egui::Ui) {
    let width = ui.available_width();
    if width <= 1.0 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(
        eframe::egui::vec2(width, 3.0),
        egui::Sense::hover(),
    );
    const STEPS: u32 = 48;
    let a = crate::ui::theme::PINK;
    let b = crate::ui::theme::PURPLE;
    let seg_w = rect.width() / STEPS as f32;
    for i in 0..STEPS {
        let t = i as f32 / (STEPS - 1) as f32;
        let color = Color32::from_rgb(
            (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
            (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
            (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
        );
        let x0 = rect.left() + i as f32 * seg_w;
        let seg = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(x0, rect.top()),
            eframe::egui::pos2(x0 + seg_w + 1.0, rect.bottom()),
        );
        ui.painter().rect_filled(seg, eframe::egui::Rounding::ZERO, color);
    }
}

/// Device connection + switches + rotation — the connection details panel.
/// Gallery only: while editing a page this is hidden (the pinned editor head
/// takes its place) so the display preview stays visible during edits.
fn shared_settings(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) {    use crate::ui::widgets;
    use crate::ui::widgets::ButtonKind;
    use crate::core::types::*;

    ui.horizontal(|ui| {
        // ── DeviceConnection ──
        let status = state.connection;
        ui.horizontal_wrapped(|ui| {
            let connecting = status == ConnectionStatus::Connecting;
            let label = if connecting { "Connecting…" } else { "Find & Connect" };
            if widgets::button(
                ui,
                label,
                ButtonKind::Contained(crate::ui::theme::PINK),
                !connecting,
            )
            .clicked()
            {
                state.connect_error = None;
                state.connection = ConnectionStatus::Connecting;
                let _ = state.worker_tx.send(WorkerMsg::Connect);
            }
            if status == ConnectionStatus::Connected
                && widgets::button(ui, "Disconnect", ButtonKind::OutlinedSecondary, true).clicked()
            {
                let _ = state.worker_tx.send(WorkerMsg::Disconnect);
            }
            let (fill, tc) = widgets::status_colors(status.as_str());
            let status_text = status.as_str().to_uppercase();
            widgets::chip(ui, &status_text, fill, tc);
            if connecting {
                ui.add(egui::ProgressBar::new(0.35).animate(true).desired_width(90.0));
            }
        });

        if status == ConnectionStatus::Error {
            ui.add_space(4.0);
            widgets::alert(
                ui,
                widgets::AlertKind::Error,
                "Failed to connect — make sure the device is plugged in and the udev rule is installed.",
            );
        }

        if status == ConnectionStatus::Connected {
            if let Some(info) = state.device_info.clone() {
                ui.add_space(4.0);
                egui::Grid::new("device-info").show(ui, |ui| {
                    ui.label(egui::RichText::new("Model").strong().size(12.0));
                    ui.label(egui::RichText::new(&info.model).size(12.0));
                    ui.end_row();
                    ui.label(egui::RichText::new("Serial").strong().size(12.0));
                    ui.label(egui::RichText::new(&info.serial_number).size(12.0));
                    ui.end_row();
                    ui.label(egui::RichText::new("Firmware").strong().size(12.0));
                    ui.label(egui::RichText::new(&info.firmware_version).size(12.0));
                    ui.end_row();
                    ui.label(egui::RichText::new("Resolution").strong().size(12.0));
                    ui.label(
                        egui::RichText::new(format!(
                            "{}×{}",
                            info.display_width, info.display_height
                        ))
                        .size(12.0),
                    );
                    ui.end_row();
                });
            }
        }

        // ── Switches (right-aligned) ──
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(false),
            |ui| {
                let mut v = state.cfg.lock().unwrap().minimize_to_tray;
                if widgets::switch(ui, &mut v, "Close button minimizes to tray").changed() {
                    let mut g = state.cfg.lock().unwrap();
                    g.minimize_to_tray = v;
                    crate::core::save_locked(&g);
                }
                let mut v = state.cfg.lock().unwrap().hide_to_tray_on_startup;
                if widgets::switch(ui, &mut v, "Hide to tray on startup").changed() {
                    let mut g = state.cfg.lock().unwrap();
                    g.hide_to_tray_on_startup = v;
                    crate::core::save_locked(&g);
                }
                let mut v = state.cfg.lock().unwrap().auto_connect;
                if widgets::switch(ui, &mut v, "Auto-connect").changed() {
                    let mut g = state.cfg.lock().unwrap();
                    g.auto_connect = v;
                    crate::core::save_locked(&g);
                }
            },
        );
    });

    ui.add_space(8.0);

    // ── Page rotation slider (gallery only, per TS) ──
    if state.editing_page_id.is_none() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Page Rotation").strong().size(13.0));
            let mut interval = state.cfg.lock().unwrap().page_rotation_interval as f64;
            // MUI: `<Slider sx={{ maxWidth: 400 }}>` (egui default is 100px).
            // Reserve the value-label width so track+label fits the Paper.
            let prev_sw = ui.spacing().slider_width;
            ui.spacing_mut().slider_width =
                widgets::slider_fill_width(ui, 72.0).min(400.0);
            ui.add(
                egui::Slider::new(&mut interval, 0.0..=60000.0)
                    .fixed_decimals(0)
                    .custom_formatter(|v, _| match v as u64 {
                        0 => "Off".to_string(),
                        ms if ms >= 1000 => format!("{}s", ms / 1000),
                        ms => format!("{ms}ms"),
                    }),
            );
            ui.spacing_mut().slider_width = prev_sw;
            ui.horizontal(|ui| {
                for (v, label) in [
                    (0.0, "Off"),
                    (5000.0, "5s"),
                    (10000.0, "10s"),
                    (30000.0, "30s"),
                    (60000.0, "60s"),
                ] {
                    if ui.add(egui::Button::new(label).frame(false)).clicked() {
                        interval = v;
                    }
                }
            });
            let new_v = interval.round() as u64;
            if new_v != state.cfg.lock().unwrap().page_rotation_interval {
                let mut g = state.cfg.lock().unwrap();
                g.page_rotation_interval = new_v;
                crate::core::save_locked(&g);
                drop(g);
                let _ = state.worker_tx.send(WorkerMsg::Reload);
            }
        });
    }

    // Auto-connect on first frame (TS requestAutoConnect).
    if !state.auto_connect_sent {
        state.auto_connect_sent = true;
        let auto = state.cfg.lock().unwrap().auto_connect;
        if auto && state.connection == crate::core::types::ConnectionStatus::Disconnected {
            state.connection = crate::core::types::ConnectionStatus::Connecting;
            let _ = state.worker_tx.send(WorkerMsg::Connect);
        }
    }
    let _ = ctx;
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
