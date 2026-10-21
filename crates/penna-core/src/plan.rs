//! A configuration resolved against a machine (DESIGN.md §4, §6.3): defaults filled in, floors and emergency limits worked out,
//! everything [`crate::control::tick`] needs and nothing it has to look up twice.
use crate::config::{self, Config, CurveKind, HysteresisDirection, Measured, Problem, VirtualKind};
use crate::{ControlKind, Inventory, Key, LimitKind};
use std::collections::{BTreeMap, BTreeSet};

/// Without calibration, a fan that may not stop never runs below this.
pub const UNCALIBRATED_FLOOR: f64 = 30.0;
/// The margin kept above a calibrated stop duty when a fan may not stop (§4.5).
pub const STOP_MARGIN: f64 = 5.0;
/// An NVML fan resumes manual control this far above its floor (§4.5).
pub const RESUME_MARGIN: f64 = 5.0;
/// Hysteresis on a graph when the configuration names none (§4.4).
pub const DEFAULT_HYSTERESIS: f64 = 3.0;
/// Response time for CPU sources when the configuration names none (§4.4).
pub const CPU_RESPONSE: f64 = 5.0;
/// The emergency limit for a sensor with nothing better (§6.3).
pub const DEFAULT_EMERGENCY: f64 = 95.0;

const CPU_DRIVERS: &[&str] = &["k10temp", "zenpower", "coretemp"];

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub config: Config,
    /// Curves in evaluation order: every mix after what it mixes.
    pub curves: Vec<CurvePlan>,
    /// Virtual sensors in evaluation order.
    pub virtuals: Vec<(Key, VirtualKind)>,
    /// Every control a profile names, present on this machine or not.
    pub controls: BTreeMap<Key, ControlPlan>,
    /// Sensor → the temperature at which every adopted fan goes to 100 %.
    pub emergency: BTreeMap<Key, f64>,
    /// Every physical sensor the loop reads: what curves read, and what can declare an emergency.
    pub physical: Vec<Key>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurvePlan {
    pub id: String,
    pub kind: CurveKind,
    pub response: f64,
    pub hysteresis: f64,
    pub direction: HysteresisDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlPlan {
    pub key: Key,
    pub label: String,
    /// `None`: named by a profile but not on this machine.
    pub kind: Option<ControlKind>,
    pub min: f64,
    pub max: f64,
    pub offset: f64,
    pub step_up: f64,
    pub step_down: f64,
    pub may_stop: bool,
    /// Calibrated: the lowest duty that still turns the fan.
    pub stop: Option<f64>,
    /// The kick that spins a stopped fan.
    pub start: f64,
    pub failsafe: f64,
    /// The lowest non-zero duty this control is ever given.
    pub floor: f64,
}

impl ControlPlan {
    pub fn is_nvml(&self) -> bool {
        matches!(self.kind, Some(ControlKind::Nvml { .. }))
    }

    /// The raw value for a duty: 0–255 PWM, rounded up so the fan never gets less than it was given, or NVML percent within its range.
    pub fn raw(&self, duty: f64) -> u32 {
        match self.kind {
            Some(ControlKind::Nvml { min, max }) => (duty.round() as i64).clamp(min as i64, max as i64) as u32,
            _ => (duty * 255.0 / 100.0 - 1e-9).ceil().clamp(0.0, 255.0) as u32,
        }
    }
}

impl Plan {
    /// `cpu_model` is `/proc/cpuinfo`'s model name, for the TjMax table.
    pub fn new(config: Config, inventory: &Inventory, measured: &Measured, cpu_model: Option<&str>) -> Result<Plan, Vec<Problem>> {
        let mut problems = config.validate();
        if !problems.is_empty() {
            return Err(problems);
        }
        let order = config::curve_order(&config).map_err(|c| vec![Problem { at: format!("curve {c:?}"), message: "loops".into() }])?;
        let curves = order
            .iter()
            .map(|id| {
                let c = config.curve(id).expect("ordered curves exist");
                let cpu = c.kind.source().is_some_and(is_cpu_sensor);
                CurvePlan {
                    id: c.id.clone(),
                    kind: c.kind.clone(),
                    response: c.response.unwrap_or(if cpu { CPU_RESPONSE } else { 0.0 }),
                    hysteresis: c.hysteresis.unwrap_or(if matches!(c.kind, CurveKind::Graph { .. }) { DEFAULT_HYSTERESIS } else { 0.0 }),
                    direction: c.hysteresis_direction,
                }
            })
            .collect();
        let virtuals = config::virtual_order(&config)
            .map_err(|k| vec![Problem { at: format!("sensor {k}"), message: "loops".into() }])?
            .into_iter()
            .map(|k| {
                let v = config.virtual_sensor(&k).expect("ordered sensors exist");
                (k, v.kind.clone())
            })
            .collect();

        let present: BTreeMap<&Key, (ControlKind, &str)> =
            inventory.devices.iter().flat_map(|d| d.controls.iter()).map(|c| (&c.key, (c.kind, c.label.as_str()))).collect();
        let mut keys: BTreeSet<Key> = config.controls.iter().map(|c| c.key.clone()).collect();
        keys.extend(config.profiles.values().flat_map(|p| p.keys().cloned()));
        let mut controls = BTreeMap::new();
        for key in keys {
            let cfg = config.control(&key).cloned().unwrap_or_else(|| config::ControlConfig::defaults(key.clone()));
            let found = present.get(&key);
            let m = measured.get(&key);
            let stop = m.and_then(|m| m.stop);
            let kind = found.map(|f| f.0);
            let at = format!("control {key}");
            let floor = match kind {
                Some(ControlKind::Nvml { min, .. }) => cfg.min.max(min as f64),
                _ if cfg.may_stop => cfg.min.max(stop.unwrap_or(0.0)),
                _ => cfg.min.max(stop.map_or(UNCALIBRATED_FLOOR, |s| s + STOP_MARGIN)),
            };
            if kind.is_some() && !matches!(kind, Some(ControlKind::Nvml { .. })) && cfg.may_stop && stop.is_none() {
                problems.push(Problem { at: at.clone(), message: "may_stop needs a calibrated stop duty (penna calibrate)".into() });
            }
            if let Some(ControlKind::Nvml { max, .. }) = kind
                && cfg.failsafe > max as f64
            {
                problems.push(Problem {
                    at: at.clone(),
                    message: format!("failsafe ({} %) is above what this GPU accepts ({max} %)", cfg.failsafe),
                });
            }
            if kind.is_some() && cfg.failsafe < floor {
                problems.push(Problem {
                    at: at.clone(),
                    message: format!("failsafe ({} %) is below this fan's floor ({floor} %)", cfg.failsafe),
                });
            }
            let start = cfg.start.or(m.and_then(|m| m.start)).unwrap_or(floor.max(50.0)).max(floor);
            let label = cfg.label.clone().or_else(|| found.map(|f| f.1.to_owned())).unwrap_or_else(|| key.channel().to_owned());
            controls.insert(
                key.clone(),
                ControlPlan {
                    key,
                    label,
                    kind,
                    min: cfg.min,
                    max: cfg.max.max(floor),
                    offset: cfg.offset,
                    step_up: cfg.step_up,
                    step_down: cfg.step_down,
                    may_stop: cfg.may_stop,
                    stop,
                    start: start.min(cfg.max.max(floor)),
                    failsafe: cfg.failsafe,
                    floor,
                },
            );
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let sources = config.physical_sources();
        let emergency = emergency_limits(inventory, cpu_model, &sources);
        let physical: BTreeSet<Key> = sources.into_iter().chain(emergency.keys().cloned()).collect();
        Ok(Plan { config, curves, virtuals, controls, emergency, physical: physical.into_iter().collect() })
    }

    /// The curve a profile gives a control, or `None` for firmware.
    pub fn assignment(&self, profile: &str, key: &Key) -> Option<&str> {
        let curve = self.config.profiles.get(profile)?.get(key)?;
        (curve != config::FIRMWARE).then_some(curve.as_str())
    }
}

fn is_cpu_sensor(key: &Key) -> bool {
    key.as_str().strip_prefix("hwmon/").and_then(|r| r.split('/').next()).is_some_and(|d| CPU_DRIVERS.contains(&d))
}

/// Which sensors can declare an emergency, and at what (§6.3). Every sensor with a plausible limit of its own counts, and so does
/// every CPU sensor (TjMax). Other sensors count only when a curve reads them: Super I/O chips report unconnected inputs as 127 °C,
/// and those must not hold every fan at 100 % for ever.
pub fn emergency_limits(inventory: &Inventory, cpu_model: Option<&str>, sources: &BTreeSet<Key>) -> BTreeMap<Key, f64> {
    let mut out = BTreeMap::new();
    for device in &inventory.devices {
        for s in &device.sensors {
            let own = s.limits.iter().find(|l| matches!(l.kind, LimitKind::Crit | LimitKind::GpuMax)).map(|l| l.celsius);
            let limit = if let Some(own) = own {
                Some(own)
            } else if CPU_DRIVERS.contains(&device.driver.as_str()) {
                Some(cpu_model.and_then(tjmax).unwrap_or(DEFAULT_EMERGENCY))
            } else if sources.contains(&s.key) {
                Some(DEFAULT_EMERGENCY)
            } else {
                None
            };
            if let Some(l) = limit {
                out.insert(s.key.clone(), l);
            }
        }
    }
    out
}

/// AMD's published maximum operating temperatures for a few desktop CPUs.
pub fn tjmax(model: &str) -> Option<f64> {
    const TABLE: &[(&str, f64)] =
        &[("Ryzen 9 5950X", 90.0), ("Ryzen 9 5900X", 90.0), ("Ryzen 7 5800X", 90.0), ("Ryzen 5 5600X", 95.0), ("Ryzen 9 7950X", 95.0)];
    TABLE.iter().find(|(m, _)| model.contains(m)).map(|(_, t)| *t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlInfo, Device, Limit, SensorInfo, Source};

    pub fn inventory() -> Inventory {
        let sensor = |key: &str, label: &str, limits: Vec<Limit>| SensorInfo {
            key: Key::parse(key).unwrap(),
            label: label.into(),
            limits,
            ignored_limits: vec![],
            below_ambient: false,
        };
        Inventory {
            devices: vec![
                Device {
                    id: "hwmon/k10temp/0000:00:18.3".into(),
                    source: Source::Hwmon,
                    driver: "k10temp".into(),
                    label: "AMD CPU".into(),
                    location: "0000:00:18.3".into(),
                    model: None,
                    sensors: vec![sensor("hwmon/k10temp/0000:00:18.3/temp1", "Tctl", vec![])],
                    tachs: vec![],
                    controls: vec![],
                },
                Device {
                    id: "hwmon/it87/it87.2608".into(),
                    source: Source::Hwmon,
                    driver: "it87".into(),
                    label: "Motherboard".into(),
                    location: "it87.2608".into(),
                    model: None,
                    sensors: vec![sensor("hwmon/it87/it87.2608/temp3", "temp3", vec![])],
                    tachs: vec![],
                    controls: vec![ControlInfo {
                        key: Key::parse("hwmon/it87/it87.2608/pwm1").unwrap(),
                        label: "pwm1".into(),
                        kind: ControlKind::Pwm,
                    }],
                },
                Device {
                    id: "nvml/GPU-1".into(),
                    source: Source::Nvml,
                    driver: "nvml".into(),
                    label: "GPU".into(),
                    location: "0000:09:00.0".into(),
                    model: None,
                    sensors: vec![sensor("nvml/GPU-1/temp", "GPU", vec![Limit { kind: LimitKind::GpuMax, celsius: 93.0 }])],
                    tachs: vec![],
                    controls: vec![ControlInfo {
                        key: Key::parse("nvml/GPU-1/fan0").unwrap(),
                        label: "Fan 1".into(),
                        kind: ControlKind::Nvml { min: 30, max: 100 },
                    }],
                },
            ],
            ..Inventory::default()
        }
    }

    #[test]
    fn emergency_limits_follow_the_sensors() {
        let l = emergency_limits(&inventory(), Some("AMD Ryzen 9 5950X 16-Core Processor"), &BTreeSet::new());
        assert_eq!(l.get(&Key::parse("hwmon/k10temp/0000:00:18.3/temp1").unwrap()), Some(&90.0));
        assert_eq!(l.get(&Key::parse("nvml/GPU-1/temp").unwrap()), Some(&93.0));
        assert_eq!(l.get(&Key::parse("hwmon/it87/it87.2608/temp3").unwrap()), None, "an unread Super I/O input is not watched");
        let read = BTreeSet::from([Key::parse("hwmon/it87/it87.2608/temp3").unwrap()]);
        let l = emergency_limits(&inventory(), None, &read);
        assert_eq!(l.get(&Key::parse("hwmon/it87/it87.2608/temp3").unwrap()), Some(&95.0));
        assert_eq!(l.get(&Key::parse("hwmon/k10temp/0000:00:18.3/temp1").unwrap()), Some(&95.0), "an unknown CPU gets 95 °C");
    }

    #[test]
    fn tjmax_table() {
        assert_eq!(tjmax("AMD Ryzen 9 5950X 16-Core Processor"), Some(90.0));
        assert_eq!(tjmax("AMD Ryzen 5 5600X 6-Core Processor"), Some(95.0));
        assert_eq!(tjmax("Intel(R) Core(TM) i9-12900K"), None);
    }
}
