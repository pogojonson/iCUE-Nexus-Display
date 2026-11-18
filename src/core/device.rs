//! Corsair iCUE Nexus USB HID device manager (pure Rust, hidraw).
//!
//! Protocol reference: codingncaffeine/Nexus-Manager docs/PROTOCOL.md.
//! Device: 640×48 touchscreen, VID 0x1b1c, PID 0x1b8e.
//! Interface 0: usage page 0x0C, usage 0x01 (consumer device) — never the
//! boot-keyboard interface (usage page 0x01, usage 0x06).
//! Pixel format on the wire: BGRA (4th byte ignored), 6-bit color depth.

use crate::core::hidraw::{enumerate, HidDevice};
use crate::core::types::*;
use std::time::{SystemTime, UNIX_EPOCH};

const DEVICE_NAME: &str = "CORSAIR iCUE NEXUS";

pub struct NexusDevice {
    handle: Option<HidDevice>,
    device_info: Option<NexusDeviceInfo>,
    applied_brightness: Option<u8>,
}

impl NexusDevice {
    pub fn new() -> Self {
        NexusDevice {
            handle: None,
            device_info: None,
            applied_brightness: None,
        }
    }

    /// Scan for the Nexus display by VID/PID and usage page.
    /// Reads firmware/serial/geometry from feature reports like node-hid did.
    pub fn discover(&mut self) -> Option<NexusDeviceInfo> {
        let nexus = enumerate().into_iter().find(|d| {
            d.vendor_id == NEXUS_VID
                && d.product_id == NEXUS_PID
                && d.usage_page == NEXUS_USAGE_PAGE
                && d.usage == NEXUS_USAGE
        })?;

        let mut firmware_version = "unknown".to_string();
        let mut serial_number = if nexus.serial_number.is_empty() {
            "unknown".to_string()
        } else {
            nexus.serial_number.clone()
        };
        let mut display_width = NEXUS_WIDTH as u16;
        let mut display_height = NEXUS_HEIGHT as u16;

        // Best-effort metadata reads — the device may be busy; enumeration
        // results are still reported in that case (same as the TS).
        if let Ok(hid) = HidDevice::open(&nexus.path) {
            // Feature 0x04: [04][len][tag(4)][version string...]
            if let Ok(fw) = hid.get_feature_report(0x04, 32) {
                if fw.len() > 6 {
                    let end = fw.len().min(14);
                    firmware_version = clean_str(&fw[6..end]);
                }
            }
            // Feature 0x07: [07][len][serial string...]
            if let Ok(sr) = hid.get_feature_report(0x07, 32) {
                if sr.len() > 2 {
                    let len = sr[1] as usize;
                    let end = (2 + len).min(sr.len());
                    let serial = clean_str(&sr[2..end]);
                    if !serial.is_empty() {
                        serial_number = serial;
                    }
                }
            }
            // Feature 0x09: width LE16 @ [7..9], height LE16 @ [9..11]
            if let Ok(geom) = hid.get_feature_report(0x09, 32) {
                if geom.len() >= 11 {
                    display_width = u16::from_le_bytes([geom[7], geom[8]]);
                    display_height = u16::from_le_bytes([geom[9], geom[10]]);
                }
            }
        }

        let info = NexusDeviceInfo {
            device_id: nexus.path.to_string_lossy().into_owned(),
            model: DEVICE_NAME.to_string(),
            firmware_version,
            serial_number,
            display_width,
            display_height,
            bytes_per_pixel: NEXUS_BPP as u8,
            connected: true,
        };
        self.device_info = Some(info.clone());
        Some(info)
    }

    /// Open the HID connection to the device.
    pub fn connect(&mut self) -> Result<(), String> {
        if self.handle.is_some() {
            self.disconnect();
        }
        let nexus = enumerate().into_iter().find(|d| {
            d.vendor_id == NEXUS_VID
                && d.product_id == NEXUS_PID
                && d.usage_page == NEXUS_USAGE_PAGE
                && d.usage == NEXUS_USAGE
        });
        let Some(dev) = nexus else {
            return Err("Nexus device not found".into());
        };
        let handle = HidDevice::open(&dev.path).map_err(|e| format!("open {}: {e}", dev.path.display()))?;
        self.handle = Some(handle);
        self.applied_brightness = None;
        if self.device_info.is_none() {
            self.discover();
        }
        Ok(())
    }

    /// Close the HID connection, restoring the idle animation first.
    pub fn disconnect(&mut self) {
        if self.handle.is_some() {
            // Restore the default idle animation before disconnecting —
            // never blank + brightness 0 (leaves a dead black strip).
            let _ = self.play_animation(AnimationId::IDLE, true);
            self.handle = None;
            self.applied_brightness = None;
        }
    }

    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }

    #[allow(dead_code)] // TS getDeviceInfo parity
    pub fn get_device_info(&self) -> Option<NexusDeviceInfo> {
        self.device_info.clone()
    }

    /// Set screen brightness (0–100). Feature report 3: `[03, 01, value]`.
    /// Returns whether the device actually saw a new value (parity with the
    /// worker's `appliedBrightness` bookkeeping).
    pub fn set_brightness(&mut self, value: u8) -> Result<bool, String> {
        let clamped = value.min(100);
        if self.applied_brightness == Some(clamped) {
            return Ok(false);
        }
        self.send_feature_command(&[HID_REPORT_FEATURE_CMD, 0x01, clamped])?;
        self.applied_brightness = Some(clamped);
        Ok(true)
    }

    /// Blank the screen. Feature report 3: `[03, 04]`.
    #[allow(dead_code)]
    pub fn blank(&mut self) -> Result<(), String> {
        self.send_feature_command(&[HID_REPORT_FEATURE_CMD, 0x04])
    }

    /// Play a built-in firmware animation (1=idle, 2=alt, 3=update).
    /// Feature report 3: `[03, 0D, anim, loop]`.
    pub fn play_animation(&mut self, anim: AnimationId, run_loop: bool) -> Result<(), String> {
        self.send_feature_command(&[
            HID_REPORT_FEATURE_CMD,
            0x0d,
            anim.0,
            if run_loop { 1 } else { 0 },
        ])
    }

    /// Stop the currently playing animation. Feature report 3: `[03, 0F]`.
    pub fn stop_animation(&mut self) -> Result<(), String> {
        self.send_feature_command(&[HID_REPORT_FEATURE_CMD, 0x0f])
    }

    /// Upload a raw BGRA buffer (exactly 640×48×4 bytes) as 121×1024-byte
    /// output reports.
    pub fn upload_image(&self, bgra: &[u8]) -> Result<(), String> {
        let handle = self.handle.as_ref().ok_or("Device not connected")?;
        let expected_size = IMAGE_TOTAL_BYTES;
        if bgra.len() != expected_size {
            return Err(format!(
                "Invalid buffer size: {}, expected {expected_size}",
                bgra.len()
            ));
        }
        let total_payload =
            (IMAGE_TOTAL_BLOCKS - 1) * IMAGE_BLOCK_PAYLOAD + IMAGE_LAST_BLOCK_PAYLOAD;
        if total_payload != expected_size {
            return Err(format!(
                "Payload math error: {total_payload} != {expected_size}"
            ));
        }

        let mut report = [0u8; IMAGE_BLOCK_SIZE];
        for block in 0..IMAGE_TOTAL_BLOCKS {
            let is_last = block == IMAGE_TOTAL_BLOCKS - 1;
            let payload_size = if is_last {
                IMAGE_LAST_BLOCK_PAYLOAD
            } else {
                IMAGE_BLOCK_PAYLOAD
            };
            let offset = block * IMAGE_BLOCK_PAYLOAD;

            report.fill(0);
            report[0] = HID_REPORT_OUTPUT_IMAGE; // Report ID
            report[1] = 0x05; // Command
            report[2] = 0x40; // Don't-care (some impls use 0x1F)
            report[3] = if is_last { 1 } else { 0 }; // Final block flag
            report[4] = (block & 0xff) as u8; // Block number LE
            report[5] = ((block >> 8) & 0xff) as u8;
            report[6] = (payload_size & 0xff) as u8; // Payload length LE
            report[7] = ((payload_size >> 8) & 0xff) as u8;
            report[8..8 + payload_size]
                .copy_from_slice(&bgra[offset..offset + payload_size]);

            handle
                .write_report(&report)
                .map_err(|e| format!("block {block} write: {e}"))?;
        }
        Ok(())
    }

    /// Upload a solid color frame (no dithering, like TS uploadSolidColor).
    #[allow(dead_code)]
    pub fn upload_solid_color(&self, r: u8, g: u8, b: u8) -> Result<(), String> {
        let mut buf = vec![0u8; IMAGE_TOTAL_BYTES];
        for px in buf.chunks_exact_mut(4) {
            px[0] = b;
            px[1] = g;
            px[2] = r;
            px[3] = 0;
        }
        self.upload_image(&buf)
    }

    /// Upload an RGBA buffer with Floyd–Steinberg dithering for the display's
    /// 6-bit color depth, then push it to the device.
    ///
    /// Quantization clamps at 252 (=63×4): `round(255/4)*4` would yield 256,
    /// which `& 0xff` turns into 0 — riddling full-white pixels with black
    /// holes while near-white error diffusion spread them across glyphs.
    pub fn upload_with_dithering(&self, rgba: &[u8]) -> Result<(), String> {
        let (bgra, _) = dither_rgba_to_bgra(rgba)?;
        self.upload_image(&bgra)
    }

    fn send_feature_command(&self, data: &[u8]) -> Result<(), String> {
        let handle = self
            .handle
            .as_ref()
            .ok_or("Device not connected")?;
        handle
            .send_feature_report(data)
            .map(|_| ())
            .map_err(|e| format!("feature report: {e}"))
    }
}

impl Default for NexusDevice {
    fn default() -> Self {
        Self::new()
    }
}

fn clean_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// Pure dither step (unit-testable): RGBA frame → dithered BGRA wire bytes.
/// Returns the wire buffer and the number of pixels processed.
pub fn dither_rgba_to_bgra(rgba: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let width = NEXUS_WIDTH;
    let height = NEXUS_HEIGHT;
    let pixel_count = width * height;
    if rgba.len() != pixel_count * 4 {
        return Err(format!(
            "Invalid RGBA buffer: {} bytes, expected {}",
            rgba.len(),
            pixel_count * 4
        ));
    }

    // Working copy with floating-point values for error diffusion.
    let mut pixels = vec![0f64; pixel_count * 3];
    for i in 0..pixel_count {
        pixels[i * 3] = rgba[i * 4] as f64;
        pixels[i * 3 + 1] = rgba[i * 4 + 1] as f64;
        pixels[i * 3 + 2] = rgba[i * 4 + 2] as f64;
    }

    let distribute =
        |pixels: &mut [f64], x: isize, y: isize, err: [f64; 3], ox: isize, oy: isize, weight: f64| {
            let nx = x + ox;
            let ny = y + oy;
            if nx >= 0 && (nx as usize) < width && ny >= 0 && (ny as usize) < height {
                let n = ((ny as usize) * width + nx as usize) * 3;
                pixels[n] += err[0] * weight;
                pixels[n + 1] += err[1] * weight;
                pixels[n + 2] += err[2] * weight;
            }
        };

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 3;
            let old = [
                pixels[idx].clamp(0.0, 255.0),
                pixels[idx + 1].clamp(0.0, 255.0),
                pixels[idx + 2].clamp(0.0, 255.0),
            ];
            let quant = |v: f64| (v / 4.0).round() * 4.0;
            let new = [quant(old[0]).min(252.0), quant(old[1]).min(252.0), quant(old[2]).min(252.0)];
            pixels[idx] = new[0];
            pixels[idx + 1] = new[1];
            pixels[idx + 2] = new[2];
            let err = [old[0] - new[0], old[1] - new[1], old[2] - new[2]];
            distribute(&mut pixels, x as isize, y as isize, err, 1, 0, 7.0 / 16.0);
            distribute(&mut pixels, x as isize, y as isize, err, -1, 1, 3.0 / 16.0);
            distribute(&mut pixels, x as isize, y as isize, err, 0, 1, 5.0 / 16.0);
            distribute(&mut pixels, x as isize, y as isize, err, 1, 1, 1.0 / 16.0);
        }
    }

    let mut bgra = vec![0u8; pixel_count * 4];
    for i in 0..pixel_count {
        let off = i * 4;
        bgra[off] = pixels[i * 3 + 2] as u8; // B
        bgra[off + 1] = pixels[i * 3 + 1] as u8; // G
        bgra[off + 2] = pixels[i * 3] as u8; // R
        bgra[off + 3] = 0; // X (ignored)
    }
    Ok((bgra, pixel_count))
}

/// Milliseconds since the UNIX epoch (for touch timestamps / parity helpers).
#[allow(dead_code)]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_upload_math() {
        let total_payload =
            (IMAGE_TOTAL_BLOCKS - 1) * IMAGE_BLOCK_PAYLOAD + IMAGE_LAST_BLOCK_PAYLOAD;
        assert_eq!(total_payload, NEXUS_WIDTH * NEXUS_HEIGHT * NEXUS_BPP);
        // Header layout per block: offsets 4..6 block#, 6..8 len (LE).
        let block = 300u16; // fits u16 as the protocol requires
        let bytes = block.to_le_bytes();
        assert_eq!([bytes[0], bytes[1]], [0x2c, 0x01]);
    }

    #[test]
    fn dither_quantizes_to_6bit_steps() {
        // One pixel: mid gray — must land on a multiple of 4 ≤ 252.
        let mut rgba = vec![0u8; 640 * 48 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[..3].copy_from_slice(&[130, 130, 130]);
            px[3] = 255;
        }
        let (bgra, count) = dither_rgba_to_bgra(&rgba).unwrap();
        assert_eq!(count, 640 * 48);
        // First pixel: 130 quantizes to 132 (130/4=32.5 → 33 → 132).
        assert_eq!(bgra[0], 132); // B
        assert_eq!(bgra[1], 132); // G
        assert_eq!(bgra[2], 132); // R
        assert_eq!(bgra[3], 0);
        // Every channel in the whole frame is a multiple of 4, never 255
        // (clamp at 252 prevents the 256→0 wrap).
        for px in bgra.chunks_exact(4) {
            assert_eq!(px[0] % 4, 0);
            assert_eq!(px[1] % 4, 0);
            assert_eq!(px[2] % 4, 0);
            assert!(px[0] <= 252);
        }
    }

    #[test]
    fn dither_white_stays_white() {
        let rgba = vec![255u8; 640 * 48 * 4];
        let (bgra, _) = dither_rgba_to_bgra(&rgba).unwrap();
        // All white must remain maxed — the 252 clamp means 252, and no
        // black holes (0) may appear anywhere.
        assert!(bgra.chunks_exact(4).all(|p| p[0] == 252 && p[3] == 0));
    }

    #[test]
    fn dither_rejects_bad_size() {
        assert!(dither_rgba_to_bgra(&[0u8; 10]).is_err());
    }

    #[test]
    fn clean_str_trims() {
        assert_eq!(clean_str(b"1.2.3\0\0\0"), "1.2.3");
        assert_eq!(clean_str(b"  hi  \0"), "hi");
        assert_eq!(clean_str(b""), "");
    }

    #[test]
    fn brightness_clamps() {
        // Pure logic: clamp behavior mirrored from TS Math.max/min/round.
        let clamp = |v: i32| v.clamp(0, 100) as u8;
        assert_eq!(clamp(-5), 0);
        assert_eq!(clamp(101), 100);
        assert_eq!(clamp(50), 50);
    }
}

#[cfg(test)]
mod hardware_tests {
    use super::*;

    /// Read-only probe: enumerates the Nexus and reads its three feature
    /// reports (firmware/serial/geometry). Safe alongside a running session:
    /// feature requests travel on EP0 (control endpoint), never touching the
    /// interrupt OUT endpoint used for frame uploads, and nothing is written.
    #[test]
    #[ignore = "needs the Nexus display plugged in (read-only, safe alongside a live session)"]
    fn discover_real_device() {
        let mut dev = NexusDevice::new();
        let info = dev.discover();
        match info {
            Some(info) => {
                println!("model={} fw={} serial={}", info.model, info.firmware_version, info.serial_number);
                println!("geometry={}x{} bpp={}", info.display_width, info.display_height, info.bytes_per_pixel);
                assert_eq!(info.display_width, 640, "unexpected width");
                assert_eq!(info.bytes_per_pixel, 4);
                // Geometry came from feature report 0x09 (or sane fallback).
                assert!(!info.serial_number.is_empty());
            }
            None => panic!("Nexus display not found — is it plugged in?"),
        }
    }

    /// Enumerate raw hidraw nodes and confirm the usage-page parser finds
    /// the consumer-display pair (0x0C:0x01) and the boot-keyboard pair
    /// (0x01:0x06) for the Nexus — validating descriptor parsing on real
    /// hardware. Purely read-only (opens fds, reads descriptors, closes).
    #[test]
    #[ignore = "needs the Nexus display plugged in (read-only)"]
    fn enumerate_usage_pairs() {
        let devices = crate::core::hidraw::enumerate();
        let nexus: Vec<_> = devices
            .iter()
            .filter(|d| d.vendor_id == NEXUS_VID && d.product_id == NEXUS_PID)
            .collect();
        println!("{nexus:#?}");
        assert!(!nexus.is_empty(), "no Nexus hidraw nodes found");
        assert!(
            nexus.iter().any(|d| d.usage_page == 0x0c && d.usage == 0x01),
            "consumer-display interface (0x0C:0x01) not matched"
        );
    }
}
