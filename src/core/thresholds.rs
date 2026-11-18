//! Threshold evaluation — pure, no I/O.
//!
//! Evaluation order is critical → serious → warning, first enabled level whose
//! predicate matches wins. That means the highest severity always wins even if
//! the configured values are out of order — no validation needed.

use crate::core::types::*;

pub const DEFAULT_ALERT_STYLE: AlertStyle = AlertStyle {
    warning_color: RgbColor::new(255, 212, 0), // yellow
    serious_color: RgbColor::new(255, 59, 48), // red
    critical_color: RgbColor::new(255, 32, 32), // red (animated)
    critical_animation: CriticalAnimation::ValueOnly,
};

fn level_matches(level: &ThresholdLevel, value: f64, direction: ThresholdDirection) -> bool {
    if !level.enabled {
        return false;
    }
    match direction {
        ThresholdDirection::GreaterEq => value >= level.value,
        ThresholdDirection::LessEq => value <= level.value,
    }
}

/// Evaluate a single reading against its (optional) threshold config.
pub fn evaluate_severity(value: f64, cfg: Option<&SensorThresholds>) -> Severity {
    let Some(cfg) = cfg else { return Severity::None };
    if value.is_nan() {
        return Severity::None;
    }
    if level_matches(&cfg.critical, value, cfg.direction) {
        return Severity::Critical;
    }
    if level_matches(&cfg.serious, value, cfg.direction) {
        return Severity::Serious;
    }
    if level_matches(&cfg.warning, value, cfg.direction) {
        return Severity::Warning;
    }
    Severity::None
}

/// Attach severity to every reading. The "time" sensor is exempt (its unit
/// carries HH:MM — a threshold on it makes no sense).
pub fn apply_severities(readings: &mut [SensorReading], thresholds: &ThresholdMap) {
    for r in readings.iter_mut() {
        r.severity = if r.id == "time" {
            Some(Severity::None)
        } else {
            Some(evaluate_severity(r.value, thresholds.get(&r.id)))
        };
    }
}

/// Animation phase period for the configured critical animation.
pub fn animation_period_ms(style: &AlertStyle) -> u64 {
    match style.critical_animation {
        CriticalAnimation::Rainbow => 250,
        CriticalAnimation::Crazy | CriticalAnimation::Nyan => 125,
        _ => 500, // value-only / tile / both
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn th(direction: ThresholdDirection, w: f64, s: f64, c: f64) -> SensorThresholds {
        SensorThresholds {
            direction,
            warning: ThresholdLevel { enabled: true, value: w },
            serious: ThresholdLevel { enabled: true, value: s },
            critical: ThresholdLevel { enabled: true, value: c },
        }
    }

    #[test]
    fn critical_wins_even_out_of_order() {
        // critical lower than warning — still critical first
        let cfg = th(ThresholdDirection::GreaterEq, 90.0, 80.0, 70.0);
        assert_eq!(evaluate_severity(85.0, Some(&cfg)), Severity::Critical);
        assert_eq!(evaluate_severity(95.0, Some(&cfg)), Severity::Critical);
    }

    #[test]
    fn direction_matters() {
        let cfg = th(ThresholdDirection::LessEq, 20.0, 10.0, 5.0);
        assert_eq!(evaluate_severity(15.0, Some(&cfg)), Severity::Warning);
        assert_eq!(evaluate_severity(4.0, Some(&cfg)), Severity::Critical);
        assert_eq!(evaluate_severity(50.0, Some(&cfg)), Severity::None);
    }

    #[test]
    fn disabled_levels_never_match() {
        let mut cfg = th(ThresholdDirection::GreaterEq, 50.0, 60.0, 70.0);
        cfg.warning.enabled = false;
        cfg.serious.enabled = false;
        cfg.critical.enabled = false;
        assert_eq!(evaluate_severity(99.0, Some(&cfg)), Severity::None);
        assert_eq!(evaluate_severity(99.0, None), Severity::None);
    }

    #[test]
    fn time_sensor_exempt() {
        let mut thresholds = HashMap::new();
        thresholds.insert("time".to_string(), th(ThresholdDirection::GreaterEq, 0.0, 0.0, 0.0));
        let mut readings = vec![SensorReading {
            id: "time".into(),
            label: "Time".into(),
            category: SensorCategory::System,
            value: 0.0,
            unit: "14:30".into(),
            min: None,
            max: None,
            icon: "🕐".into(),
            severity: None,
        }];
        apply_severities(&mut readings, &thresholds);
        assert_eq!(readings[0].severity, Some(Severity::None));
    }

    #[test]
    fn animation_periods() {
        let mut s = DEFAULT_ALERT_STYLE;
        s.critical_animation = CriticalAnimation::Rainbow;
        assert_eq!(animation_period_ms(&s), 250);
        s.critical_animation = CriticalAnimation::Nyan;
        assert_eq!(animation_period_ms(&s), 125);
        s.critical_animation = CriticalAnimation::ValueOnly;
        assert_eq!(animation_period_ms(&s), 500);
    }
}
