//! A configuration resolved against a machine (DESIGN.md §4, §6.3): defaults filled in, floors and emergency limits worked out,
//! everything [`crate::control::tick`] needs and nothing it has to look up twice.
use crate::config::{self, Config, CurveKind, HysteresisDirection, Measured, Problem, VirtualKind};
use crate::control::LoopState;
use crate::{ControlKind, Inventory, Key, LimitKind};
use std::collections::{BTreeMap, BTreeSet};

/// Without calibration, a fan that may not stop never runs below this.
pub const UNCALIBRATED_FLOOR: f64 = 30.0;
/// The margin kept above a calibrated stop duty when a fan may not stop, and above a GPU fan's always (§4.5).
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
    /// Controls in `controls` that the configuration no longer names but Penna still held when this plan replaced the last one
    /// ([`Plan::keep_held`]). Nothing assigns them, so the loop gives them back; then [`Plan::drop_released`] lets them go.
    pub carried: BTreeSet<Key>,
    /// Controls in `controls` that setup measures and the configuration doesn't name ([`Plan::carry`]), kept while setup runs.
    pub setup: BTreeSet<Key>,
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

    /// The raw value for a duty: 0 to the control's scale (255 for PWM), rounded up so the fan never gets less than it was given,
    /// or NVML percent within its range.
    pub fn raw(&self, duty: f64) -> u32 {
        match self.kind {
            Some(ControlKind::Nvml { min, max }) => (duty.round() as i64).clamp(min as i64, max as i64) as u32,
            _ => {
                let scale = self.scale() as f64;
                (duty * scale / 100.0 - 1e-9).ceil().clamp(0.0, scale) as u32
            }
        }
    }

    /// What 100 % is written as.
    pub fn scale(&self) -> u32 {
        match self.kind {
            Some(ControlKind::Duty { scale }) => scale,
            Some(ControlKind::Nvml { max, .. }) => max,
            _ => 255,
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
                CurvePlan {
                    id: c.id.clone(),
                    kind: c.kind.clone(),
                    response: c.response.unwrap_or_else(|| default_response(&c.kind)),
                    hysteresis: c.hysteresis.unwrap_or_else(|| default_hysteresis(&c.kind)),
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
            let (plan, found) = control_plan(&cfg, present.get(&key).map(|f| (f.0, f.1)), measured);
            problems.extend(found);
            controls.insert(key, plan);
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let sources = config.physical_sources();
        let emergency = emergency_limits(inventory, cpu_model, &sources);
        let physical: BTreeSet<Key> = sources.into_iter().chain(emergency.keys().cloned()).collect();
        Ok(Plan {
            config,
            curves,
            virtuals,
            controls,
            emergency,
            physical: physical.into_iter().collect(),
            carried: BTreeSet::new(),
            setup: BTreeSet::new(),
        })
    }

    /// Keeps every control Penna holds that this plan, about to replace `prev`, doesn't name: a fan that only a discarded draft
    /// adopted would otherwise stay manual at its last duty, because the loop only steps the controls its plan names (§6.1). It
    /// comes over with nothing assigned, so the next tick gives it back, and with the kind it has now, so a fan whose device went
    /// away is `absent` and waits in `held.json` (§3.6).
    pub fn keep_held(&mut self, prev: &Plan, state: &LoopState, inventory: &Inventory) {
        let held = state.controls.iter().filter(|(_, c)| c.held).map(|(k, _)| k);
        let keys: BTreeSet<&Key> = held.chain(state.measuring.keys()).chain(prev.setup.iter()).collect();
        for key in keys {
            if self.controls.contains_key(key) {
                continue;
            }
            let Some(old) = prev.controls.get(key) else { continue };
            let mut c = old.clone();
            c.kind = inventory.devices.iter().flat_map(|d| &d.controls).find(|i| &i.key == key).map(|i| i.kind);
            self.controls.insert(key.clone(), c);
            if prev.setup.contains(key) {
                self.setup.insert(key.clone());
            } else {
                self.carried.insert(key.clone());
            }
        }
    }

    /// Lets go of carried controls once they are given back.
    pub fn drop_released(&mut self, state: &LoopState) {
        let done: Vec<Key> = self
            .carried
            .iter()
            .filter(|k| !state.measuring.contains_key(*k) && state.controls.get(*k).is_none_or(|c| !c.held))
            .cloned()
            .collect();
        for key in done {
            self.carried.remove(&key);
            self.controls.remove(&key);
        }
    }

    /// Brings in controls that setup measures and the configuration doesn't name (§5), unassigned. They stay while setup runs,
    /// so the loop takes each when setup asks for a duty; [`Plan::end_setup`] then lets them be given back and go.
    pub fn carry(&mut self, keys: &[Key], inventory: &Inventory, measured: &Measured) {
        for key in keys {
            if self.controls.contains_key(key) {
                continue;
            }
            let present = inventory.devices.iter().flat_map(|d| &d.controls).find(|c| &c.key == key).map(|c| (c.kind, c.label.as_str()));
            let (plan, _) = control_plan(&config::ControlConfig::defaults(key.clone()), present, measured);
            self.controls.insert(key.clone(), plan);
            self.setup.insert(key.clone());
        }
    }

    /// Setup is over: its controls are given back like any carried control, then let go.
    pub fn end_setup(&mut self) {
        self.carried.append(&mut self.setup);
    }

    /// The curve a profile gives a control, or `None` for firmware. Defaults gives every control to its firmware (§4.7).
    pub fn assignment(&self, profile: &str, key: &Key) -> Option<&str> {
        if profile == config::DEFAULTS {
            return None;
        }
        let curve = self.config.profiles.get(profile)?.get(key)?;
        (curve != config::FIRMWARE).then_some(curve.as_str())
    }
}

/// One control's limits resolved against what is on the machine (`present`: its kind and hardware label) and what calibration
/// measured, with what is wrong with them. The studio uses it for controls no profile names yet.
pub fn control_plan(cfg: &config::ControlConfig, present: Option<(ControlKind, &str)>, measured: &Measured) -> (ControlPlan, Vec<Problem>) {
    let key = &cfg.key;
    let mut problems = Vec::new();
    let m = measured.get(key);
    let stop = m.and_then(|m| m.stop);
    let kind = present.map(|f| f.0);
    let at = format!("control {key}");
    let lowest = m.and_then(|m| m.lowest);
    let floor = match kind {
        // A GPU fan has a stop too, once setup has measured one: the 3090's stand still below 35–45 % in manual, a different
        // duty each run, and held at it for long enough they stall there too (§5.4). The floor keeps the margin above it, and one
        // that may stop is handed back to its driver below the floor.
        Some(ControlKind::Nvml { min, .. }) => cfg.min.max(min as f64).max(stop.map_or(0.0, |s| s + STOP_MARGIN)),
        _ if cfg.may_stop => cfg.min.max(stop.unwrap_or(0.0)),
        // Measured down to its lowest without stopping is calibration enough: a pump, or a header that races below it.
        _ => cfg.min.max(stop.map_or(lowest.unwrap_or(UNCALIBRATED_FLOOR), |s| s + STOP_MARGIN)),
    }
    .max(lowest.unwrap_or(0.0));
    if kind.is_some() && !matches!(kind, Some(ControlKind::Nvml { .. })) && cfg.may_stop && stop.is_none() {
        problems.push(Problem { at: at.clone(), message: "may_stop needs a calibrated stop duty (penna calibrate)".into() });
    }
    if let Some(ControlKind::Nvml { max, .. }) = kind
        && cfg.failsafe > max as f64
    {
        problems
            .push(Problem { at: at.clone(), message: format!("failsafe ({} %) is above what this GPU accepts ({max} %)", cfg.failsafe) });
    }
    if kind.is_some() && cfg.failsafe < floor {
        problems.push(Problem { at: at.clone(), message: format!("failsafe ({} %) is below this fan's floor ({floor} %)", cfg.failsafe) });
    }
    let start = cfg.start.or(m.and_then(|m| m.start)).unwrap_or(floor.max(50.0)).max(floor);
    let label = cfg.label.clone().or_else(|| present.map(|f| f.1.to_owned())).unwrap_or_else(|| key.channel().to_owned());
    let plan = ControlPlan {
        key: key.clone(),
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
    };
    (plan, problems)
}

/// A curve's response time when the configuration names none (§4.4): 5 s on a CPU sensor, none otherwise.
pub fn default_response(kind: &CurveKind) -> f64 {
    if kind.source().is_some_and(is_cpu_sensor) { CPU_RESPONSE } else { 0.0 }
}

/// A curve's hysteresis when the configuration names none (§4.4): 3 °C on a graph, none otherwise.
pub fn default_hysteresis(kind: &CurveKind) -> f64 {
    if matches!(kind, CurveKind::Graph { .. }) { DEFAULT_HYSTERESIS } else { 0.0 }
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
                        kind: ControlKind::Duty { scale: 255 },
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
    fn a_held_control_the_next_plan_drops_is_kept_until_it_is_given_back() {
        let fan = Key::parse("hwmon/it87/it87.2608/pwm1").unwrap();
        let mut cfg = Config::parse("version = 1\nprofile = \"p\"\n[profiles.p]\n").unwrap();
        cfg.curves.push(config::Curve {
            id: "flat".into(),
            label: None,
            kind: CurveKind::Flat { percent: 50.0 },
            response: None,
            hysteresis: None,
            hysteresis_direction: HysteresisDirection::Falling,
        });
        let mut adopting = cfg.clone();
        adopting.profiles.get_mut("p").unwrap().insert(fan.clone(), "flat".into());
        let measured = Measured::default();
        let before = Plan::new(adopting, &inventory(), &measured, None).unwrap();
        let mut state = LoopState::new(&before);
        state.controls.entry(fan.clone()).or_default().held = true;

        let mut after = Plan::new(cfg.clone(), &inventory(), &measured, None).unwrap();
        assert!(!after.controls.contains_key(&fan));
        after.keep_held(&before, &state, &inventory());
        assert_eq!(after.controls[&fan].kind, Some(ControlKind::Duty { scale: 255 }));
        assert_eq!(after.assignment("p", &fan), None, "nothing assigns it, so the loop gives it back");
        after.drop_released(&state);
        assert!(after.controls.contains_key(&fan), "still held");
        state.controls.get_mut(&fan).unwrap().held = false;
        after.drop_released(&state);
        assert!(!after.controls.contains_key(&fan) && after.carried.is_empty());

        // Its device is gone: it comes along absent, and waits in held.json rather than being written.
        state.controls.get_mut(&fan).unwrap().held = true;
        let mut gone = inventory();
        gone.devices.retain(|d| d.driver != "it87");
        let mut after = Plan::new(cfg, &gone, &measured, None).unwrap();
        after.keep_held(&before, &state, &gone);
        assert_eq!(after.controls[&fan].kind, None);
    }

    #[test]
    fn tjmax_table() {
        assert_eq!(tjmax("AMD Ryzen 9 5950X 16-Core Processor"), Some(90.0));
        assert_eq!(tjmax("AMD Ryzen 5 5600X 6-Core Processor"), Some(95.0));
        assert_eq!(tjmax("Intel(R) Core(TM) i9-12900K"), None);
    }
}
