# iCUE Nexus Display Control

A **pure-Rust** application for controlling the **Corsair iCUE Nexus Companion
Touch Screen** on Linux. Communicates directly with the display over USB HID —
no iCUE software, **no Electron, no Python**. Once built, the
single static binary keeps working regardless of system library updates.

This is a full rewrite of the original Electron+React app in
[`../k100display/`](../k100display) with the same features and the same
config file — existing pages, macros, thresholds and calibration load
unchanged. A config left behind by the original app (or a pre-rebrand build)
at `~/.config/k100display/pages.json` is copied to
`~/.config/icue-nexus/pages.json` on first start.

## Features

- **Device Discovery** — finds and connects to the Nexus display via USB HID
- **Screen Brightness** — backlight 0–100 %
- **Background Color** — full RGB color picker
- **Display Layouts** — Grid (configurable rows×cols) or Dynamic (flexible columns)
- **Position & Style controls** — label/value/category-bar placement, 8 value
  fonts (Classic 5×7, 7-Segment, Retro 8×8, Tiny5 Mono + thin variants),
  6 type styles, adjustable line strength
- **Live Sensors** — CPU, GPU, memory, disk, network, motherboard, system stats
  rendered directly on the 640×48 display (pixel-accurate egui preview)
- **Multiple Pages** — create/reorder/activate pages, per-page settings,
  timed page rotation
- **Thresholds & Alerts** — per-sensor warning/serious/critical levels with
  6 critical animations (blink, tile flash, rainbow, crazy, nyan)
- **G-Key Macros** — record keyboard macros (grabbed, never leak to apps),
  combos, play back via virtual uinput device, bind to G1–G6 of a
  **Corsair K100** keyboard, G-bit calibration wizard
- **System Tray** — show/hide/quit, hide-to-tray on startup, close-to-tray

## Hardware

- **Display**: Corsair iCUE Nexus (640×48 touchscreen), VID `0x1b1c`,
  PID `0x1b8e`, usage page `0x0C` usage `0x01`, BGRA, 6-bit color (Floyd–Steinberg)
- **Keyboard (macros)**: Corsair K100, VID `0x1b1c`, PID `0x1b7c` (optional)

## Prerequisites

- Linux (X11 or Wayland; tested on KDE Plasma / Wayland)
- The iCUE Nexus plugged in via USB
- For macros: a Corsair K100 + write access to `/dev/uinput` (udev rule below)

## Build

```bash
cd icue-nexus-rs
cargo build --release
# binary: target/release/icue-nexus-display  (~9.5 MB)
```

Only the Rust toolchain is required.

## Setup

### 1. udev rules (required for non-root access)

```bash
sudo cp udev/70-icue-nexus.rules /etc/udev/rules.d/   # Nexus + K100 hidraw
sudo cp udev/80-rapl.rules /etc/udev/rules.d/          # CPU power sensor (optional)
sudo udevadm control -R && sudo udevadm trigger --action=add --subsystem-match=hidraw
# Log out and back in, or reboot
```

For macros you also want `/dev/uinput` writable by your user (most distros
ship a rule for this via the `input` group; otherwise add
`KERNEL=="uinput", MODE="0660", GROUP="input"` and add yourself to `input`).

### 2. Run

```bash
./target/release/icue-nexus-display
```

Assets (icons) are resolved relative to the executable, so the binary can be
launched from anywhere. Optional install:

```bash
sudo install -Dm755 target/release/icue-nexus-display /usr/local/bin/icue-nexus-display
sudo install -Dm644 assets/tray-k-default.png /usr/share/icue-nexus-display/tray-k-default.png
sudo install -Dm644 assets/icon-32.png /usr/share/icue-nexus-display/icon-32.png
```

## Configuration

`~/.config/icue-nexus/pages.json`
(pages, brightness, thresholds, alert style, macros, G-key bindings,
`gbit0` calibration, tray settings). A file at the pre-rebrand location
`~/.config/k100display/pages.json` is copied here on first start. The loader
is field-by-field tolerant —
pages written by older versions that lack newer fields load with defaults
instead of being dropped. The app never writes to the config while another
instance owns it; each mutation is saved immediately (pretty-printed JSON).

## Environment variables

| Variable | Effect |
|---|---|
| `K100_GBIT0` | Override the initial G-bit offset (calibration debug aid) |
| `ICUENEXUS_EDITOR=1` | Open straight into the first page's editor (dev) |
| `ICUENEXUS_SHOT=path.png` | Save one screenshot of the window ~1.5 s after start (dev) |
| `ICUENEXUS_SHOT_SCROLL=px` | Pin the editor's scroll offset for the screenshot above (dev) |
| `ICUENEXUS_SIZE=WxH` | Override the initial window size (dev) |
| `ICUENEXUS_NATIVE_WAYLAND=1` | Keep the Wayland backend (hide-to-tray and the window icon degrade) |

(`K100_PYTHON` from the original app is meaningless now — there is no Python.)

## Architecture

```
icue-nexus-rs/
├── src/
│   ├── main.rs                # entry, tray (ksni/DBus), window, shutdown
│   ├── app.rs                 # egui shell: gallery⇄editor, tray cmds, close-to-tray
│   ├── state.rs               # UI state, event pump, page helpers
│   ├── core/
│   │   ├── hidraw.rs          # pure-Rust hidraw: enumeration, report-descriptor
│   │   │                      #   usage parsing (hidapi-equivalent), feature ioctls
│   │   ├── device.rs          # NexusDevice: discovery, 121×1024B frame upload,
│   │   │                      #   6-bit Floyd–Steinberg dither, brightness/animations
│   │   ├── renderer.rs        # 640×48 frame composer (grid/dynamic, severity,
│   │   │                      #   all 6 critical animations) — also powers the preview
│   │   ├── font.rs            # bitmap font engine: 4 typefaces + thin variants,
│   │   │                      #   6 styles, weight dilation/erosion (supersampled)
│   │   ├── sensors.rs         # /proc,/sys readers + nvidia-smi/checkupdates
│   │   ├── definitions.rs     # 30 sensor definitions
│   │   ├── thresholds.rs      # severity evaluation, alert style
│   │   ├── worker.rs          # display worker thread: 3 cadence timers,
│   │   │                      #   coalesced emission, frame skip, page rotation
│   │   ├── config.rs          # pages.json load/save (field-by-field tolerant)
│   │   └── events.rs          # WorkerMsg/MacroCmd/CoreEvent contract
│   ├── macro_/                # K100 macro engine (no Python)
│   │   ├── manager.rs         # mode policy, command handling, status
│   │   ├── k100.rs            # Bragi channel (1024B jumbo), G-key IFC2 reader
│   │   ├── record.rs          # evdev EVIOCGRAB recording
│   │   └── play.rs            # uinput playback (evdev code tables)
│   └── ui/                    # egui components (gallery, editor, dialogs, …)
├── udev/                      # udev rules (same as the original app)
└── assets/                    # icons
```

Threads: UI (eframe) · display worker (HID + sensors) · macro manager ·
K100 IFC2 reader · tray (ksni, D-Bus StatusNotifierItem).

## Testing

```bash
cargo test                # 89 unit tests (fonts, dither, thresholds, protocol, config…)
cargo test -- --ignored   # hardware tests (need the Nexus/K100 plugged in;
                          #   read-only and safe to run alongside a live session)
```

## Protocol reference

Based on the reverse-engineered protocol from:
- [codingncaffeine/Nexus-Manager](https://github.com/codingncaffeine/Nexus-Manager) — [PROTOCOL.md](https://github.com/codingncaffeine/Nexus-Manager/blob/main/docs/PROTOCOL.md)
- [willneedit/NexusTool](https://github.com/willneedit/NexusTool)

See `../k100display/docs/` for the original protocol/setup notes.

## License

MIT
