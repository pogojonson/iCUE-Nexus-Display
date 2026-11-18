//! iCUE Nexus Display Control — Corsair iCUE Nexus Companion Touch Screen for Linux.
//!
//! Pure Rust. Threads:
//!   * main/eframe thread — egui UI
//!   * worker thread — Nexus HID + sensor sampling (`core::worker`)
//!   * macro manager thread — K100 keyboard (`macro_::manager`)
//!   * tray (ksni, D-Bus StatusNotifierItem)

mod app;
mod core;
mod macro_;
mod state;
mod ui;

use crate::core::config::AppConfig;
use crate::core::events::{CoreEvent, WorkerMsg};
use crate::core::{new_shared_config, worker};
use app::{TrayCmd, NexusApp};
use crossbeam_channel::unbounded;
use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    // ── Backend selection (must happen before winit initializes) ─────
    // glutin-winit's default features keep winit's Wayland backend
    // compiled in (feature unification), and winit prefers Wayland
    // whenever WAYLAND_DISPLAY is set. That backend cannot hide windows
    // (`set_visible` is a documented no-op) and cannot set a window icon
    // — breaking hide-to-tray (startup, close-to-tray, tray menu) and the
    // taskbar icon. Unset the Wayland env so winit falls back to X11:
    // running on XWayland, exactly like the original Electron app did.
    // ICUENEXUS_NATIVE_WAYLAND=1 opts out (those features degrade).
    if std::env::var_os("ICUENEXUS_NATIVE_WAYLAND").is_none() {
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("WAYLAND_SOCKET");
    }

    // ── Config ────────────────────────────────────────────────────────
    let mut cfg = AppConfig::load();
    cfg.migrate_legacy_bindings();
    let shared = new_shared_config(cfg.clone());

    // ── Channels ──────────────────────────────────────────────────────
    let (event_tx, event_rx) = unbounded::<CoreEvent>();
    let (worker_tx, worker_rx) = unbounded::<WorkerMsg>();
    let (tray_tx, tray_rx) = unbounded::<TrayCmd>();

    // ── Threads ───────────────────────────────────────────────────────
    let _worker_handle = worker::spawn(shared.clone(), worker_rx, event_tx.clone());
    let macro_manager = macro_::manager::MacroManager::start(shared.clone(), event_tx.clone());
    // The UI must talk to the manager's own command channel.
    let macro_tx = macro_manager.cmd_sender();

    // ── Tray (ksni StatusNotifierItem) ────────────────────────────────
    spawn_tray(tray_tx.clone(), tray_cmd_ctx());

    // ── Window ────────────────────────────────────────────────────────
    // Built fresh per attempt: NativeOptions isn't Clone, and the icon/size
    // reads are cheap. Retry run_native a few times — at boot the session's
    // DISPLAY may not be imported into the systemd user manager yet, and
    // winit fails hard when no display is set (systemd's Restart=on-failure
    // stays as the outer net, but this keeps the journal clean and covers
    // manual launches from a bare shell too).
    for attempt in 1..=3u32 {
        let icon = load_window_icon();
        let mut viewport = egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("iCUE Nexus Display Control");
        // Dev hook: ICUENEXUS_SIZE=<w>x<h> overrides the initial window
        // size for scripted screenshots (ICUENEXUS_SHOT). Unset in normal
        // use.
        if let Ok(s) = std::env::var("ICUENEXUS_SIZE") {
            if let Some((w, h)) = s.split_once('x') {
                if let (Ok(w), Ok(h)) = (w.trim().parse::<f32>(), h.trim().parse::<f32>()) {
                    viewport = viewport.with_inner_size([w, h]);
                }
            }
        }
        if let Some(icon) = icon {
            viewport = viewport.with_icon(icon);
        }
        if cfg.hide_to_tray_on_startup {
            viewport = viewport.with_visible(false);
        }
        let options = eframe::NativeOptions {
            viewport,
            ..Default::default()
        };

        // Fresh clones per attempt (the closure is consumed by run_native).
        let (shared, wtx, mtx, erx, trx) = (
            shared.clone(),
            worker_tx.clone(),
            macro_tx.clone(),
            event_rx.clone(),
            tray_rx.clone(),
        );
        match eframe::run_native(
            "iCUE Nexus Display Control",
            options,
            Box::new(move |cc| {
                egui_extras_install(cc);
                Ok(Box::new(NexusApp::new(shared, wtx, mtx, erx, trx)))
            }),
        ) {
            Ok(()) => break,
            Err(e) => {
                eprintln!("[main] eframe failed (attempt {attempt}/3): {e}");
                if attempt == 3 {
                    macro_manager.shutdown();
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
    }

    // ── Shutdown ordering (before-quit parity): the app's on_exit() has
    //    already sent WorkerMsg::Shutdown + MacroCmd::Shutdown; join the
    //    macro manager so the K100 is reverted to hardware mode and all
    //    threads are gone before the process exits. (The attempt-3 error
    //    branch above does the same before returning early.)
    macro_manager.shutdown();
    Ok(())
}

// ─── Tray ──────────────────────────────────────────────────────────────

fn tray_cmd_ctx() {
    // ksni tray needs no egui context: show/hide/quit go through the channel,
    // and the app's repaint timer picks them up within 100 ms.
}

struct NexusTray {
    tx: crossbeam_channel::Sender<TrayCmd>,
    /// (width, height, ARGB32 bytes) from `load_tray_icon_argb`
    icon: Option<(i32, i32, Vec<u8>)>,
}

impl ksni::Tray for NexusTray {
    fn id(&self) -> String {
        "icue-nexus-display".into()
    }

    fn title(&self) -> String {
        "iCUE Nexus Display Control".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon
            .as_ref()
            .map(|(w, h, data)| ksni::Icon {
                width: *w,
                height: *h,
                data: data.clone(),
            })
            .into_iter()
            .collect()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let tx = self.tx.clone();
        let tx2 = self.tx.clone();
        let tx3 = self.tx.clone();
        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Show Window".into(),
                activate: Box::new(move |_| {
                    let _ = tx.send(TrayCmd::Show);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Hide Window".into(),
                activate: Box::new(move |_| {
                    let _ = tx2.send(TrayCmd::Hide);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(move |_| {
                    let _ = tx3.send(TrayCmd::Quit);
                }),
                ..Default::default()
            }),
        ]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        // Left-click → show (Electron tray click parity)
        let _ = self.tx.send(TrayCmd::Show);
    }
}

fn spawn_tray(
    tx: crossbeam_channel::Sender<TrayCmd>,
    _ctx: (),
) {
    let icon = load_tray_icon_argb();
    std::thread::Builder::new()
        .name("tray-supervisor".into())
        .spawn(move || {
            // Supervise the ksni service. ksni's own `spawn()` does
            // `thread::spawn(|| run().unwrap())` — at boot the session bus
            // may not answer yet (D-Bus NoReply), which panicked the tray
            // thread and left NO tray for the entire session. Run it on our
            // own thread instead: both `Err` and a panic surface here, and
            // we back off and retry until the bus is up.
            loop {
                let tray = NexusTray {
                    tx: tx.clone(),
                    icon: icon.clone(),
                };
                let joined = std::thread::Builder::new()
                    .name("tray".into())
                    .spawn(move || ksni::TrayService::new(tray).run())
                    .map(|h| h.join());
                match joined {
                    Ok(Ok(Ok(()))) => eprintln!("[tray] service stopped"),
                    Ok(Ok(Err(e))) => eprintln!("[tray] D-Bus error: {e}"),
                    Ok(Err(_)) => eprintln!("[tray] service thread panicked"),
                    Err(e) => eprintln!("[tray] could not spawn service thread: {e}"),
                }
                eprintln!("[tray] restarting in 2s");
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        })
        .ok();
}

/// Resolve an asset path: next to the executable first (so launching the
/// binary from any cwd works), then the repo layout relative to the exe
/// (`target/{profile}/../../assets` — covers systemd units, whose cwd is
/// `$HOME`), then the cwd, then a system install prefix.
fn asset_path(name: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("assets").join(name));
            candidates.push(dir.join("../../assets").join(name));
        }
    }
    candidates.push(PathBuf::from("assets").join(name));
    candidates.push(PathBuf::from("/usr/share/icue-nexus-display").join(name));
    candidates.into_iter().find(|p| p.exists())
}

/// Decode `assets/tray-k-default.png` into ARGB32 network-byte-order
/// (byte sequence A,R,G,B — big-endian `Format_ARGB32`, as ksni's `Icon`
/// expects) as `(width, height, data)`.
///
/// NOTE: the asset is 64×64 — the old loader hard-required 32×32 and
/// silently returned None here, which is why the tray icon never showed.
fn load_tray_icon_argb() -> Option<(i32, i32, Vec<u8>)> {
    let path = match asset_path("tray-k-default.png") {
        Some(p) => p,
        None => {
            eprintln!("[tray] icon: assets/tray-k-default.png not found");
            return None;
        }
    };
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[tray] icon: cannot open {}: {e}", path.display());
            return None;
        }
    };
    let decoder = png::Decoder::new(file);
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[tray] icon: cannot decode {}: {e}", path.display());
            return None;
        }
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("[tray] icon: frame read failed for {}: {e}", path.display());
            return None;
        }
    };
    let n = info.width as usize * info.height as usize * 4;
    buf.truncate(n);
    // RGBA → ARGB byte sequence (straight alpha, per StatusNotifierItem spec)
    let mut argb = Vec::with_capacity(n);
    for px in buf.chunks_exact(4) {
        argb.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    Some((info.width as i32, info.height as i32, argb))
}

fn load_window_icon() -> Option<egui::IconData> {
    let candidates = [asset_path("icon-32.png")?];
    for path in candidates {
        if let Ok(file) = std::fs::File::open(&path) {
            let decoder = png::Decoder::new(file);
            let mut reader = decoder.read_info().ok()?;
            let mut buf = vec![0u8; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).ok()?;
            let mut rgba = buf;
            // Premultiply (egui IconData expects premultiplied RGBA? — egui
            // uses straight alpha in practice; pass through as-is).
            rgba.truncate(info.width as usize * info.height as usize * 4);
            return Some(egui::IconData {
                rgba,
                width: info.width,
                height: info.height,
            });
        }
    }
    None
}

fn egui_extras_install(cc: &eframe::CreationContext<'_>) {
    // egui's Proportional family (Ubuntu-Light → NotoEmoji → emoji-icon-font)
    // lacks the ▲▼⌫ arrows and geometric shapes our buttons use. Hack ships
    // with egui and covers them — insert it into the fallback chain.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        if !proportional.iter().any(|f| f == "Hack") {
            // after Ubuntu-Light, before the emoji fonts
            let pos = proportional
                .iter()
                .position(|f| f == "Ubuntu-Light")
                .map(|i| i + 1)
                .unwrap_or(0);
            proportional.insert(pos, "Hack".to_owned());
        }
    }
    cc.egui_ctx.set_fonts(fonts);

    // MUI-dark theme (pink primary / purple secondary, #121212 canvas).
    crate::ui::theme::apply(&cc.egui_ctx);
}
