//! Sensors, controls and tachs (DESIGN.md §3.1): separate things, related only by explicit pairing.
use crate::Key;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Hwmon,
    Nvml,
}

/// What discovery found, plus what it could see but not reach.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inventory {
    pub devices: Vec<Device>,
    pub unreachable: Vec<Unreachable>,
    /// Plain-language findings about the machine as a whole, e.g. that no Super I/O driver is bound.
    pub notes: Vec<String>,
}

impl Inventory {
    pub fn sensor_count(&self) -> usize {
        self.devices.iter().map(|d| d.sensors.len()).sum()
    }
    pub fn tach_count(&self) -> usize {
        self.devices.iter().map(|d| d.tachs.len()).sum()
    }
    pub fn control_count(&self) -> usize {
        self.devices.iter().map(|d| d.controls.len()).sum()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// The key prefix its channels share, e.g. `hwmon/k10temp/0000:00:18.3` or `nvml/GPU-…`.
    pub id: String,
    pub source: Source,
    /// hwmon's `name` (`k10temp`, `nvme`, `it87`), or `nvml`.
    pub driver: String,
    /// What a person calls it: "AMD CPU", "NVMe", "NVIDIA GeForce RTX 3090".
    pub label: String,
    /// Where it sits: a PCI address, a platform device, a thermal zone.
    pub location: String,
    /// The device's own name for itself when it has one (an NVMe drive's `model`).
    pub model: Option<String>,
    pub sensors: Vec<SensorInfo>,
    pub tachs: Vec<TachInfo>,
    pub controls: Vec<ControlInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SensorInfo {
    pub key: Key,
    pub label: String,
    /// Limits that passed [`crate::sanity::limit`].
    pub limits: Vec<Limit>,
    /// Limits the hardware reported that are not temperatures (NVMe's 65261.85 °C placeholder).
    pub ignored_limits: Vec<Limit>,
    /// Read below room temperature at discovery: listed, but never chosen as a default source (§3.3).
    pub below_ambient: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    pub kind: LimitKind,
    pub celsius: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// hwmon `tempN_max`
    Max,
    /// hwmon `tempN_crit`
    Crit,
    /// NVML: the maximum operating temperature
    GpuMax,
    /// NVML: where the GPU starts to throttle
    Slowdown,
    /// NVML: where the GPU switches itself off
    Shutdown,
    /// NVML: the temperature the card's own fan curve holds it under, its acoustic threshold: 83 °C on the 3090. Generated GPU
    /// curves aim there (§5.5).
    Acoustic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TachInfo {
    pub key: Key,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlInfo {
    pub key: Key,
    pub label: String,
    pub kind: ControlKind,
}

/// What a control accepts (DESIGN.md §3.5). Penna drives `Duty` and `Nvml` from M5; the others are listed for what they are and
/// driven from M9.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum ControlKind {
    /// hwmon `pwmN` in manual mode: any duty. `scale` is what 100 % is written as: 255 almost everywhere, 100 on nouveau.
    Duty { scale: u32 },
    /// An NVML fan, settable between `min` and `max` NVML percent (30–100 on the 3090, DESIGN.md §0)
    Nvml { min: u32, max: u32 },
    /// A few fixed steps: a ThinkPad's 8 levels, Dell's off, low and high.
    Levels { steps: u32 },
    /// A target RPM between the driver's bounds.
    Speed { min: u32, max: u32 },
    /// Points the firmware runs by itself.
    FirmwareCurve { points: u32 },
    /// Automatic, full speed, or a vendor's named modes.
    Modes,
    /// Nothing Penna can write: the driver offers no control, or not one Penna will use.
    ReadOnly,
}

impl ControlKind {
    /// Whether this Penna drives it.
    pub fn drivable(self) -> bool {
        matches!(self, ControlKind::Duty { .. } | ControlKind::Nvml { .. })
    }

    pub fn name(self) -> &'static str {
        match self {
            ControlKind::Duty { .. } => "duty",
            ControlKind::Nvml { .. } => "nvml",
            ControlKind::Levels { .. } => "levels",
            ControlKind::Speed { .. } => "speed",
            ControlKind::FirmwareCurve { .. } => "firmware-curve",
            ControlKind::Modes => "modes",
            ControlKind::ReadOnly => "read-only",
        }
    }

    /// Why Penna doesn't drive it, in words, or `None` when it does.
    pub fn undrivable_reason(self) -> Option<String> {
        let later = "Penna drives these from M9 (DESIGN.md §3.5)";
        match self {
            ControlKind::Duty { .. } | ControlKind::Nvml { .. } => None,
            ControlKind::Levels { steps } => Some(format!("This fan takes {steps} fixed steps; {later}.")),
            ControlKind::Speed { .. } => Some(format!("This fan takes a target speed in RPM; {later}.")),
            ControlKind::FirmwareCurve { points } => Some(format!("Its firmware runs a {points}-point curve itself; {later}.")),
            ControlKind::Modes => Some(format!("This fan only switches between its firmware's modes; {later}.")),
            ControlKind::ReadOnly => Some("Its driver offers no control Penna can use.".to_owned()),
        }
    }
}

/// A device that exists but that no backend can reach, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unreachable {
    pub id: String,
    pub label: String,
    pub reason: String,
}

/// One value as it came off the hardware.
#[derive(Clone, Debug, PartialEq)]
pub enum Sample<T> {
    Value(T),
    /// A number that cannot be what it claims to be (a temperature of 4000 °C).
    Implausible(f64),
    /// The read itself failed.
    Error(String),
}

impl<T: Copy> Sample<T> {
    pub fn value(&self) -> Option<T> {
        match self {
            Sample::Value(v) => Some(*v),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlReading {
    /// PWM duty for hwmon, NVML percent for NVML (which is not duty, DESIGN.md §0).
    pub percent: f64,
    /// hwmon's raw `pwmN`, 0–255.
    pub raw: Option<u32>,
    pub mode: ControlMode,
}

/// Who is driving a control right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlMode {
    /// hwmon `pwmN_enable` = 0: no control, full speed. it87 and nct6775 also read 0 for a manual channel at 100 % (§3.5).
    Full,
    /// hwmon `pwmN_enable` = 1, or NVML's manual policy
    Manual,
    /// hwmon `pwmN_enable` ≥ 2: the chip's own automatic mode; the exact value is chip-specific
    Automatic(u32),
    /// NVML's temperature-controlled policy: the driver's curve
    Driver,
    /// No `pwmN_enable`, or a value we do not know
    Unknown,
}

impl ControlMode {
    pub fn from_pwm_enable(v: i64) -> ControlMode {
        match v {
            0 => ControlMode::Full,
            1 => ControlMode::Manual,
            2.. => u32::try_from(v).map_or(ControlMode::Unknown, ControlMode::Automatic),
            _ => ControlMode::Unknown,
        }
    }
}

/// One pass of values over an [`Inventory`]. NVML fans appear in both `rpms` and `controls` under one key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readings {
    pub temps: BTreeMap<Key, Sample<f64>>,
    pub rpms: BTreeMap<Key, Sample<u32>>,
    pub controls: BTreeMap<Key, Sample<ControlReading>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwm_enable_values() {
        assert_eq!(ControlMode::from_pwm_enable(0), ControlMode::Full);
        assert_eq!(ControlMode::from_pwm_enable(1), ControlMode::Manual);
        assert_eq!(ControlMode::from_pwm_enable(2), ControlMode::Automatic(2));
        assert_eq!(ControlMode::from_pwm_enable(5), ControlMode::Automatic(5));
        assert_eq!(ControlMode::from_pwm_enable(-1), ControlMode::Unknown);
    }
}
