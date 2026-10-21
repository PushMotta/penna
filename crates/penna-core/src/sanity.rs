//! What counts as a temperature (DESIGN.md §3.3).
use crate::Sample;
use std::ops::RangeInclusive;

/// A reading outside this range is a failed read, not a temperature.
pub const READING_C: RangeInclusive<f64> = -40.0..=150.0;
/// A limit outside this range is ignored.
pub const LIMIT_C: RangeInclusive<f64> = 40.0..=130.0;
/// Sensors that read below this at idle are listed but never chosen as a default source (`acpitz` reads 16.8 °C here).
pub const AMBIENT_C: f64 = 20.0;

pub fn temperature(celsius: f64) -> Sample<f64> {
    if READING_C.contains(&celsius) { Sample::Value(celsius) } else { Sample::Implausible(celsius) }
}

pub fn limit(celsius: f64) -> Option<f64> {
    LIMIT_C.contains(&celsius).then_some(celsius)
}

pub fn below_ambient(celsius: f64) -> bool {
    celsius < AMBIENT_C
}

/// hwmon reports temperatures in millidegrees Celsius.
pub fn millidegrees(raw: i64) -> f64 {
    raw as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_at_and_past_the_edges() {
        assert_eq!(temperature(-40.0), Sample::Value(-40.0));
        assert_eq!(temperature(150.0), Sample::Value(150.0));
        assert_eq!(temperature(78.5), Sample::Value(78.5));
        assert_eq!(temperature(150.001), Sample::Implausible(150.001));
        assert_eq!(temperature(-40.5), Sample::Implausible(-40.5));
        assert!(matches!(temperature(f64::NAN), Sample::Implausible(_)));
    }

    #[test]
    fn limits_from_this_desk() {
        // NVMe Composite: max 81.85, crit 84.85; Sensor 1 and 2: max 65261.85, a firmware placeholder
        assert_eq!(limit(millidegrees(81_850)), Some(81.85));
        assert_eq!(limit(millidegrees(84_850)), Some(84.85));
        assert_eq!(limit(millidegrees(65_261_850)), None);
        assert_eq!(limit(39.9), None);
        assert_eq!(limit(130.0), Some(130.0));
    }

    #[test]
    fn acpitz_is_below_ambient() {
        assert!(below_ambient(millidegrees(16_800)));
        assert!(!below_ambient(20.0));
        assert!(!below_ambient(37.85));
    }
}
