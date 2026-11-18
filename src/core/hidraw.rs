//! Pure-Rust `hidraw` backend for Linux — a dependency-free replacement for
//! the hidapi layer that `node-hid` wraps. Mirrors hidapi's behavior exactly:
//!
//! * enumeration scans `/sys/class/hidraw/*/device/uevent` for `HID_ID`,
//!   `HID_NAME`, `HID_UNIQ` (same fields, same USB bus filter),
//! * usage page/usage come from parsing the report descriptor with the same
//!   top-level-collection algorithm hidapi uses (`get_next_hid_usage`),
//! * feature reports use the `HIDIOCGFEATURE`/`HIDIOCSFEATURE` ioctls with
//!   `buf[0] = report_id` exactly as node-hid does,
//! * output reports are a plain `write()` of the full report (report id in
//!   byte 0).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

// ─── ioctl plumbing ─────────────────────────────────────────────────

const IOC_NR_BITS: u32 = 8;
const IOC_TYPE_BITS: u32 = 8;
const IOC_SIZE_BITS: u32 = 14;
const IOC_NR_SHIFT: u32 = 0;
const IOC_TYPE_SHIFT: u32 = IOC_NR_SHIFT + IOC_NR_BITS;
const IOC_SIZE_SHIFT: u32 = IOC_TYPE_SHIFT + IOC_TYPE_BITS;
const IOC_DIR_SHIFT: u32 = IOC_SIZE_SHIFT + IOC_SIZE_BITS;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> libc::c_ulong {
    ((dir as libc::c_ulong) << IOC_DIR_SHIFT)
        | ((size as libc::c_ulong) << IOC_SIZE_SHIFT)
        | ((ty as libc::c_ulong) << IOC_TYPE_SHIFT)
        | ((nr as libc::c_ulong) << IOC_NR_SHIFT)
}

// From /usr/include/linux/hidraw.h
const HID_MAX_DESCRIPTOR_SIZE: usize = 4096;
const HIDIOCGRDESCSIZE: libc::c_ulong = ioc(IOC_READ, b'H', 0x01, 4);
const HIDIOCGRDESC: libc::c_ulong =
    ioc(IOC_READ, b'H', 0x02, 4 + HID_MAX_DESCRIPTOR_SIZE);
const fn hidioscfeature(len: usize) -> libc::c_ulong {
    ioc(IOC_READ | IOC_WRITE, b'H', 0x06, len)
}
const fn hidiocgfeature(len: usize) -> libc::c_ulong {
    ioc(IOC_READ | IOC_WRITE, b'H', 0x07, len)
}
#[allow(dead_code)]
const IOC_NONE_UNUSED: u32 = IOC_NONE; // keep the constant table honest

unsafe fn ioctl_retry(fd: libc::c_int, req: libc::c_ulong, arg: *mut u8) -> io::Result<isize> {
    loop {
        let ret = libc::ioctl(fd, req, arg as *mut libc::c_void);
        if ret >= 0 {
            return Ok(ret as isize);
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err);
    }
}

// ─── Enumeration ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct HidDeviceInfo {
    /// `/dev/hidrawN`
    pub path: PathBuf,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: String,
    pub product_name: String,
    pub usage_page: u16,
    pub usage: u16,
}

/// Parse a hid device `uevent` file (HID_ID / HID_NAME / HID_UNIQ).
/// Same success criteria as hidapi's `parse_uevent_info`: all three fields.
fn parse_uevent(contents: &str) -> Option<(u32, u16, u16, String, String)> {
    let mut bus = None;
    let mut vid = None;
    let mut pid = None;
    let mut name = None;
    let mut uniq = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            "HID_ID" => {
                // HID_ID=0003:00001B1C:00001B8E  (type vendor product)
                let mut it = value.split(':');
                let b = u32::from_str_radix(it.next()?, 16).ok()?;
                let v = u16::from_str_radix(it.next()?, 16).ok()?;
                let p = u16::from_str_radix(it.next()?, 16).ok()?;
                bus = Some(b);
                vid = Some(v);
                pid = Some(p);
            }
            "HID_NAME" => name = Some(value.to_string()),
            "HID_UNIQ" => uniq = Some(value.to_string()),
            _ => {}
        }
    }
    Some((bus?, vid?, pid?, name?, uniq?))
}

/// hidapi's bus filter: USB / Bluetooth / I2C / SPI only.
fn bus_supported(bus: u32) -> bool {
    matches!(bus, 0x03 /*USB*/ | 0x05 /*BLUETOOTH*/ | 0x18 /*I2C*/ | 0x1c /*SPI*/)
}

/// Scan all hidraw nodes and return one entry per top-level usage pair,
/// exactly like hidapi's `hid_enumerate`.
pub fn enumerate() -> Vec<HidDeviceInfo> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/hidraw") else {
        return out;
    };
    for entry in rd.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else { continue };
        if !name.starts_with("hidraw") {
            continue;
        }
        let Ok(uevent) = fs::read_to_string(entry.path().join("device").join("uevent")) else {
            continue;
        };
        let Some((bus, vid, pid, product, serial)) = parse_uevent(&uevent) else {
            continue;
        };
        if !bus_supported(bus) {
            continue;
        }
        let dev_path = PathBuf::from("/dev").join(name);
        // The report descriptor requires an open fd; without udev permissions
        // the node is unusable anyway, so skip it (matches node-hid, which
        // would also fail to open it).
        let Ok(handle) = HidDevice::open(&dev_path) else { continue };
        let Ok(desc) = handle.report_descriptor() else { continue };
        for (usage_page, usage) in top_level_usages(&desc) {
            out.push(HidDeviceInfo {
                path: dev_path.clone(),
                vendor_id: vid,
                product_id: pid,
                serial_number: serial.clone(),
                product_name: product.clone(),
                usage_page,
                usage,
            });
        }
    }
    out
}

// ─── Report descriptor parsing (port of hidapi's get_next_hid_usage) ─

/// HID item size table. Returns `(data_len, key_size)` exactly like hidapi:
/// `key_size` is the 1-byte item prefix, callers advance `pos += data_len + key_size`.
fn item_size(desc: &[u8], pos: usize) -> Option<(usize, usize)> {
    let key = *desc.get(pos)?;
    let size_code = (key & 0x03) as usize;
    let data_len = match size_code {
        0 | 1 | 2 => size_code,
        3 => 4,
        _ => return None,
    };
    Some((data_len, 1))
}

/// hidapi's `get_hid_report_bytes` (including its off-by-one bounds check,
/// kept verbatim so parsing matches node-hid for identical descriptors).
fn report_bytes(desc: &[u8], num_bytes: usize, cur: usize) -> u32 {
    if cur + num_bytes >= desc.len() || num_bytes == 0 {
        return 0;
    }
    match num_bytes {
        1 => desc[cur + 1] as u32,
        2 => (desc[cur + 2] as u32) * 256 + desc[cur + 1] as u32,
        4 => {
            (desc[cur + 4] as u32) * 0x0100_0000
                + (desc[cur + 3] as u32) * 0x0001_0000
                + (desc[cur + 2] as u32) * 0x0000_0100
                + (desc[cur + 1] as u32)
        }
        _ => 0,
    }
}

/// Skip to the end of the collection starting at `pos` (hidapi's
/// `hid_iterate_over_collection`). Returns the position OF the matching
/// End Collection item (hidapi leaves `*pos` there; the caller then advances
/// past it, which the outer loop does since End Collection is a no-op).
fn skip_collection(desc: &[u8], pos: usize) -> Option<usize> {
    let mut level: i32 = 0;
    let mut p = pos;
    while p < desc.len() {
        let key = desc[p];
        let key_cmd = key & 0xfc;
        let (data_len, key_size) = item_size(desc, p)?;
        match key_cmd {
            0xa0 => level += 1, // Collection
            0xc0 => level -= 1, // End Collection
            _ => {}
        }
        if level < 0 {
            return None; // End before begin — broken descriptor
        }
        if level == 0 {
            return Some(p);
        }
        p += data_len + key_size;
    }
    None
}

/// All top-level (usage page, usage) pairs of a report descriptor —
/// a faithful port of hidapi's `get_next_hid_usage` loop.
pub fn top_level_usages(desc: &[u8]) -> Vec<(u16, u16)> {
    let mut pairs = Vec::new();
    let mut pos = 0usize;
    let mut usage_page: u16 = 0;
    let mut usage_page_found = false;
    let mut usage: u16 = 0;
    let mut usage_found = false;

    while pos < desc.len() {
        let key = desc[pos];
        let key_cmd = key & 0xfc;
        let Some((data_len, key_size)) = item_size(desc, pos) else { break };
        match key_cmd {
            0x04 => {
                // Usage Page (Global)
                usage_page = report_bytes(desc, data_len, pos) as u16;
                usage_page_found = true;
            }
            0x08 => {
                // Usage (Local)
                if data_len == 4 {
                    // 5.5: 32-bit usage carries its own page in the high half
                    usage_page = (report_bytes(desc, 2, pos + 2)) as u16;
                    usage_page_found = true;
                    usage = report_bytes(desc, 2, pos) as u16;
                } else {
                    usage = report_bytes(desc, data_len, pos) as u16;
                }
                usage_found = true;
            }
            0xa0 => {
                // Collection (Main) — report the pair in scope at this point
                match skip_collection(desc, pos) {
                    Some(next) => {
                        if usage_found && usage_page_found {
                            pairs.push((usage_page, usage));
                            // hidapi starts a fresh get_next_hid_usage call
                            // after each reported pair, which resets the
                            // local usage flag (the page flag lives in the
                            // iterator struct and persists).
                            usage_found = false;
                        }
                        pos = next;
                        continue;
                    }
                    None => break, // malformed descriptor
                }
            }
            _ => {}
        }
        pos += data_len + key_size;
    }
    pairs
}

// ─── Open device ────────────────────────────────────────────────────

pub struct HidDevice {
    file: File,
}

impl HidDevice {
    pub fn open(path: &Path) -> io::Result<HidDevice> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(HidDevice { file })
    }

    /// Report descriptor bytes (`HIDIOCGRDESC`).
    pub fn report_descriptor(&self) -> io::Result<Vec<u8>> {
        let mut size: i32 = 0;
        let ret = unsafe {
            ioctl_retry(
                self.file.as_raw_fd(),
                HIDIOCGRDESCSIZE,
                &mut size as *mut i32 as *mut u8,
            )
        };
        ret?;
        if size <= 0 || size as usize > HID_MAX_DESCRIPTOR_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad descriptor size"));
        }
        // struct hidraw_report_descriptor { __u32 size; __u8 value[4096]; }
        let mut buf = vec![0u8; 4 + HID_MAX_DESCRIPTOR_SIZE];
        buf[..4].copy_from_slice(&(size as u32).to_ne_bytes());
        unsafe {
            ioctl_retry(self.file.as_raw_fd(), HIDIOCGRDESC, buf.as_mut_ptr())?;
        }
        Ok(buf[4..4 + size as usize].to_vec())
    }

    /// `node-hid` `getFeatureReport(report_id, length)`: buffer of `length`
    /// bytes with `buf[0] = report_id`, ioctl, return `ret` bytes.
    /// Byte 0 of the result is the report id, payload from index 1.
    pub fn get_feature_report(&self, report_id: u8, length: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; length];
        buf[0] = report_id;
        let ret = unsafe { ioctl_retry(self.file.as_raw_fd(), hidiocgfeature(length), buf.as_mut_ptr()) }?;
        let n = ret as usize;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty feature report"));
        }
        buf.truncate(n.min(length).max(1));
        Ok(buf)
    }

    /// `node-hid` `sendFeatureReport(data)` — `data[0]` is the report id.
    /// Returns bytes accepted by the kernel.
    pub fn send_feature_report(&self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty feature report"));
        }
        let mut buf = data.to_vec();
        let ret = unsafe { ioctl_retry(self.file.as_raw_fd(), hidioscfeature(buf.len()), buf.as_mut_ptr()) }?;
        Ok(ret as usize)
    }

    /// `node-hid` `write(data)` — full output report with report id in byte 0.
    pub fn write_report(&self, data: &[u8]) -> io::Result<usize> {
        use std::io::Write;
        // `&File` implements Write — no mutability needed (writes are
        // atomic per hidraw's single write() syscall semantics).
        let mut f = &self.file;
        let mut total = 0usize;
        while total < data.len() {
            match f.write(&data[total..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "hidraw write returned 0",
                    ))
                }
                Ok(n) => total += n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uevent_parses_nexus() {
        let s = "MAJOR=243\nMINOR=0\nDEVNAME=hidraw0\n\
                 HID_ID=0003:00001B1C:00001B8E\n\
                 HID_NAME=CORSAIR iCUE NEXUS\n\
                 HID_UNIQ=ABCDEF123\n";
        let (bus, vid, pid, name, uniq) = parse_uevent(s).unwrap();
        assert_eq!((bus, vid, pid), (0x0003, 0x1b1c, 0x1b8e));
        assert_eq!(name, "CORSAIR iCUE NEXUS");
        assert_eq!(uniq, "ABCDEF123");
        assert!(bus_supported(bus));
    }

    #[test]
    fn uevent_requires_all_fields() {
        assert!(parse_uevent("HID_ID=0003:00001B1C:00001B8E\nHID_NAME=x\n").is_none());
        assert!(parse_uevent("").is_none());
    }

    /// Minimal Consumer-Control descriptor: the pair node-hid matches
    /// (usage page 0x0C, usage 0x01) plus a boot-keyboard style pair.
    fn fixture() -> Vec<u8> {
        vec![
            0x05, 0x0c, // Usage Page (Consumer)
            0x09, 0x01, // Usage (Consumer Control)
            0xa1, 0x01, // Collection (Application)
            0x15, 0x00, //   Logical Minimum (0)
            0x26, 0xff, 0x00, //   Logical Maximum (255)
            0x75, 0x08, //   Report Size (8)
            0x95, 0x01, //   Report Count (1)
            0x09, 0x00, //   Usage (Undefined)
            0x81, 0x00, //   Input (Data,Array)
            0xc0,       // End Collection
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x06, // Usage (Keyboard)
            0xa1, 0x01, // Collection (Application)
            0xc0,       // End Collection
        ]
    }

    #[test]
    fn finds_toplevel_usage_pairs() {
        let pairs = top_level_usages(&fixture());
        assert_eq!(pairs, vec![(0x0c, 0x01), (0x01, 0x06)]);
    }

    #[test]
    fn nested_collections_report_once() {
        // Pair is reported at the TOP-level collection only (hidapi skips
        // nested content via skip_collection).
        let d = vec![
            0x05, 0x0d, // Usage Page (Digitizers)
            0x09, 0x04, // Usage (Touch Screen)
            0xa1, 0x01, // Collection (Application)
            0x09, 0x22, //   Usage (Finger)
            0xa1, 0x02, //   Collection (Physical)
            0xc0,       //   End Collection
            0xc0,       // End Collection
        ];
        let pairs = top_level_usages(&d);
        assert_eq!(pairs, vec![(0x0d, 0x04)]);
    }

    #[test]
    fn malformed_descriptor_does_not_panic() {
        assert!(top_level_usages(&[0xa1]).is_empty()); // collection w/o end
        assert!(top_level_usages(&[]).is_empty());
        assert!(top_level_usages(&[0xfe, 0xff]).is_empty()); // long item bytes
    }

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        // Values compiled from the kernel headers on this machine.
        assert_eq!(HIDIOCGRDESCSIZE, 0x8004_4801);
        assert_eq!(HIDIOCGRDESC, 0x9004_4802); // struct size 4100
        assert_eq!(hidiocgfeature(32), 0xc020_4807);
        assert_eq!(hidioscfeature(3), 0xc003_4806);
    }

    #[test]
    fn skip_collection_positions_at_end_item() {
        let d = fixture();
        // First Collection starts at index 4; its End Collection is at 19.
        assert_eq!(skip_collection(&d, 4), Some(19));
        // Second (empty) Collection at 24 ends at 26.
        assert_eq!(skip_collection(&d, 24), Some(26));
        // Positioned on a non-collection item: level stays 0 → immediate
        // "found end" (hidapi's behavior; harmless because the caller only
        // invokes this on Collection items).
        assert_eq!(skip_collection(&d, 0), Some(0));
        // Unterminated collection → None.
        assert_eq!(skip_collection(&[0xa1, 0x01, 0x09, 0x01], 0), None);
    }
}
