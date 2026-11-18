//! Corsair K100 hardware access — pure-Rust port of `k100helper.py`.
//!
//! * discovery: hidraw nodes for USB interfaces 0/1/2 plus the K100 evdev node
//! * the Bragi jumbo (1024-byte) command channel on interface 1 — mode
//!   get/set (hardware mode double-sent, the ckb-next rollback pattern) and
//!   the 50 s keepalive while in software mode
//! * the IFC2 G-key bitmap reader on interface 2: `00 02` reports, first
//!   packet = baseline (no emit), edge detection on bits `gbit0..gbit0+5`
//!   (default 147 → byte 18 bit 3 … byte 19 bit 0), and raw packet streaming
//!   while a calibration watch is active
//!
//! Threading: the manager thread owns the Bragi channel; the reader owns
//! its own fd and thread, watching shared atomic/mutex state for path,
//! listen flag, watch deadline and G-bit offset changes.
//!
//! `unsafe` is confined to the thin fd wrappers (`open_hidraw`,
//! `wait_readable`, `write_all`, `read_packet`) — poll/read/write with
//! documented EINTR/EAGAIN handling.  EVIOCGRAB and uinput go through the
//! evdev crate's safe API in `record.rs` / `play.rs`.
//!
//! NOTE on `CoreEvent::Ifc2`: `k100helper.py` emits `data.hex()` — the FULL
//! packet, no slicing (`{"evt":"ifc2", …, "hex": data.hex()}`).  The
//! calibration UI (`bitsFromHex` in CalibrationDialog.tsx) skips the two
//! header bytes itself while computing *absolute* bit indices (byte 2 →
//! bits 16..23), which must line up with this module's absolute `gbit0`
//! indexing — so the full-packet hex is what keeps both sides consistent.

use super::manager::Internal;
use crate::core::types::{GkeyMode, GKEY_COUNT};
use crossbeam_channel::Sender;
use std::ffi::CString;
use std::fmt::Write as _;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ─── protocol constants (k100helper.py) ───────────────────────────

pub(crate) const VID: &str = "1B1C";
pub(crate) const PID: &str = "1B7C";
/// Bragi frames are padded to this size (the firmware's jumbo report).
pub(crate) const JUMBO: usize = 1024;
/// IFC2 reports read from USB interface 2.
pub(crate) const IFC2_PKT: usize = 64;
/// Keepalive cadence while the keyboard is in software mode.
pub(crate) const KEEPALIVE_S: u64 = 50;
/// A recording may never outlive its UI (`REC_MAX_S`).
pub(crate) const REC_MAX_S: u64 = 600;
/// Monitor tick: discovery rescan + recording watchdog + keepalive.
pub(crate) const MONITOR_TICK_S: u64 = 2;
/// Bragi reply deadline (Python `xfer(wait=3.0)`).
const XFER_WAIT: Duration = Duration::from_secs(3);
/// G-reader poll interval (Python `select(…, 0.2)`).
const READER_POLL: Duration = Duration::from_millis(200);

// ─── discovery ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Devices {
    /// hidraw nodes for USB interfaces 0 (input), 1 (Bragi), 2 (IFC2).
    pub ifaces: [Option<String>; 3],
    /// The keyboard's evdev node (name contains "K100", not "Mouse").
    pub evdev: Option<String>,
}

/// Port of `k100helper.discover()`.  The interface number comes from the
/// parent (USB interface) directory name via Python's `:\d+\.(\d+)$`
/// regex — see [`iface_number`].
pub(crate) fn discover() -> Devices {
    let mut ifaces: [Option<String>; 3] = [None, None, None];
    let prefix = format!("0003:{VID}:{PID}.");
    if let Ok(rd) = std::fs::read_dir("/sys/bus/hid/devices") {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for d in entries {
            let Some(name) = d.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with(&prefix) {
                continue;
            }
            // glob <device>/hidraw/hidraw* → first (sorted) node, like Python
            let node = std::fs::read_dir(d.join("hidraw"))
                .ok()
                .and_then(|hr| {
                    let mut nodes: Vec<_> = hr
                        .filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.starts_with("hidraw"))
                        })
                        .collect();
                    nodes.sort();
                    nodes.into_iter().next()
                });
            let Some(node) = node else {
                continue;
            };
            let Some(fname) = node.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // parent of the canonicalized device dir = the USB interface dir
            let Ok(real) = std::fs::canonicalize(&d) else {
                continue; // vanished mid-scan
            };
            let Some(parent) = real.parent() else {
                continue;
            };
            let Some(num) = iface_number(&parent.to_string_lossy()) else {
                continue;
            };
            if (num as usize) < ifaces.len() {
                ifaces[num as usize] = Some(format!("/dev/{fname}"));
            }
        }
    }

    // evdev node: device name contains "K100" but not "Mouse"; the last
    // sorted match wins, exactly like Python's loop.
    let mut evdev = None;
    if let Ok(rd) = std::fs::read_dir("/sys/class/input") {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            let Some(n) = e.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !n.starts_with("event") {
                continue;
            }
            let Ok(name) = std::fs::read_to_string(e.join("device/name")) else {
                continue;
            };
            if name.contains("K100") && !name.contains("Mouse") {
                evdev = Some(format!("/dev/input/{n}"));
            }
        }
    }
    Devices { ifaces, evdev }
}

/// Interface number from a sysfs parent path, Python's
/// `re.search(r":\d+\.(\d+)$", parent)`.  A match must start at a ':' and
/// run `<digits>.<digits>` to the end of the string — since `\d` cannot
/// contain ':', only the *last* ':' of the path can ever start a match, so
/// `rfind(':')` + an ASCII-digit parse is exactly equivalent (and there is
/// no regex crate available).
pub(crate) fn iface_number(parent: &str) -> Option<u32> {
    let idx = parent.rfind(':')?;
    let rest = &parent[idx + 1..];
    let dot = rest.find('.')?;
    let (head, frac) = (&rest[..dot], &rest[dot + 1..]);
    let ascii_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !ascii_digits(head) || !ascii_digits(frac) {
        return None;
    }
    frac.parse().ok()
}

// ─── raw fd wrappers (the only unsafe in this module) ─────────────

/// Open a hidraw node without blocking: `O_RDWR` for the Bragi channel,
/// `O_RDONLY` for IFC2 — the Python helper's open flags.
pub(crate) fn open_hidraw(path: &str, writable: bool) -> std::io::Result<OwnedFd> {
    let flags = if writable { libc::O_RDWR } else { libc::O_RDONLY } | libc::O_NONBLOCK;
    let cpath =
        CString::new(path).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `cpath` is a valid NUL-terminated string; a negative return is
    // checked below, a non-negative one is a fresh descriptor we own and
    // wrap immediately.
    let fd = unsafe { libc::open(cpath.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` was just validated as a descriptor we own exclusively.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(crate) enum PollOutcome {
    Ready,
    TimedOut,
    Failed(std::io::Error),
}

/// Wait until `fd` is readable or `timeout` elapses — the poll(2)
/// equivalent of the helper's `select([fd], [], [], t)`.  `POLLERR`/
/// `POLLHUP`/`POLLNVAL` surface as `Failed`; `EINTR` recomputes the
/// remaining time against a monotonic deadline so the overall timeout
/// stays accurate.
pub(crate) fn wait_readable(fd: RawFd, timeout: Duration) -> PollOutcome {
    let now = Instant::now();
    let deadline = now.checked_add(timeout).unwrap_or(now);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        // round a sub-millisecond remainder up so truncation never busy-spins
        let ms = if ms == 0 && !remaining.is_zero() { 1 } else { ms };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single stack pollfd, valid fd, kernel fills `revents`.
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                if Instant::now() >= deadline {
                    return PollOutcome::TimedOut;
                }
                continue;
            }
            return PollOutcome::Failed(e);
        }
        if r == 0 {
            if Instant::now() >= deadline {
                return PollOutcome::TimedOut;
            }
            continue; // rounded-down millisecond — recheck against deadline
        }
        let re = pfd.revents;
        if re & libc::POLLIN != 0 {
            return PollOutcome::Ready;
        }
        if re & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return PollOutcome::Failed(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("poll revents {re:#x}"),
            ));
        }
        if Instant::now() >= deadline {
            return PollOutcome::TimedOut;
        }
    }
}

/// Write the whole buffer: `EINTR` retried, partial writes resumed, and
/// `EAGAIN` (the fd is non-blocking) reported as an error — Python's
/// single `os.write` treats `BlockingIOError` the same way.
fn write_all(fd: RawFd, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: `buf` points at valid memory of length `buf.len()`.
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => continue,
                _ => return Err(e),
            }
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "bragi write made no progress",
            ));
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// One read: `Ok(Some(bytes))` with data, `Ok(Some(empty))` on EOF,
/// `Ok(None)` on `EAGAIN` (retry), `Err` otherwise.  `EINTR` retries.
fn read_some(fd: RawFd, buf: &mut [u8]) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        // SAFETY: `buf` is valid and sized for the read.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            return Ok(Some(buf[..n as usize].to_vec()));
        }
        if n == 0 {
            return Ok(Some(Vec::new()));
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return Ok(None),
            _ => return Err(e),
        }
    }
}

// ─── Bragi command channel (USB interface 1) ──────────────────────

/// Reply to a get-mode query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeReply {
    Sw,
    Hw,
    Unknown(u8),
}

impl ModeReply {
    pub(crate) fn as_mode(self) -> Option<GkeyMode> {
        match self {
            ModeReply::Sw => Some(GkeyMode::Sw),
            ModeReply::Hw => Some(GkeyMode::Hw),
            ModeReply::Unknown(_) => None,
        }
    }

    /// Wire text for status/error payloads: `"sw" | "hw" | "unknown(n)"`
    /// (the helper's `verified` field / TS `mode switch failed (…)` text).
    pub(crate) fn as_str(self) -> String {
        match self {
            ModeReply::Sw => "sw".to_string(),
            ModeReply::Hw => "hw".to_string(),
            ModeReply::Unknown(n) => format!("unknown({n})"),
        }
    }
}

/// Outcome of a Bragi SET-mode, mirroring what `k100helper.set_mode`
/// reported to the host (`ok` + `verified`).
pub(crate) struct SetModeOutcome {
    pub ok: bool,
    pub verified: Option<ModeReply>,
}

impl SetModeOutcome {
    /// The known hw/sw reply, if any (drives `MacroStatus.mode`).
    pub(crate) fn verified_mode(&self) -> Option<GkeyMode> {
        self.verified.and_then(ModeReply::as_mode)
    }

    /// Detail string for `mode switch failed (…)` — the host's
    /// `err || verified || "no reply"` fallback chain.
    pub(crate) fn error_detail(&self) -> String {
        self.verified
            .map(ModeReply::as_str)
            .unwrap_or_else(|| "no reply".to_string())
    }
}

/// Bragi jumbo (1024-byte) command channel.
pub(crate) struct Bragi {
    fd: Option<OwnedFd>,
    /// Used for `bragi write failed: …` log lines (Python's `log()` in xfer).
    log: Sender<Internal>,
}

impl Bragi {
    pub(crate) fn new(log: Sender<Internal>) -> Self {
        Bragi { fd: None, log }
    }

    pub(crate) fn alive(&self) -> bool {
        self.fd.is_some()
    }

    pub(crate) fn open(&mut self, path: &str) -> std::io::Result<()> {
        self.close();
        self.fd = Some(open_hidraw(path, true)?);
        Ok(())
    }

    pub(crate) fn close(&mut self) {
        self.fd = None;
    }

    /// Pad `payload` into a jumbo frame, write it, and wait up to `wait`
    /// for one reply.  Write failures are logged (Python's
    /// `bragi write failed: …`); a missing/late reply is a silent `None`.
    pub(crate) fn xfer(&mut self, payload: &[u8], wait: Duration) -> Option<Vec<u8>> {
        let fd = self.fd.as_ref()?; // fd None → silent None, like Python
        let frame = jumbo_frame(payload);
        if let Err(e) = write_all(fd.as_raw_fd(), &frame) {
            let _ = self.log.send(Internal::Log {
                level: crate::core::events::LogLevel::Error,
                msg: format!("bragi write failed: {e}"),
            });
            return None;
        }
        let now = Instant::now();
        let deadline = now.checked_add(wait).unwrap_or(now);
        let mut buf = [0u8; JUMBO];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match wait_readable(fd.as_raw_fd(), remaining.min(Duration::from_millis(100))) {
                PollOutcome::TimedOut => continue,
                PollOutcome::Failed(e) => {
                    // select/poll failure (device gone) → None, like Python,
                    // but surfaced as a log line for diagnosis
                    let _ = self.log.send(Internal::Log {
                        level: crate::core::events::LogLevel::Error,
                        msg: format!("bragi read failed: {e}"),
                    });
                    return None;
                }
                PollOutcome::Ready => {}
            }
            match read_some(fd.as_raw_fd(), &mut buf) {
                Ok(Some(data)) if data.is_empty() => continue, // EOF — keep waiting
                Ok(Some(data)) => return Some(data),
                Ok(None) => continue, // EAGAIN: retry until the deadline
                Err(_) => return None,
            }
        }
    }

    /// `08 02 03 00` → current mode: reply byte[2] == 0 and byte[3] is
    /// 2 → sw, 1 → hw.
    pub(crate) fn get_mode(&mut self) -> Option<ModeReply> {
        let r = self.xfer(&[0x08, 0x02, 0x03, 0x00], XFER_WAIT)?;
        if r.len() > 3 && r[2] == 0 {
            Some(match r[3] {
                2 => ModeReply::Sw,
                1 => ModeReply::Hw,
                other => ModeReply::Unknown(other),
            })
        } else {
            None
        }
    }

    /// `08 01 03 00 <val> 00` with val 02 = sw / 01 = hw.  Hardware mode is
    /// sent twice (the ckb-next rollback pattern) and, with `verify`, read
    /// back — Python `Bragi.set_mode`, whose host-facing `mode` event the
    /// manager now emits from this returned outcome.
    pub(crate) fn set_mode(&mut self, mode: GkeyMode, verify: bool) -> SetModeOutcome {
        let val: u8 = if mode == GkeyMode::Sw { 0x02 } else { 0x01 };
        let payload = [0x08, 0x01, 0x03, 0x00, val, 0x00];
        if self.xfer(&payload, XFER_WAIT).is_none() {
            return SetModeOutcome {
                ok: false,
                verified: None,
            };
        }
        if mode == GkeyMode::Hw {
            let _ = self.xfer(&payload, XFER_WAIT); // double-send rollback
        }
        if !verify {
            return SetModeOutcome {
                ok: true,
                verified: None,
            };
        }
        let got = self.get_mode();
        let ok = got
            == Some(match mode {
                GkeyMode::Sw => ModeReply::Sw,
                GkeyMode::Hw => ModeReply::Hw,
            });
        SetModeOutcome {
            ok,
            verified: got,
        }
    }

    /// `08 12` — keep the firmware in software mode (every 50 s).
    pub(crate) fn keepalive(&mut self) {
        let _ = self.xfer(&[0x08, 0x12], Duration::from_secs(1));
    }
}

impl Drop for Bragi {
    fn drop(&mut self) {
        self.close();
    }
}

/// Pad a Bragi payload into a 1024-byte jumbo frame (Python's
/// `pkt = bytearray(JUMBO); pkt[:len(payload)] = payload`).  Payloads are
/// at most 6 bytes in practice; longer input is truncated rather than
/// panicking (the Python array would silently grow instead).
pub(crate) fn jumbo_frame(payload: &[u8]) -> [u8; JUMBO] {
    let mut frame = [0u8; JUMBO];
    let n = payload.len().min(JUMBO);
    frame[..n].copy_from_slice(&payload[..n]);
    frame
}

/// True when `elapsed` since the last tick reaches the keepalive cadence
/// (Python's `keepalive_loop` `sleep(KEEPALIVE_S)`).
pub(crate) fn keepalive_due(elapsed: Duration) -> bool {
    elapsed.as_secs_f64() >= KEEPALIVE_S as f64
}

// ─── IFC2 G-key decoding (pure, unit-tested) ──────────────────────

/// Bitmap decode state: the first valid packet seeds a baseline without
/// emitting (a key may already be held); afterwards only edges are
/// reported.  Reset whenever the node or the bit offset changes.
#[derive(Debug, Default, Clone)]
pub(crate) struct GState {
    pub baseline_done: bool,
    pub last: [bool; GKEY_COUNT],
}

impl GState {
    pub(crate) fn reset(&mut self) {
        *self = GState::default();
    }
}

/// Extract one bit from an IFC2 packet (Python `GReader._bit`): out-of-range
/// bits are `false`.  G1 sits at `gbit0` (default 147 = byte 18, bit 3) and
/// G1..G6 occupy six consecutive bits — byte18 bits 3..7 plus byte19 bit 0.
pub(crate) fn gbit(data: &[u8], bit: u32) -> bool {
    let Some(&byte) = data.get((bit >> 3) as usize) else {
        return false;
    };
    (byte >> (bit & 7)) & 1 == 1
}

/// Decode one IFC2 report into `(g, down)` edge changes (g is 1..=6).
/// Short packets and packets without the `00 02` trigger header are
/// ignored entirely (state untouched); the first valid packet only seeds
/// the baseline and yields no events.
pub(crate) fn decode_gkeys(data: &[u8], gbit0: u32, state: &mut GState) -> Vec<(usize, bool)> {
    if data.len() < 2 || data[0] != 0x00 || data[1] != 0x02 {
        return Vec::new();
    }
    if !state.baseline_done {
        for (i, slot) in state.last.iter_mut().enumerate() {
            *slot = gbit(data, gbit0 + i as u32);
        }
        state.baseline_done = true;
        return Vec::new();
    }
    let mut edges = Vec::new();
    for (i, slot) in state.last.iter_mut().enumerate() {
        let down = gbit(data, gbit0 + i as u32);
        if *slot != down {
            *slot = down;
            edges.push((i + 1, down));
        }
    }
    edges
}

/// Lowercase hex dump of a packet — Python's `bytes.hex()`.
pub(crate) fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(s, "{b:02x}"); // writing to a String cannot fail
    }
    s
}

/// Unix seconds rounded to 3 decimals — Python `round(time.time(), 3)`.
pub(crate) fn unix_time_rounded() -> f64 {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    round3(t)
}

fn round3(t: f64) -> f64 {
    (t * 1000.0).round() / 1000.0
}

// ─── IFC2 G-key reader thread (USB interface 2) ───────────────────

/// State shared between the manager and the reader thread.  The manager
/// mutates it; the reader polls it every iteration (like Python's fields
/// written from other threads).
struct ReaderShared {
    /// hidraw node the thread should be attached to (`None` = detached).
    path: Mutex<Option<String>>,
    /// Press streaming for the keycap visualization — always on (the
    /// host's `listen: true`), checked before `GKey` events are emitted.
    listen: AtomicBool,
    gbit0: AtomicU32,
    /// Bitmap decode state; reset when the node or bit offset changes.
    state: Mutex<GState>,
    /// Watch deadline for raw `Ifc2` streaming (`None` = off).
    watch_until: Mutex<Option<Instant>>,
    stop: AtomicBool,
}

pub(crate) struct Reader {
    shared: Arc<ReaderShared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Reader {
    /// Spawn the reader thread against `path` (may be `None` while the
    /// IFC2 interface is absent; the thread then idles and picks the path
    /// up when [`Reader::set_path`] supplies one).
    pub(crate) fn start(path: Option<String>, gbit0: u32, int_tx: Sender<Internal>) -> Self {
        let shared = Arc::new(ReaderShared {
            path: Mutex::new(path),
            listen: AtomicBool::new(true),
            gbit0: AtomicU32::new(gbit0),
            state: Mutex::new(GState::default()),
            watch_until: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let arc = Arc::clone(&shared);
        let thread = match std::thread::Builder::new()
            .name("k100-ifc2".to_string())
            .spawn(move || reader_loop(arc, int_tx))
        {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("[k100] failed to spawn IFC2 reader: {e}");
                None
            }
        };
        Reader {
            shared,
            thread: Mutex::new(thread),
        }
    }

    /// Point the reader at a (possibly new) hidraw node; it closes the old
    /// fd and reopens by itself.
    pub(crate) fn set_path(&self, path: Option<String>) {
        let mut p = lock(&self.shared.path);
        *p = path;
    }

    /// Recalibrate the G-bit offset: update the counter and re-seed the
    /// baseline (Python's `set-gbits`).
    pub(crate) fn set_gbit0(&self, bit0: u32) {
        self.shared.gbit0.store(bit0, Ordering::Relaxed);
        lock(&self.shared.state).reset();
    }

    /// Stream raw IFC2 packets for `seconds` (0 stops) — Python's `watch`.
    pub(crate) fn set_watch(&self, seconds: u64) {
        let mut w = lock(&self.shared.watch_until);
        *w = if seconds == 0 {
            None
        } else {
            Instant::now().checked_add(Duration::from_secs(seconds))
        };
    }

    /// Stop the thread and join it (bounded by the 200 ms poll / 100 ms
    /// idle sleep).
    pub(crate) fn shutdown(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let handle = {
            let mut g = lock(&self.thread);
            g.take()
        };
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// The reader loop: keep `fd` in sync with the desired path, poll 200 ms,
/// read 64-byte reports and decode them.  A failed open logs once per
/// node; a read/select failure reports "device lost" once until the node
/// is successfully reopened (the manager's rescan supplies new paths).
fn reader_loop(shared: Arc<ReaderShared>, int_tx: Sender<Internal>) {
    let mut fd: Option<OwnedFd> = None;
    let mut fd_path: Option<String> = None;
    let mut lost = false; // "device lost" reported since the last good open
    let mut open_failed_for: Option<String> = None; // open error already logged
    while !shared.stop.load(Ordering::Relaxed) {
        let want = { lock(&shared.path).clone() };
        let needs_change = match (&fd, &want) {
            (None, None) => false,
            (Some(_), None) => true,                      // detach
            (None, Some(_)) => true,                      // attach
            (Some(_), Some(p)) => fd_path.as_deref() != Some(p.as_str()), // moved
        };
        if needs_change {
            fd = None; // OwnedFd drop closes the old node
            fd_path = None;
            if let Some(p) = &want {
                match open_hidraw(p, false) {
                    Ok(f) => {
                        fd = Some(f);
                        fd_path = Some(p.clone());
                        lost = false;
                        open_failed_for = None;
                        // Python resets the baseline on every (re)open
                        lock(&shared.state).reset();
                    }
                    Err(e) => {
                        // log once per node (Python logs per open attempt;
                        // once avoids repeating every 100 ms retry here)
                        if open_failed_for.as_deref() != Some(p.as_str()) {
                            open_failed_for = Some(p.clone());
                            let _ = int_tx.send(Internal::Log {
                                level: crate::core::events::LogLevel::Error,
                                msg: format!("open ifc2 interface failed: {e}"),
                            });
                        }
                    }
                }
            }
        }
        let Some(raw) = fd.as_ref().map(|f| f.as_raw_fd()) else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        match wait_readable(raw, READER_POLL) {
            PollOutcome::TimedOut => continue,
            PollOutcome::Failed(_) => {
                // Python: select OSError → close + device_lost("ifc2")
                fd = None;
                fd_path = None;
                if !lost {
                    lost = true;
                    let _ = int_tx.send(Internal::DeviceLost {
                        what: "ifc2".to_string(),
                    });
                }
            }
            PollOutcome::Ready => match read_some(raw, &mut [0u8; IFC2_PKT]) {
                Ok(None) => continue,                    // EAGAIN — retry
                Ok(Some(data)) if data.is_empty() => continue, // EOF
                Ok(Some(data)) => handle_packet(&shared, &int_tx, &data),
                Err(_) => {
                    // Python: read OSError → close + device_lost("ifc2")
                    fd = None;
                    fd_path = None;
                    if !lost {
                        lost = true;
                        let _ = int_tx.send(Internal::DeviceLost {
                            what: "ifc2".to_string(),
                        });
                    }
                }
            },
        }
    }
    drop(fd); // exit path: close the node (Python helper.shutdown)
}

/// Handle one IFC2 report: stream it while a watch is active, then decode
/// G-key edges (emitted only when press streaming is on).
fn handle_packet(shared: &ReaderShared, int_tx: &Sender<Internal>, data: &[u8]) {
    // Watch stream first, exactly like Python's `handle()` — the header
    // check happens afterwards, so non-trigger packets stream too.
    let watching = {
        let w = lock(&shared.watch_until);
        matches!(*w, Some(deadline) if Instant::now() < deadline)
    };
    if watching {
        let _ = int_tx.send(Internal::Ifc2 {
            t: unix_time_rounded(),
            hex: hex_encode(data),
        });
    }
    let gbit0 = shared.gbit0.load(Ordering::Relaxed);
    let edges = {
        let mut state = lock(&shared.state);
        decode_gkeys(data, gbit0, &mut state)
    };
    if edges.is_empty() || !shared.listen.load(Ordering::Relaxed) {
        return;
    }
    for (g, down) in edges {
        let _ = int_tx.send(Internal::GKey { g, down });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_interval_math() {
        assert!(!keepalive_due(Duration::from_secs(49)));
        assert!(!keepalive_due(Duration::from_secs(49) + Duration::from_millis(999)));
        assert!(keepalive_due(Duration::from_secs(50)));
        assert!(keepalive_due(Duration::from_secs(120)));
        assert!(!keepalive_due(Duration::ZERO));
    }

    #[test]
    fn jumbo_frames_pad_to_1024() {
        let f = jumbo_frame(&[0x08, 0x12]);
        assert_eq!(f.len(), 1024);
        assert_eq!(&f[..2], &[0x08, 0x12]);
        assert!(f[2..].iter().all(|&b| b == 0));
        let f = jumbo_frame(&[0x08, 0x01, 0x03, 0x00, 0x02, 0x00]);
        assert_eq!(&f[..6], &[0x08, 0x01, 0x03, 0x00, 0x02, 0x00]);
        assert_eq!(f[6], 0);
        // oversized payload is truncated, never panics
        assert_eq!(jumbo_frame(&[0xAA; 4096]).len(), 1024);
    }

    #[test]
    fn iface_number_matches_python_regex() {
        // real K100 parent: …/8-2.4/8-2.4:1.1 → interface 1
        assert_eq!(iface_number("/sys/devices/pci0000:00/0000:00:02.1/usb8/8-2/8-2.4/8-2.4:1.1"), Some(1));
        assert_eq!(iface_number("/sys/devices/x/8-2.4:1.0"), Some(0));
        assert_eq!(iface_number("/sys/devices/x/8-2.4:1.2"), Some(2));
        assert_eq!(iface_number("/x:1.10"), Some(10));
        // the hid device dir itself never matches (letters in "1B7C")
        assert_eq!(iface_number("/sys/devices/x/0003:1B1C:1B7C.0020"), None);
        // no colon / no dot / non-digits → no match
        assert_eq!(iface_number("/sys/bus/hid/devices"), None);
        assert_eq!(iface_number("/a/b"), None);
        assert_eq!(iface_number("/x:abc.def"), None);
        assert_eq!(iface_number("/x:1."), None);
    }

    #[test]
    fn gbit_extraction_default_offset() {
        let mut pkt = [0u8; IFC2_PKT];
        // G1 = bit 147 → byte 18, bit 3
        pkt[18] = 1 << 3;
        assert!(gbit(&pkt, 147));
        assert!(!gbit(&pkt, 148));
        // G6 = bit 152 → byte 19, bit 0
        pkt[19] = 1;
        assert!(gbit(&pkt, 152));
        // out-of-range bits are false (Python's bounds check)
        assert!(!gbit(&[0u8; 1], 147));
        assert!(!gbit(&[], 147));
    }

    #[test]
    fn decode_gkeys_header_baseline_edges() {
        let mut pkt = [0u8; IFC2_PKT];
        pkt[0] = 0x00;
        pkt[1] = 0x02;
        pkt[18] = 1 << 3; // bit 147 → G1 held at open time

        let mut st = GState::default();
        // first packet = baseline, no emit (key may already be held)
        assert!(decode_gkeys(&pkt, 147, &mut st).is_empty());
        assert!(st.baseline_done);
        assert!(st.last[0]); // G1 recorded as down
        // unchanged → no edges
        assert!(decode_gkeys(&pkt, 147, &mut st).is_empty());

        // G1 released, G2 pressed in one report
        pkt[18] = 1 << 4;
        assert_eq!(decode_gkeys(&pkt, 147, &mut st), vec![(1, false), (2, true)]);

        // G2 released, G6 (byte 19, bit 0) pressed
        pkt[18] = 0;
        pkt[19] = 1;
        assert_eq!(decode_gkeys(&pkt, 147, &mut st), vec![(2, false), (6, true)]);

        // wrong header → ignored entirely, state untouched
        let snapshot = st.clone();
        pkt[1] = 0x03;
        pkt[18] = 0xFF;
        assert!(decode_gkeys(&pkt, 147, &mut st).is_empty());
        assert_eq!(st.last, snapshot.last);

        // truncated body with a valid header: out-of-range bits read as
        // false, exactly like Python's bounds check — a held bit (G6)
        // reports its release edge rather than being dropped
        assert_eq!(decode_gkeys(&[0x00, 0x02], 147, &mut st), vec![(6, false)]);

        // packets shorter than the 2-byte header → ignored entirely
        assert!(decode_gkeys(&[0x00], 147, &mut st).is_empty());
        assert!(decode_gkeys(&[], 147, &mut st).is_empty());
    }

    #[test]
    fn state_reset_reseeds_baseline() {
        let mut st = GState {
            baseline_done: true,
            last: [true; GKEY_COUNT],
        };
        st.reset();
        assert!(!st.baseline_done);
        assert!(st.last.iter().all(|&b| !b));
    }

    #[test]
    fn hex_encode_matches_python_bytes_hex() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_encode(&[0x00, 0x0f]), "000f");
        assert_eq!(hex_encode(&[0x00, 0x02, 0x80]), "000280");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn time_rounded_to_milliseconds() {
        assert_eq!(round3(1.23456), 1.235);
        assert_eq!(round3(1.2344), 1.234);
        assert_eq!(round3(0.0005), 0.001);
        let t = unix_time_rounded();
        assert!(t > 1_700_000_000.0); // sane unix epoch value
        assert_eq!(t, (t * 1000.0).round() / 1000.0); // 3 decimals only
    }

    #[test]
    fn mode_reply_error_text_matches_host_chain() {
        let out = SetModeOutcome {
            ok: false,
            verified: Some(ModeReply::Hw),
        };
        assert_eq!(out.error_detail(), "hw");
        assert_eq!(out.verified_mode(), Some(GkeyMode::Hw));
        let out = SetModeOutcome {
            ok: false,
            verified: Some(ModeReply::Unknown(3)),
        };
        assert_eq!(out.error_detail(), "unknown(3)");
        assert_eq!(out.verified_mode(), None);
        let out = SetModeOutcome {
            ok: false,
            verified: None,
        };
        assert_eq!(out.error_detail(), "no reply"); // TS fallback text
    }

    /// Hardware probe (read-only): discovery + a single Bragi get-mode.
    /// Run explicitly with `cargo test -- --ignored` with the K100 plugged in.
    #[test]
    #[ignore = "needs the K100 plugged in"]
    fn hardware_discovery_and_get_mode() {
        let (tx, _rx) = crossbeam_channel::unbounded::<Internal>();
        let dev = discover();
        assert!(dev.ifaces[1].is_some(), "cmd interface not found: {:?}", dev.ifaces);
        assert!(dev.ifaces[0].is_some() && dev.ifaces[2].is_some(), "{:?}", dev.ifaces);
        assert!(dev.evdev.is_some(), "evdev node not found");
        let mut bragi = Bragi::new(tx);
        bragi.open(dev.ifaces[1].as_ref().unwrap()).expect("open cmd channel");
        let mode = bragi.get_mode().expect("no Bragi reply to get-mode");
        // read-only probe: verify it parses as a known reply shape
        let _ = mode.as_str();
    }
}
