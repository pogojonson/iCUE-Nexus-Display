//! Static sensor picker definitions — verbatim port of `SENSOR_DEFINITIONS`
//! from `k100display/src/constants.ts`.
//!
//! Note: the picker icons here are the emoji set from `constants.ts`, while
//! the live `SensorReading`s produced by `sensors.rs` carry the lucide-style
//! icon names from `electron/nexus/sensors.ts` (both exactly as in the TS
//! originals — the renderer uses whichever applies).

use crate::core::types::{SensorCategory, SensorDefinition};

/// All sensors selectable in the UI (ids/labels/units/icons exactly as TS).
pub const SENSOR_DEFINITIONS: &[SensorDefinition] = &[
    // System (time, updates)
    SensorDefinition {
        id: "time",
        label: "Time",
        category: SensorCategory::System,
        unit: "",
        icon: "🕐",
    },
    SensorDefinition {
        id: "updates_available",
        label: "Updates",
        category: SensorCategory::System,
        unit: "pkgs",
        icon: "📦",
    },
    // CPU
    SensorDefinition {
        id: "cpu_usage",
        label: "CPU Usage",
        category: SensorCategory::Cpu,
        unit: "%",
        icon: "🔧",
    },
    SensorDefinition {
        id: "cpu_temp_core1",
        label: "CPU Temp",
        category: SensorCategory::Cpu,
        unit: "°C",
        icon: "🌡️",
    },
    SensorDefinition {
        id: "cpu_clock",
        label: "CPU Clock",
        category: SensorCategory::Cpu,
        unit: "MHz",
        icon: "⏱️",
    },
    SensorDefinition {
        id: "cpu_power",
        label: "CPU Power",
        category: SensorCategory::Cpu,
        unit: "W",
        icon: "⚡",
    },
    // GPU (NVIDIA + AMD)
    SensorDefinition {
        id: "gpu_temp",
        label: "GPU Temp (Nvidia)",
        category: SensorCategory::Gpu,
        unit: "°C",
        icon: "🌡️",
    },
    SensorDefinition {
        id: "gpu_usage",
        label: "GPU Usage (Nvidia)",
        category: SensorCategory::Gpu,
        unit: "%",
        icon: "🎮",
    },
    SensorDefinition {
        id: "gpu_vram_used",
        label: "VRAM Used (Nvidia)",
        category: SensorCategory::Gpu,
        unit: "GB",
        icon: "💾",
    },
    SensorDefinition {
        id: "gpu_power",
        label: "GPU Power (Nvidia)",
        category: SensorCategory::Gpu,
        unit: "W",
        icon: "⚡",
    },
    SensorDefinition {
        id: "gpu_clock",
        label: "GPU Clock (Nvidia)",
        category: SensorCategory::Gpu,
        unit: "MHz",
        icon: "⏱️",
    },
    // AMD-specific GPU sensors
    SensorDefinition {
        id: "amd_gpu_temp1",
        label: "GPU Edge (AMD)",
        category: SensorCategory::Gpu,
        unit: "°C",
        icon: "🌡️",
    },
    SensorDefinition {
        id: "amd_gpu_clock",
        label: "GPU Clock (AMD)",
        category: SensorCategory::Gpu,
        unit: "MHz",
        icon: "⏱️",
    },
    SensorDefinition {
        id: "amd_gpu_vram_clock",
        label: "VRAM Clock (AMD)",
        category: SensorCategory::Gpu,
        unit: "MHz",
        icon: "⏱️",
    },
    SensorDefinition {
        id: "amd_gpu_power",
        label: "GPU Power (AMD)",
        category: SensorCategory::Gpu,
        unit: "W",
        icon: "⚡",
    },
    SensorDefinition {
        id: "amd_gpu_fan",
        label: "GPU Fan (AMD)",
        category: SensorCategory::Gpu,
        unit: "RPM",
        icon: "🌀",
    },
    SensorDefinition {
        id: "amd_gpu_vram_used",
        label: "GPU VRAM (AMD)",
        category: SensorCategory::Gpu,
        unit: "GB",
        icon: "💾",
    },
    // Memory
    SensorDefinition {
        id: "mem_used_pct",
        label: "RAM Usage",
        category: SensorCategory::Memory,
        unit: "%",
        icon: "📊",
    },
    SensorDefinition {
        id: "mem_used_gb",
        label: "RAM Used",
        category: SensorCategory::Memory,
        unit: "GB",
        icon: "📊",
    },
    SensorDefinition {
        id: "mem_free_gb",
        label: "RAM Free",
        category: SensorCategory::Memory,
        unit: "GB",
        icon: "📊",
    },
    SensorDefinition {
        id: "swap_used_pct",
        label: "Swap Usage",
        category: SensorCategory::Memory,
        unit: "%",
        icon: "📊",
    },
    // Disk
    SensorDefinition {
        id: "disk_read_speed",
        label: "Disk Read",
        category: SensorCategory::Disk,
        unit: "speed",
        icon: "💿",
    },
    SensorDefinition {
        id: "disk_write_speed",
        label: "Disk Write",
        category: SensorCategory::Disk,
        unit: "speed",
        icon: "💿",
    },
    SensorDefinition {
        id: "disk_usage_pct",
        label: "Disk Usage",
        category: SensorCategory::Disk,
        unit: "%",
        icon: "💿",
    },
    // Network
    SensorDefinition {
        id: "net_rx_speed",
        label: "Net Download",
        category: SensorCategory::Network,
        unit: "speed",
        icon: "🌐",
    },
    SensorDefinition {
        id: "net_tx_speed",
        label: "Net Upload",
        category: SensorCategory::Network,
        unit: "speed",
        icon: "🌐",
    },
    SensorDefinition {
        id: "net_total_speed",
        label: "Net Traffic",
        category: SensorCategory::Network,
        unit: "speed",
        icon: "🌐",
    },
    // System
    SensorDefinition {
        id: "uptime_hours",
        label: "Uptime",
        category: SensorCategory::System,
        unit: "h",
        icon: "⏰",
    },
    SensorDefinition {
        id: "load_avg_1m",
        label: "Load Avg",
        category: SensorCategory::System,
        unit: "",
        icon: "📈",
    },
    SensorDefinition {
        id: "process_count",
        label: "Processes",
        category: SensorCategory::System,
        unit: "",
        icon: "📋",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ts_definitions_present_and_unique() {
        // TS `SENSOR_DEFINITIONS` has 30 entries (the "27" in some docs is off).
        assert_eq!(SENSOR_DEFINITIONS.len(), 30);
        let mut seen = std::collections::HashSet::new();
        for def in SENSOR_DEFINITIONS {
            assert!(seen.insert(def.id), "duplicate sensor id: {}", def.id);
            assert!(!def.label.is_empty());
            assert!(!def.icon.is_empty());
        }
    }

    #[test]
    fn spot_check_ts_entries() {
        let find = |id: &str| SENSOR_DEFINITIONS.iter().find(|d| d.id == id).copied();
        let time = find("time").expect("time definition");
        assert_eq!(time.label, "Time");
        assert_eq!(time.category, SensorCategory::System);
        assert_eq!(time.unit, "");
        assert_eq!(time.icon, "🕐");
        let upd = find("updates_available").expect("updates definition");
        assert_eq!(upd.unit, "pkgs");
        let amd = find("amd_gpu_temp1").expect("amd temp definition");
        assert_eq!(amd.label, "GPU Edge (AMD)");
        assert_eq!(amd.unit, "°C");
        let disk = find("disk_read_speed").expect("disk read definition");
        assert_eq!(disk.unit, "speed");
    }
}
