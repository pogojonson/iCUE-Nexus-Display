# AGENTS.md

Single-crate Rust binary (`icue-nexus-display`) controlling the Corsair iCUE
Nexus touch display (640×48) over hidraw on Linux. No workspace, no CI, no
lint/fmt config — `cargo test` is the only automated gate.

## Commands

- `cargo build --release` → `target/release/icue-nexus-display`
- `cargo test` — ~89 unit tests (fonts, dither, thresholds, protocol, config)
- `cargo test -- --ignored` — 5 hardware tests (`src/core/device.rs`,
  `src/macro_/k100.rs`, `record.rs`, `play.rs`); need the Nexus/K100/uinput
  plugged in, read-only, safe alongside a running session
- Run a single test: `cargo test <substring> -- --nocapture`

## Non-obvious constraints

- **Wayland is deliberately disabled at runtime**: `main()` unsets
  `WAYLAND_DISPLAY`/`WAYLAND_SOCKET` before winit starts so the window runs on
  XWayland — native Wayland breaks hide-to-tray and the window icon
  (`set_visible` is a no-op). `ICUENEXUS_NATIVE_WAYLAND=1` opts out. Do not
  "fix" this by editing eframe features: dropping the `"wayland"` feature does
  nothing (glutin-winit feature unification keeps it compiled in). See the
  comment in `Cargo.toml` and top of `src/main.rs`.
- **Config JSON shape is frozen**: `~/.config/icue-nexus/pages.json` keys must
  match the original Electron app exactly (serde field names are asserted by
  tests in `src/core/config.rs`). Loader is field-by-field tolerant — old
  files missing newer fields must load with defaults, never be dropped.
  Every mutation saves immediately.
- eframe and egui versions must match (both `0.29`).

## Architecture (threads)

UI (eframe/egui, `main.rs`+`app.rs`+`state.rs`) · display worker
(`core/worker.rs`: HID upload + sensor sampling, 3 cadence timers) · macro
manager (`macro_/manager.rs`) · K100 IFC2 reader · tray (ksni/D-Bus). Messages
flow through `core/events.rs` (`WorkerMsg`/`MacroCmd`/`CoreEvent`) — UI must
use the macro manager's own `cmd_sender()`. The renderer (`core/renderer.rs`)
powers both the device frames and the on-screen preview.

`src/macro_/` is named that way because `macro` is a Rust keyword.

## Environment

- Hardware access needs the udev rules in `udev/` installed (otherwise the
  app/tests see no device); macros also need write access to `/dev/uinput`.
- Dev helpers: `ICUENEXUS_SHOT=path.png` (screenshot ~1.5 s after start),
  `ICUENEXUS_EDITOR=1`, `ICUENEXUS_SIZE=WxH`, `ICUENEXUS_SHOT_SCROLL=px`,
  `K100_GBIT0=<offset>` (calibration debug).
- Assets (icons) resolve relative to the executable; keep `assets/` next to
  the binary when installing.

## External references

- Protocol docs and the original Electron app live in the sibling repo
  `../k100display/` (referenced by doc comments in `core/sensors.rs`,
  `core/definitions.rs`); may be absent on other machines. Upstream protocol
  sources: Nexus-Manager PROTOCOL.md, willneedit/NexusTool.
- `README.md` has full setup, hardware IDs, and the module map — trust it
  unless code disagrees.