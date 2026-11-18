//! Pure-Rust port of `SensorReader` from
//! `k100display/electron/nexus/sensors.ts`.
//!
//! Like the TS original (which ran inside the Electron worker thread), all
//! reads are synchronous — this module is meant to run on the dedicated
//! display worker thread. IO failures are swallowed exactly like the TS
//! `try/catch` blocks: the reading is omitted, or 0 where TS defaults to 0.
//!
//! Readings are returned **without** severity — the worker attaches
//! severities afterwards.
//!
//! Deviations from the TS file are deliberate and only affect the *order* of
//! readings, never their ids/values: directory listings are sorted for
//! determinism (Node's `readdirSync` order is arbitrary), and the hwmon
//! device *scan* (which files exist) is memoized for 2 s — values themselves
//! are always read fresh from sysfs/proc.

use crate::core::types::{SensorCategory, SensorReading};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ─── helpers ────────────────────────────────────────────────────────

/// Synchronous file read with TS-style try/catch → empty string on failure.
fn read_file(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Read + trim (the TS code trims every sysfs/proc read).
fn read_trimmed(path: &str) -> String {
    read_file(path).trim().to_string()
}

/// JS-`parseInt`-alike for the common decimal case: optional sign plus
/// leading digits, stopping at the first non-digit — so `"45.0"` → 45,
/// `"2000 "` → 2000, `"abc"` / `""` → `None` (TS: `NaN`).
fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = match b.first() {
        Some(&b'-') | Some(&b'+') => 1,
        _ => 0,
    };
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    s[..i].parse::<i64>().ok()
}

/// JS-`parseFloat`-alike for the clean numeric payloads we consume
/// (nvidia-smi CSV, `/proc/uptime`, `/proc/loadavg`).
fn parse_float(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// JS `Math.round(x * 100) / 100`. (`Math.round` rounds half up; `f64::round`
/// rounds half away from zero — identical for the non-negative values these
/// formulas receive.)
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// JS `Math.round(x * 10) / 10`.
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Build a `SensorReading` without severity (the worker attaches it later).
fn reading(
    id: &str,
    label: &str,
    category: SensorCategory,
    value: f64,
    unit: &str,
    min: Option<f64>,
    max: Option<f64>,
    icon: &str,
) -> SensorReading {
    SensorReading {
        id: id.to_string(),
        label: label.to_string(),
        category,
        value,
        unit: unit.to_string(),
        min,
        max,
        icon: icon.to_string(),
        severity: None,
    }
}

/// Port of TS `formatSpeed` — auto-scaling bytes/sec with unit.
fn format_speed(bytes_per_sec: f64) -> (f64, &'static str) {
    let abs = bytes_per_sec.abs();
    if abs >= 1_073_741_824.0 {
        (round1(bytes_per_sec / 1_073_741_824.0), "GB/s")
    } else if abs >= 1_048_576.0 {
        (round1(bytes_per_sec / 1_048_576.0), "MB/s")
    } else if abs >= 1_024.0 {
        (round1(bytes_per_sec / 1_024.0), "KB/s")
    } else {
        (round1(bytes_per_sec), "B/s")
    }
}

/// TS `/proc/diskstats` predicate, verbatim:
/// `if (/\d$/.test(name) && !name.startsWith("nvme")) continue;`
/// `if (name === "loop" || name === "ram" || name.startsWith("loop")) continue;`
///
/// So `sda` is counted, `sda1` is not (partition), `nvme0n1p1` is counted
/// (nvme partitions are exempt), and `loop*`/`ram*` are always skipped.
fn counts_disk(name: &str) -> bool {
    let ends_digit = name
        .chars()
        .next_back()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    if ends_digit && !name.starts_with("nvme") {
        return false;
    }
    name != "loop" && name != "ram" && !name.starts_with("loop")
}

/// Sum read/written sectors (×512 = bytes handled by the caller) across all
/// counted devices in a `/proc/diskstats` dump.
fn sum_diskstats(raw: &str) -> (u64, u64) {
    let (mut read, mut write) = (0u64, 0u64);
    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 14 {
            continue;
        }
        if !counts_disk(parts[2]) {
            continue;
        }
        // parts[5] = sectors read, parts[9] = sectors written.
        read += parts[5].parse::<u64>().unwrap_or(0);
        write += parts[9].parse::<u64>().unwrap_or(0);
    }
    (read, write)
}

/// Parse the aggregate `cpu` line of `/proc/stat` into the 8 jiffy counters
/// (user, nice, system, idle, iowait, irq, softirq, steal). Mirrors TS
/// `parseCpuStat`: needs ≥ 9 fields and every remaining field numeric.
fn parse_cpu_stat(line: &str) -> Option<[u64; 8]> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 9 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len() - 1);
    for p in &parts[1..] {
        nums.push(p.parse::<u64>().ok()?);
    }
    let mut times = [0u64; 8];
    times.copy_from_slice(&nums[..8]);
    Some(times)
}

/// Port of the TS `getCpuUsage` delta math:
/// `Math.round((1 - idleDelta / totalDelta) * 10000) / 100`, or 0 when there
/// is no positive total delta (first sample, or a counter reset).
fn cpu_usage_delta(prev: [u64; 8], cur: [u64; 8]) -> f64 {
    let cur_total: i128 = cur.iter().map(|&x| x as i128).sum();
    let prev_total: i128 = prev.iter().map(|&x| x as i128).sum();
    let total_delta = cur_total - prev_total;
    let idle_delta =
        (cur[3] as i128 + cur[4] as i128) - (prev[3] as i128 + prev[4] as i128);
    if total_delta > 0 {
        ((1.0 - idle_delta as f64 / total_delta as f64) * 10000.0).round() / 100.0
    } else {
        0.0
    }
}

/// Parse one `/proc/meminfo` field (`^NAME:\s+(\d+)\s+kB`, multiline in TS).
fn meminfo_field(raw: &str, field: &str) -> Option<u64> {
    let prefix = format!("{field}:");
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix(prefix.as_str()) {
            let mut toks = rest.split_whitespace();
            let Some(value) = toks.next() else { continue };
            if toks.next() != Some("kB") {
                continue;
            }
            if let Ok(n) = value.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

/// Port of TS `getMemoryStats` — pure so it can be tested from a fixture.
fn compute_memory_stats(raw: &str) -> Vec<SensorReading> {
    let mut out = Vec::new();

    let mem_total = meminfo_field(raw, "MemTotal");
    let mem_available = meminfo_field(raw, "MemAvailable");
    let swap_total = meminfo_field(raw, "SwapTotal");
    let swap_free = meminfo_field(raw, "SwapFree");

    if let (Some(mem_total), Some(mem_available)) = (mem_total, mem_available) {
        if mem_total > 0 {
            let used_kb = mem_total.saturating_sub(mem_available);
            // TS: Math.round((usedKb / memTotal) * 10000) / 100
            let used_pct = ((used_kb as f64 / mem_total as f64) * 10000.0).round() / 100.0;
            let used_gb = round2(used_kb as f64 / 1_048_576.0);
            let free_gb = round2(mem_available as f64 / 1_048_576.0);
            let total_gb = round2(mem_total as f64 / 1_048_576.0);

            out.push(reading(
                "mem_used_pct",
                "RAM Usage",
                SensorCategory::Memory,
                used_pct,
                "%",
                Some(0.0),
                Some(100.0),
                "memory",
            ));
            out.push(reading(
                "mem_used_gb",
                "RAM Used",
                SensorCategory::Memory,
                used_gb,
                "GB",
                Some(0.0),
                Some(total_gb),
                "memory",
            ));
            out.push(reading(
                "mem_free_gb",
                "RAM Free",
                SensorCategory::Memory,
                free_gb,
                "GB",
                Some(0.0),
                Some(total_gb),
                "memory",
            ));
        }
    }

    if let (Some(swap_total), Some(swap_free)) = (swap_total, swap_free) {
        if swap_total > 0 {
            let swap_used = swap_total.saturating_sub(swap_free);
            let swap_pct =
                ((swap_used as f64 / swap_total as f64) * 10000.0).round() / 100.0;
            out.push(reading(
                "swap_used_pct",
                "Swap Usage",
                SensorCategory::Memory,
                swap_pct,
                "%",
                Some(0.0),
                Some(100.0),
                "swap",
            ));
        }
    }

    out
}

/// TS hwmon label sanitizing: `replace(/[^a-zA-Z0-9]/g, "_").toLowerCase()`.
fn sanitize_label(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Classify a hwmon entry like TS
/// `/^(temp|fan|in|power)(\d+)_input$/` → (kind, index).
fn parse_input_entry(entry: &str) -> Option<(&'static str, u32)> {
    let name = entry.strip_suffix("_input")?;
    for prefix in ["temp", "fan", "in", "power"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                return Some((prefix, rest.parse::<u32>().ok()?));
            }
        }
    }
    None
}

/// `^cpu\d+$` filter used by TS `getCpuClock`.
fn is_cpu_dir(name: &str) -> bool {
    match name.strip_prefix("cpu") {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Run a pipeline through `sh`, bounded by coreutils `timeout` (this replaces
/// Node's `execSync` timeout). `None` only when the command fails or times
/// out — the TS code falls through to the next package manager then; a
/// successful run with garbage output parses as 0 (`parseInt(...) || 0`),
/// stopping the chain, exactly like TS.
fn run_count(inner: &str, timeout_secs: u64) -> Option<u64> {
    let script = format!("timeout {timeout_secs} sh -c '{inner}'");
    let out = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(parse_int(text.trim()).unwrap_or(0).max(0) as u64)
}

/// Root filesystem usage via `statfs(2)` (libc), port of `fs.statfsSync("/")`.
fn root_disk_usage_pct() -> Option<f64> {
    let path = CString::new("/").ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(path.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let total = (st.f_blocks as u64).wrapping_mul(st.f_bsize as u64);
    let free = (st.f_bfree as u64).wrapping_mul(st.f_bsize as u64);
    if total == 0 {
        return Some(0.0);
    }
    let used = total.saturating_sub(free);
    // TS: Math.round((used / total) * 10000) / 100
    Some(((used as f64 / total as f64) * 10000.0).round() / 100.0)
}

// ─── hwmon scan (cached) ────────────────────────────────────────────

struct HwmonDevice {
    /// Directory name under `/sys/class/hwmon` (e.g. `hwmon3`).
    dir: String,
    /// Contents of the `name` file, falling back to the dir name (TS:
    /// `trim(readFile(name)) || dir`).
    name: String,
    /// File names in the device directory, sorted.
    entries: Vec<String>,
}

/// Scan `/sys/class/hwmon` for devices and their input files. On a
/// directory-listing failure the device is skipped entirely (TS
/// `catch { continue }`).
fn scan_hwmon() -> Vec<HwmonDevice> {
    let mut devices = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/hwmon") else {
        return devices;
    };
    let mut dirs: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    for dir in dirs {
        let path = format!("/sys/class/hwmon/{dir}");
        let name = read_trimmed(&format!("{path}/name"));
        let name = if name.is_empty() { dir.clone() } else { name };
        let Ok(entries_rd) = fs::read_dir(&path) else {
            continue;
        };
        let mut entries: Vec<String> = entries_rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        devices.push(HwmonDevice {
            dir,
            name,
            entries,
        });
    }
    devices
}

// ─── state ──────────────────────────────────────────────────────────

struct NetSample {
    rx: u64,
    tx: u64,
    time: Instant,
}

// ─── reader ─────────────────────────────────────────────────────────

pub struct SensorReader {
    /// Previous aggregate `/proc/stat` cpu jiffies (8 fields).
    prev_cpu_times: Option<[u64; 8]>,
    /// Previous per-interface rx/tx byte counters + sample time.
    prev_net_stats: HashMap<String, NetSample>,
    /// Previous aggregate read/written sector counts (512 B each).
    prev_disk_read: u64,
    prev_disk_write: u64,
    prev_disk_time: Option<Instant>,
    /// Cached system-updates count — `checkupdates` blocks ~460 ms, run at
    /// most once/min (60 s, as in TS).
    updates_cache: Option<(SensorReading, Instant)>,
    /// Cached `nvidia-smi` availability probe — never spawn it repeatedly on
    /// AMD boxes (probed once, as in TS).
    nvidia_available: Option<bool>,
    /// Cached `/sys/class/hwmon` device scan (which files exist), refreshed
    /// after 2 s. Sensor *values* are read fresh on every call.
    hwmon_scan: Option<(Instant, Vec<HwmonDevice>)>,
}

impl Default for SensorReader {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorReader {
    pub fn new() -> Self {
        SensorReader {
            prev_cpu_times: None,
            prev_net_stats: HashMap::new(),
            prev_disk_read: 0,
            prev_disk_write: 0,
            prev_disk_time: None,
            updates_cache: None,
            nvidia_available: None,
            hwmon_scan: None,
        }
    }

    // ── combined ────────────────────────────────────────────────────

    /// Read every sensor in the same order as TS `readAllSensors()`:
    /// time, updates, cpu usage, cpu temp, cpu clock, cpu power, gpu,
    /// memory, disk, network, motherboard, system.
    ///
    /// Returns readings **without** severity — the worker attaches
    /// severities afterwards.
    pub fn read_all_sensors(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();
        out.push(self.get_time());
        out.push(self.get_system_updates());
        out.push(self.get_cpu_usage());
        out.extend(self.get_cpu_temp());
        out.extend(self.get_cpu_clock());
        out.extend(self.get_cpu_power());
        out.extend(self.get_gpu_stats());
        out.extend(self.get_memory_stats());
        out.extend(self.get_disk_stats());
        out.extend(self.get_network_stats());
        out.extend(self.get_motherboard_sensors());
        out.extend(self.get_system_stats());
        out
    }

    // ── time ────────────────────────────────────────────────────────

    fn get_time(&mut self) -> SensorReading {
        // Local clock; the time is displayed from the unit field (`HH:MM`),
        // the numeric value is not meaningful (TS: value 0).
        let unit = chrono::Local::now().format("%H:%M").to_string();
        reading(
            "time",
            "Time",
            SensorCategory::System,
            0.0,
            &unit,
            None,
            None,
            "clock",
        )
    }

    // ── system updates ──────────────────────────────────────────────

    fn get_system_updates(&mut self) -> SensorReading {
        // checkupdates blocks ~460 ms per call — cache the result for 60 s.
        if let Some((cached, at)) = &self.updates_cache {
            if at.elapsed() < Duration::from_secs(60) {
                return cached.clone();
            }
        }

        // checkupdates works without root and doesn't require a synced db;
        // fall back to pacman, then apt (fallbacks only run when the previous
        // command failed or hit its timeout, exactly like the TS try/catch).
        let count = run_count("checkupdates 2>/dev/null | wc -l", 30)
            .or_else(|| run_count("pacman -Qu 2>/dev/null | wc -l", 10))
            .or_else(|| {
                run_count("apt list --upgradable 2>/dev/null | grep -c upgradable", 10)
            })
            .unwrap_or(0); // no supported package manager

        let r = reading(
            "updates_available",
            "Updates",
            SensorCategory::System,
            count as f64,
            "#",
            Some(0.0),
            None,
            "refresh",
        );
        self.updates_cache = Some((r.clone(), Instant::now()));
        r
    }

    // ── CPU ─────────────────────────────────────────────────────────

    fn get_cpu_usage(&mut self) -> SensorReading {
        let raw = read_file("/proc/stat");
        let first_line = raw.lines().next().unwrap_or("");
        let current = parse_cpu_stat(first_line);

        let mut r = reading(
            "cpu_usage",
            "CPU Usage",
            SensorCategory::Cpu,
            0.0,
            "%",
            Some(0.0),
            Some(100.0),
            "cpu",
        );

        if let Some(cur) = current {
            if let Some(prev) = self.prev_cpu_times {
                r.value = cpu_usage_delta(prev, cur);
            }
            self.prev_cpu_times = Some(cur);
        }
        r
    }

    /// CPU temperature from coretemp/k10temp hwmon `temp*_input` (m°C).
    fn get_cpu_temp(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();
        for dev in self.hwmon_scan() {
            let lname = dev.name.to_lowercase();
            if lname != "coretemp" && lname != "k10temp" {
                continue;
            }
            for entry in &dev.entries {
                // Entries like `temp3_input`; idx keeps its exact digits
                // (TS uses the `temp(\d+)_input` regex capture verbatim).
                let Some(rest) = entry.strip_prefix("temp") else {
                    continue;
                };
                let Some(idx) = rest.strip_suffix("_input") else {
                    continue;
                };
                if idx.is_empty() || !idx.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                let temp_c = match parse_int(&read_trimmed(&format!(
                    "/sys/class/hwmon/{}/{}",
                    dev.dir, entry
                ))) {
                    Some(v) => v,
                    None => continue,
                };
                let hw_label =
                    read_trimmed(&format!("/sys/class/hwmon/{}/temp{}_label", dev.dir, idx));
                let hw_label = if hw_label.is_empty() {
                    format!("Core {idx}")
                } else {
                    hw_label
                };
                out.push(reading(
                    &format!("cpu_temp_core{idx}"),
                    &format!("CPU {hw_label} ({lname})"),
                    SensorCategory::Cpu,
                    temp_c as f64 / 1000.0,
                    "°C",
                    Some(0.0),
                    Some(110.0),
                    "thermometer",
                ));
            }
        }
        out
    }

    /// Mean of all `cpufreq/scaling_cur_freq` (kHz → MHz).
    fn get_cpu_clock(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();
        let rd = match fs::read_dir("/sys/devices/system/cpu") {
            Ok(rd) => rd,
            Err(_) => return out, // cpufreq not available
        };
        let mut cpu_dirs: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                is_cpu_dir(&name).then_some(name)
            })
            .collect();
        cpu_dirs.sort();

        let mut total_khz: u64 = 0;
        let mut count: u64 = 0;
        for cpu_dir in cpu_dirs {
            let raw = read_trimmed(&format!(
                "/sys/devices/system/cpu/{cpu_dir}/cpufreq/scaling_cur_freq"
            ));
            if let Some(khz) = raw.parse::<u64>().ok() {
                total_khz += khz;
                count += 1;
            }
        }
        if count > 0 {
            out.push(reading(
                "cpu_clock",
                "CPU Clock",
                SensorCategory::Cpu,
                (total_khz as f64 / count as f64 / 1000.0).round(),
                "MHz",
                Some(0.0),
                Some(6000.0),
                "clock",
            ));
        }
        out
    }

    /// CPU power from Intel RAPL, else AMD k10temp `power1_average`.
    ///
    /// NOTE: the TS code reads `energy_uj` and divides by 1e6 *without* taking
    /// a delta (its comment claims "approx watts over 1s"). Ported bug-for-bug
    /// — no delta, hence no `max_energy_range_uj` rollover to handle either.
    fn get_cpu_power(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();

        let rapl_raw = read_trimmed("/sys/class/powercap/intel-rapl:0/energy_uj");
        if let Some(uj) = parse_int(&rapl_raw) {
            out.push(reading(
                "cpu_power",
                "CPU Power",
                SensorCategory::Cpu,
                round2(uj as f64 / 1_000_000.0), // µJ → J (approx watts over 1s)
                "W",
                Some(0.0),
                Some(300.0),
                "zap",
            ));
            return out;
        }

        // k10temp power (AMD) — only the first k10temp device, as in TS.
        for dev in self.hwmon_scan() {
            if dev.name.to_lowercase() != "k10temp" {
                continue;
            }
            let val = parse_int(&read_trimmed(&format!(
                "/sys/class/hwmon/{}/power1_average",
                dev.dir
            )));
            if let Some(val) = val {
                out.push(reading(
                    "cpu_power",
                    "CPU Power",
                    SensorCategory::Cpu,
                    round2(val as f64 / 1_000_000.0), // µW → W
                    "W",
                    Some(0.0),
                    Some(300.0),
                    "zap",
                ));
            }
            break;
        }

        out
    }

    // ── GPU ─────────────────────────────────────────────────────────

    /// Combined GPU — tries NVIDIA first, then AMD. Vendor detection is
    /// probed once (TS spawns `command -v nvidia-smi` exactly once too, to
    /// avoid thousands of "command not found" errors/hour on AMD machines).
    fn get_gpu_stats(&mut self) -> Vec<SensorReading> {
        if self.nvidia_available.is_none() {
            let probe = Command::new("sh")
                .arg("-c")
                .arg("timeout 2 command -v nvidia-smi")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            self.nvidia_available = Some(probe);
        }
        if self.nvidia_available == Some(true) {
            let nvidia = self.get_nvidia_gpu();
            if !nvidia.is_empty() {
                return nvidia;
            }
        }
        self.get_amd_gpu()
    }

    /// NVIDIA GPU via `nvidia-smi` (3 s timeout).
    fn get_nvidia_gpu(&self) -> Vec<SensorReading> {
        let mut out = Vec::new();
        let cmd = "timeout 3 nvidia-smi --query-gpu=temperature.gpu,utilization.gpu,\
                   memory.used,memory.total,power.draw,clocks.current.graphics \
                   --format=csv,noheader,nounits";
        let output = match Command::new("sh").arg("-c").arg(cmd).output() {
            Ok(o) if o.status.success() => o,
            _ => return out, // nvidia-smi not available
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            return out;
        }
        let multi = lines.len() > 1;

        for (idx, line) in lines.iter().enumerate() {
            let parts: Vec<Option<f64>> =
                line.split(',').map(|p| parse_float(p)).collect();
            if parts.len() < 6 || parts.iter().any(|p| p.is_none()) {
                continue; // TS: `parts.length < 6 || parts.some(isNaN)`
            }
            let mut vals = parts.into_iter().map(|p| p.unwrap_or(0.0));
            let temp = vals.next().unwrap();
            let util = vals.next().unwrap();
            let mem_used = vals.next().unwrap();
            let mem_total = vals.next().unwrap();
            let power = vals.next().unwrap();
            let clock = vals.next().unwrap();

            // One GPU → `gpu_` ids; several → `nvidia<N>_` prefix (as TS).
            let prefix = if multi {
                format!("nvidia{idx}_")
            } else {
                "gpu_".to_string()
            };

            out.push(reading(
                &format!("{prefix}temp"),
                &format!("GPU {idx} Temp (NVIDIA)"),
                SensorCategory::Gpu,
                temp,
                "°C",
                Some(0.0),
                Some(110.0),
                "thermometer",
            ));
            out.push(reading(
                &format!("{prefix}usage"),
                &format!("GPU {idx} Usage (NVIDIA)"),
                SensorCategory::Gpu,
                util,
                "%",
                Some(0.0),
                Some(100.0),
                "gpu",
            ));
            out.push(reading(
                &format!("{prefix}vram_used"),
                &format!("GPU {idx} VRAM (NVIDIA)"),
                SensorCategory::Gpu,
                round2(mem_used / 1024.0), // used MiB → GB
                "GB",
                Some(0.0),
                Some(round2(mem_total / 1024.0)),
                "memory",
            ));
            out.push(reading(
                &format!("{prefix}power"),
                &format!("GPU {idx} Power (NVIDIA)"),
                SensorCategory::Gpu,
                power,
                "W",
                Some(0.0),
                Some(400.0),
                "zap",
            ));
            out.push(reading(
                &format!("{prefix}clock"),
                &format!("GPU {idx} Clock (NVIDIA)"),
                SensorCategory::Gpu,
                clock,
                "MHz",
                Some(0.0),
                Some(3000.0),
                "clock",
            ));
        }
        out
    }

    /// AMD GPU via `/sys/class/hwmon/hwmon*` (amdgpu driver) — first device
    /// only, exactly like TS (`break` at the end of the loop body).
    fn get_amd_gpu(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();

        let dir = match self
            .hwmon_scan()
            .iter()
            .find(|d| d.name.to_lowercase() == "amdgpu")
            .map(|d| d.dir.clone())
        {
            Some(dir) => dir,
            None => return out, // amdgpu not available
        };
        let base = format!("/sys/class/hwmon/{dir}");

        // Temperature(s), temp1..temp5.
        for i in 1..=5 {
            let val = match parse_int(&read_trimmed(&format!("{base}/temp{i}_input"))) {
                Some(v) => v,
                None => continue,
            };
            let label = read_trimmed(&format!("{base}/temp{i}_label"));
            let label = if label.is_empty() {
                format!("Sensor {i}")
            } else {
                label
            };
            out.push(reading(
                &format!("amd_gpu_temp{i}"),
                &format!("GPU {label} (AMD)"),
                SensorCategory::Gpu,
                val as f64 / 1000.0,
                "°C",
                Some(0.0),
                Some(110.0),
                "thermometer",
            ));
        }

        // GPU clock (sclk), Hz → MHz.
        if let Some(sclk) = parse_int(&read_trimmed(&format!("{base}/freq1_input"))) {
            out.push(reading(
                "amd_gpu_clock",
                "GPU Clock (AMD)",
                SensorCategory::Gpu,
                (sclk as f64 / 1_000_000.0).round(),
                "MHz",
                Some(0.0),
                Some(3000.0),
                "clock",
            ));
        }

        // VRAM clock (mclk), Hz → MHz.
        if let Some(mclk) = parse_int(&read_trimmed(&format!("{base}/freq2_input"))) {
            out.push(reading(
                "amd_gpu_vram_clock",
                "VRAM Clock (AMD)",
                SensorCategory::Gpu,
                (mclk as f64 / 1_000_000.0).round(),
                "MHz",
                Some(0.0),
                Some(2000.0),
                "clock",
            ));
        }

        // Power (PPT), µW → W.
        if let Some(power) = parse_int(&read_trimmed(&format!("{base}/power1_average"))) {
            out.push(reading(
                "amd_gpu_power",
                "GPU Power (AMD)",
                SensorCategory::Gpu,
                round1(power as f64 / 1_000_000.0),
                "W",
                Some(0.0),
                Some(400.0),
                "zap",
            ));
        }

        // Fan speed.
        if let Some(fan) = parse_int(&read_trimmed(&format!("{base}/fan1_input"))) {
            out.push(reading(
                "amd_gpu_fan",
                "GPU Fan (AMD)",
                SensorCategory::Gpu,
                fan as f64,
                "RPM",
                Some(0.0),
                Some(5000.0),
                "fan",
            ));
        }

        // VRAM usage from /sys/class/drm/card*/device/mem_info_vram_* (first
        // card only, as TS breaks after the first entry).
        if let Ok(rd) = fs::read_dir("/sys/class/drm") {
            let mut cards: Vec<String> = rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.starts_with("card").then_some(name)
                })
                .collect();
            cards.sort();
            if let Some(card) = cards.first() {
                let used = parse_int(&read_trimmed(&format!(
                    "/sys/class/drm/{card}/device/mem_info_vram_used"
                )));
                let total = parse_int(&read_trimmed(&format!(
                    "/sys/class/drm/{card}/device/mem_info_vram_total"
                )));
                if let (Some(used), Some(total)) = (used, total) {
                    if total > 0 {
                        out.push(reading(
                            "amd_gpu_vram_used",
                            "GPU VRAM (AMD)",
                            SensorCategory::Gpu,
                            round2(used as f64 / 1_073_741_824.0), // bytes → GB
                            "GB",
                            Some(0.0),
                            Some(round2(total as f64 / 1_073_741_824.0)),
                            "memory",
                        ));
                    }
                }
            }
        }

        out
    }

    // ── memory ──────────────────────────────────────────────────────

    fn get_memory_stats(&mut self) -> Vec<SensorReading> {
        let raw = read_file("/proc/meminfo");
        if raw.is_empty() {
            return Vec::new();
        }
        compute_memory_stats(&raw)
    }

    // ── disk ────────────────────────────────────────────────────────

    fn get_disk_stats(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();
        let now = Instant::now();

        let raw = read_file("/proc/diskstats");
        if raw.is_empty() {
            return out;
        }

        let (total_read, total_write) = sum_diskstats(&raw);

        if let Some(prev_time) = self.prev_disk_time {
            let dt = now.duration_since(prev_time).as_secs_f64();
            if dt > 0.0 {
                let read_bps = (total_read as f64 - self.prev_disk_read as f64) * 512.0 / dt;
                let write_bps =
                    (total_write as f64 - self.prev_disk_write as f64) * 512.0 / dt;
                let (read_v, read_u) = format_speed(read_bps);
                let (write_v, write_u) = format_speed(write_bps);

                out.push(reading(
                    "disk_read_speed",
                    "Disk Read",
                    SensorCategory::Disk,
                    read_v,
                    read_u,
                    Some(0.0),
                    None,
                    "hard-drive",
                ));
                out.push(reading(
                    "disk_write_speed",
                    "Disk Write",
                    SensorCategory::Disk,
                    write_v,
                    write_u,
                    Some(0.0),
                    None,
                    "hard-drive",
                ));
            }
        }
        self.prev_disk_read = total_read;
        self.prev_disk_write = total_write;
        self.prev_disk_time = Some(now);

        // Root partition usage.
        if let Some(used_pct) = root_disk_usage_pct() {
            out.push(reading(
                "disk_usage_pct",
                "Disk Usage",
                SensorCategory::Disk,
                used_pct,
                "%",
                Some(0.0),
                Some(100.0),
                "hard-drive",
            ));
        }

        out
    }

    // ── network ─────────────────────────────────────────────────────

    fn get_network_stats(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();
        let now = Instant::now();
        let mut total_rx_speed = 0.0f64;
        let mut total_tx_speed = 0.0f64;

        let rd = match fs::read_dir("/sys/class/net") {
            Ok(rd) => rd,
            Err(_) => return out, // network stats unavailable
        };
        let interfaces: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "lo")
            .collect();

        for iface in &interfaces {
            let rx_raw = read_trimmed(&format!(
                "/sys/class/net/{iface}/statistics/rx_bytes"
            ));
            let tx_raw = read_trimmed(&format!(
                "/sys/class/net/{iface}/statistics/tx_bytes"
            ));
            let (Ok(rx), Ok(tx)) = (rx_raw.parse::<u64>(), tx_raw.parse::<u64>()) else {
                continue;
            };

            if let Some(prev) = self.prev_net_stats.get(iface) {
                let dt = now.duration_since(prev.time).as_secs_f64();
                if dt > 0.0 {
                    total_rx_speed += (rx as f64 - prev.rx as f64) / dt;
                    total_tx_speed += (tx as f64 - prev.tx as f64) / dt;
                }
            }
            self.prev_net_stats.insert(
                iface.clone(),
                NetSample {
                    rx,
                    tx,
                    time: now,
                },
            );
        }

        if !interfaces.is_empty() {
            let (rx_v, rx_u) = format_speed(total_rx_speed);
            let (tx_v, tx_u) = format_speed(total_tx_speed);
            let (tot_v, tot_u) = format_speed(total_rx_speed + total_tx_speed);

            out.push(reading(
                "net_rx_speed",
                "Net Download",
                SensorCategory::Network,
                rx_v,
                rx_u,
                Some(0.0),
                None,
                "download",
            ));
            out.push(reading(
                "net_tx_speed",
                "Net Upload",
                SensorCategory::Network,
                tx_v,
                tx_u,
                Some(0.0),
                None,
                "upload",
            ));
            out.push(reading(
                "net_total_speed",
                "Net Traffic",
                SensorCategory::Network,
                tot_v,
                tot_u,
                Some(0.0),
                None,
                "swap",
            ));
        }

        out
    }

    // ── motherboard sensors ─────────────────────────────────────────

    /// All hwmon temp/fan/in/power inputs *except* the CPU and GPU drivers
    /// (coretemp, k10temp, amdgpu, nvidia).
    fn get_motherboard_sensors(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();

        for dev in self.hwmon_scan() {
            let lname = dev.name.to_lowercase();
            if matches!(lname.as_str(), "coretemp" | "k10temp" | "amdgpu" | "nvidia") {
                continue;
            }
            let safe = sanitize_label(&dev.name);

            // TS pushes in device order: all temps, fans, voltages, powers.
            let mut temps = Vec::new();
            let mut fans = Vec::new();
            let mut voltages = Vec::new();
            let mut powers = Vec::new();
            for entry in &dev.entries {
                if let Some((kind, idx)) = parse_input_entry(entry) {
                    match kind {
                        "temp" => temps.push((entry, idx)),
                        "fan" => fans.push((entry, idx)),
                        "in" => voltages.push((entry, idx)),
                        "power" => powers.push((entry, idx)),
                        _ => {}
                    }
                }
            }

            for (entry, idx) in temps {
                let val = match parse_int(&read_trimmed(&format!(
                    "/sys/class/hwmon/{}/{}",
                    dev.dir, entry
                ))) {
                    Some(v) => v,
                    None => continue,
                };
                out.push(reading(
                    &format!("mb_temp_{safe}_{idx}"),
                    &format!("{} Temp {idx}", dev.name),
                    SensorCategory::Motherboard,
                    val as f64 / 1000.0, // m°C → °C
                    "°C",
                    Some(0.0),
                    Some(110.0),
                    "thermometer",
                ));
            }

            for (entry, idx) in fans {
                let val = match parse_int(&read_trimmed(&format!(
                    "/sys/class/hwmon/{}/{}",
                    dev.dir, entry
                ))) {
                    Some(v) => v,
                    None => continue,
                };
                out.push(reading(
                    &format!("mb_fan_{safe}_{idx}"),
                    &format!("{} Fan {idx}", dev.name),
                    SensorCategory::Motherboard,
                    val as f64,
                    "RPM",
                    Some(0.0),
                    Some(10000.0),
                    "fan",
                ));
            }

            for (entry, idx) in voltages {
                let val = match parse_int(&read_trimmed(&format!(
                    "/sys/class/hwmon/{}/{}",
                    dev.dir, entry
                ))) {
                    Some(v) => v,
                    None => continue,
                };
                out.push(reading(
                    &format!("mb_voltage_{safe}_{idx}"),
                    &format!("{} V {idx}", dev.name),
                    SensorCategory::Motherboard,
                    val as f64 / 1000.0, // mV → V
                    "V",
                    Some(0.0),
                    Some(15.0),
                    "zap",
                ));
            }

            for (entry, idx) in powers {
                let val = match parse_int(&read_trimmed(&format!(
                    "/sys/class/hwmon/{}/{}",
                    dev.dir, entry
                ))) {
                    Some(v) => v,
                    None => continue,
                };
                out.push(reading(
                    &format!("mb_power_{safe}_{idx}"),
                    &format!("{} Power {idx}", dev.name),
                    SensorCategory::Motherboard,
                    val as f64 / 1000.0, // mW → W
                    "W",
                    Some(0.0),
                    Some(500.0),
                    "zap",
                ));
            }
        }

        out
    }

    // ── system ──────────────────────────────────────────────────────

    fn get_system_stats(&mut self) -> Vec<SensorReading> {
        let mut out = Vec::new();

        // Uptime.
        let uptime_raw = read_trimmed("/proc/uptime");
        if let Some(first) = uptime_raw.split_whitespace().next() {
            if let Some(uptime_seconds) = parse_float(first) {
                out.push(reading(
                    "uptime_hours",
                    "Uptime",
                    SensorCategory::System,
                    round2(uptime_seconds / 3600.0),
                    "h",
                    Some(0.0),
                    None,
                    "clock",
                ));
            }
        }

        // Load average.
        let load_raw = read_trimmed("/proc/loadavg");
        if let Some(first) = load_raw.split_whitespace().next() {
            if let Some(load1) = parse_float(first) {
                out.push(reading(
                    "load_avg_1m",
                    "Load Avg",
                    SensorCategory::System,
                    round2(load1),
                    "",
                    Some(0.0),
                    None,
                    "activity",
                ));
            }
        }

        // Process count (numeric entries in /proc).
        if let Ok(rd) = fs::read_dir("/proc") {
            let process_count = rd
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                        .unwrap_or(false)
                })
                .count();
            out.push(reading(
                "process_count",
                "Processes",
                SensorCategory::System,
                process_count as f64,
                "",
                Some(0.0),
                None,
                "list",
            ));
        }

        out
    }

    // ── hwmon scan cache ────────────────────────────────────────────

    fn hwmon_scan(&mut self) -> &[HwmonDevice] {
        const TTL: Duration = Duration::from_secs(2);
        let fresh = matches!(&self.hwmon_scan, Some((at, _)) if at.elapsed() < TTL);
        if !fresh {
            self.hwmon_scan = Some((Instant::now(), scan_hwmon()));
        }
        match &self.hwmon_scan {
            Some((_, devices)) => devices,
            None => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_speed (auto-scaling units) ───────────────────────────

    #[test]
    fn format_speed_picks_unit_and_rounds() {
        assert_eq!(format_speed(0.0), (0.0, "B/s"));
        assert_eq!(format_speed(512.44), (512.4, "B/s"));
        assert_eq!(format_speed(1023.9), (1023.9, "B/s"));
        assert_eq!(format_speed(1536.0), (1.5, "KB/s"));
        assert_eq!(format_speed(1024.0 * 1024.0), (1.0, "MB/s"));
        assert_eq!(format_speed(5.24 * 1024.0 * 1024.0), (5.2, "MB/s"));
        assert_eq!(format_speed(1024.0f64.powi(3)), (1.0, "GB/s"));
        assert_eq!(format_speed(1.5 * 1024.0f64.powi(3)), (1.5, "GB/s"));
        // Scaling is chosen on the magnitude (abs), so negatives keep the
        // same unit as TS does.
        assert_eq!(format_speed(-2048.0).1, "KB/s");
    }

    // ── diskstats predicate ─────────────────────────────────────────

    #[test]
    fn diskstats_predicate_matches_ts() {
        // Whole disks (no trailing digit) are counted.
        assert!(counts_disk("sda"));
        assert!(counts_disk("vda"));
        assert!(counts_disk("nvme0"));
        // Partitions end in a digit → skipped, except nvme (TS exempts
        // only `nvme*`; `loop*`/`ram*` are skipped by the second rule).
        assert!(!counts_disk("sda1"));
        assert!(counts_disk("nvme0n1p1"));
        assert!(!counts_disk("mmcblk0p1"));
        assert!(!counts_disk("loop0"));
        assert!(!counts_disk("loop"));
        assert!(!counts_disk("ram0"));
        assert!(!counts_disk("ram"));
        assert!(!counts_disk("8"));
    }

    #[test]
    fn sum_diskstats_fixture() {
        // 14+ fields per line: major minor name + stats; parts[5] = sectors
        // read, parts[9] = sectors written.
        let raw = "\
  8       0 sda 100 0 2000 0 50 0 3000 0 0 0 0
259       0 nvme0n1p1 10 0 20 0 30 0 40 0 0 0 0
253       0 loop0 5 0 9999 0 1 0 9999 0 0 0 0
253       1 vda 7 0 14 0 21 0 28 0 0 0 0
254       0 ram0 1 0 7777 0 1 0 7777 0 0 0 0
  8       1 sda1 1 0 8888 0 1 0 8888 0 0 0 0
  7       0 short_line 1 0 2
";
        let (read, write) = sum_diskstats(raw);
        // sda (2000/3000) + nvme0n1p1 (20/40) + vda (14/28); loop0, ram0,
        // sda1 and the short line are excluded.
        assert_eq!(read, 2000 + 20 + 14);
        assert_eq!(write, 3000 + 40 + 28);
    }

    // ── /proc/stat delta math ───────────────────────────────────────

    #[test]
    fn cpu_stat_parse_and_delta() {
        let prev = parse_cpu_stat("cpu  100 0 50 800 50 0 0 0 0 0").expect("parse");
        assert_eq!(prev, [100, 0, 50, 800, 50, 0, 0, 0]);

        // Same shape as TS: ≥ 9 fields, all numeric.
        assert!(parse_cpu_stat("cpu 1 2 3 4 5 6 7 8").is_some());
        assert!(parse_cpu_stat("cpu 1 2 3").is_none());
        assert!(parse_cpu_stat("cpu 1 2 x 4 5 6 7 8 9").is_none());
        assert!(parse_cpu_stat("").is_none());

        let cur = parse_cpu_stat("cpu  150 0 60 850 60 0 0 0 0 0").expect("parse");
        // total delta 120, idle+iowait delta 60 → 1 - 60/120 = 50%.
        assert_eq!(cpu_usage_delta(prev, cur), 50.0);

        // First sample / no progress → 0 (two decimals, like TS).
        assert_eq!(cpu_usage_delta(prev, prev), 0.0);
        let tiny = [0, 0, 0, 0, 0, 0, 0, 0];
        let idle_only = [0, 0, 0, 1, 0, 0, 0, 0];
        // total delta 1, idle delta 1 → 0%.
        assert_eq!(cpu_usage_delta(tiny, idle_only), 0.0);
        // total delta 2, idle delta 1 → 50%.
        assert_eq!(cpu_usage_delta(tiny, [1, 0, 0, 1, 0, 0, 0, 0]), 50.0);
        // total delta 1, idle delta 0 → 100%.
        let busy = [1, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(cpu_usage_delta(tiny, busy), 100.0);
        // Non-integer percentages round to 2 decimals.
        let p = [0, 0, 0, 0, 0, 0, 0, 0];
        let c = [1, 0, 0, 2, 1, 0, 0, 0]; // total 4, idle 3 → 25%
        assert_eq!(cpu_usage_delta(p, c), 25.0);
        let c2 = [3, 0, 0, 9, 0, 0, 0, 0]; // total 12, idle 9 → 25%
        assert_eq!(cpu_usage_delta([0, 0, 0, 0, 0, 0, 0, 0], c2), 25.0);
    }

    // ── meminfo fixture ─────────────────────────────────────────────

    const MEMINFO_FIXTURE: &str = "\
MemTotal:       16384000 kB
MemFree:         4096000 kB
MemAvailable:    8192000 kB
Buffers:          524288 kB
SwapTotal:       2097152 kB
SwapFree:        1048576 kB
";

    #[test]
    fn meminfo_parse_from_fixture() {
        assert_eq!(meminfo_field(MEMINFO_FIXTURE, "MemTotal"), Some(16_384_000));
        assert_eq!(meminfo_field(MEMINFO_FIXTURE, "MemAvailable"), Some(8_192_000));
        assert_eq!(meminfo_field(MEMINFO_FIXTURE, "SwapTotal"), Some(2_097_152));
        assert_eq!(meminfo_field(MEMINFO_FIXTURE, "SwapFree"), Some(1_048_576));
        assert_eq!(meminfo_field(MEMINFO_FIXTURE, "HugePages_Total"), None);
        assert_eq!(meminfo_field("", "MemTotal"), None);
    }

    #[test]
    fn memory_stats_from_fixture() {
        let out = compute_memory_stats(MEMINFO_FIXTURE);
        assert_eq!(out.len(), 4);

        // used = 16384000 - 8192000 = 8192000 kB → 50%.
        assert_eq!(out[0].id, "mem_used_pct");
        assert_eq!(out[0].value, 50.0);
        assert_eq!(out[0].min, Some(0.0));
        assert_eq!(out[0].max, Some(100.0));
        assert_eq!(out[0].unit, "%");
        assert!(out[0].severity.is_none());

        // 8192000 / 1048576 = 7.8125 → 7.81 GB; total 16384000 / 1048576
        // = 15.625 → 15.63 GB.
        assert_eq!(out[1].id, "mem_used_gb");
        assert_eq!(out[1].value, 7.81);
        assert_eq!(out[1].max, Some(15.63));
        assert_eq!(out[2].id, "mem_free_gb");
        assert_eq!(out[2].value, 7.81);
        assert_eq!(out[2].max, Some(15.63));

        // swap: (2097152 - 1048576) / 2097152 → 50%.
        assert_eq!(out[3].id, "swap_used_pct");
        assert_eq!(out[3].value, 50.0);
        assert_eq!(out[3].category, SensorCategory::Memory);
        assert_eq!(out[3].icon, "swap");
    }

    #[test]
    fn memory_stats_missing_or_zero_fields() {
        // No MemAvailable → no RAM readings; SwapTotal 0 → no swap reading.
        let raw = "MemTotal:       16384000 kB\nSwapTotal:       0 kB\nSwapFree:       0 kB\n";
        assert!(compute_memory_stats(raw).is_empty());

        // Empty dump → nothing (TS `if (!raw) return readings`).
        assert!(compute_memory_stats("").is_empty());
    }

    // ── misc pure helpers ───────────────────────────────────────────

    #[test]
    fn sanitize_label_lowercases_and_underscores() {
        assert_eq!(sanitize_label("nct6798"), "nct6798");
        assert_eq!(sanitize_label("acpi_fan"), "acpi_fan");
        assert_eq!(sanitize_label("IT8688-E"), "it8688_e");
        assert_eq!(sanitize_label("wmi1"), "wmi1");
        assert_eq!(sanitize_label("Sensor 1"), "sensor_1");
    }

    #[test]
    fn hwmon_entry_classification() {
        assert_eq!(parse_input_entry("temp1_input"), Some(("temp", 1)));
        assert_eq!(parse_input_entry("temp10_input"), Some(("temp", 10)));
        assert_eq!(parse_input_entry("fan2_input"), Some(("fan", 2)));
        assert_eq!(parse_input_entry("in3_input"), Some(("in", 3)));
        assert_eq!(parse_input_entry("power1_average"), None);
        assert_eq!(parse_input_entry("temp1_label"), None);
        assert_eq!(parse_input_entry("name"), None);
    }

    #[test]
    fn cpu_dir_filter() {
        assert!(is_cpu_dir("cpu0"));
        assert!(is_cpu_dir("cpu112"));
        assert!(!is_cpu_dir("cpu"));
        assert!(!is_cpu_dir("cpufreq"));
        assert!(!is_cpu_dir("cpuidle"));
        assert!(!is_cpu_dir("cpu0x"));
    }

    #[test]
    fn parse_int_matches_js_parse_int_shape() {
        assert_eq!(parse_int("45"), Some(45));
        assert_eq!(parse_int("45.0"), Some(45));
        assert_eq!(parse_int("  45"), Some(45));
        assert_eq!(parse_int("-7"), Some(-7));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int("N/A"), None);
    }
}
