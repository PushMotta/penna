//! Generated curves (DESIGN.md §5.5): a curve per role and profile from what setup measured, delivered as a draft.
//!
//! Points are placed in airflow, a share of a fan's range from its floor to its top speed, and turned into duty through its
//! calibration, so a curve means the same on a fan whose RPM barely moves below 40 % as on one that is linear. Fans that share a
//! role share its curve: the curve's duty gives them, on average, the airflow it was placed at. Each fan's floor comes from the
//! plan (§4.5), which lifts it there whatever the curve asks, and its top from its own calibration. A fan that may stop isn't
//! lifted, so a curve it shares starts at its floor.
use crate::config::{Config, ControlConfig, Curve, CurveKind, HysteresisDirection, Measured, MeasuredControl, MixOp};
use crate::plan::{DEFAULT_EMERGENCY, control_plan, tjmax};
use crate::setup::{Role, WriteResult};
use crate::starter::{PROFILES, cpu_control_temperature};
use crate::{ControlKind, Inventory, Key, LimitKind, SensorInfo, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How each profile places a CPU curve: the floor until idle plus the first, 50 % airflow at the limit less the second, 100 % at
/// the limit less the third.
const CPU: [[f64; 3]; 3] = [[15.0, 15.0, 3.0], [10.0, 20.0, 8.0], [5.0, 30.0, 15.0]];
/// A GPU curve: the driver's curve below the target less the first, 50 % at the target less the second. 100 % at the limit less
/// 3, the limit less 5, and the target.
const GPU: [[f64; 2]; 3] = [[10.0, 0.0], [15.0, 5.0], [20.0, 10.0]];
/// Case fans start this far above idle, on either source, and go no higher than this share of their airflow.
const CASE_START: f64 = 15.0;
const CASE_CAP: [f64; 3] = [0.5, 0.75, 1.0];
/// Pumps run flat, in duty.
const PUMP: [f64; 3] = [70.0, 85.0, 100.0];
/// Chipset fans: the floor until 65 °C, 100 % at 80 °C. One its firmware stops may stop below 60 °C (§5.3).
const CHIPSET: [f64; 3] = [60.0, 65.0, 80.0];
/// A fan reaches its top speed at the lowest duty that turns it this close to its fastest.
const TOP: f64 = 0.98;

/// A source's anchors (§5.5): what it read idle, what a profile tries to keep it under, and its limit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchors {
    pub idle: f64,
    pub target: Option<f64>,
    pub limit: Option<f64>,
}

/// The draft, and what went into it.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    /// The configuration to save: the one it started from, with the generated curves, the fans' limits and all three profiles.
    /// Its active profile is unchanged.
    pub config: Config,
    /// The curves it made, in order.
    pub curves: Vec<String>,
    pub anchors: BTreeMap<Key, Anchors>,
    /// Each fan it left alone, and why.
    pub notes: Vec<String>,
}

/// What setup proposes once it has measured (§5.5, §11.3 call 9), for a surface to show with the draft.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// The curves in the draft that setup made.
    pub curves: Vec<String>,
    /// The anchors of every source they follow, to draw on the axis.
    pub anchors: BTreeMap<Key, Anchors>,
    /// Each fan left with its firmware, and why.
    pub notes: Vec<String>,
    /// Why there is no draft: another window holds unsaved changes.
    pub refused: Option<String>,
}

impl Generated {
    /// What keeping the draft changes, for the note that proposes it. Run again, setup finds a profile it writes already on, and
    /// the fans run the draft's curves as soon as it is made.
    pub fn draft_words(&self) -> String {
        let on = &self.config.profile;
        let running = self.config.profiles.get(on).is_some_and(|a| a.values().any(|c| self.curves.contains(c)));
        match PROFILES.iter().position(|p| p == on).filter(|_| running) {
            Some(p) => format!("{} is on, so the fans run them until the draft is saved or left", title(p)),
            None => "nothing changes until it is saved".to_owned(),
        }
    }
}

impl From<&Generated> for Proposal {
    fn from(g: &Generated) -> Proposal {
        Proposal { curves: g.curves.clone(), anchors: g.anchors.clone(), notes: g.notes.clone(), refused: None }
    }
}

/// The CPU source curves follow, and its limit: its own critical temperature, the model's TjMax, or 95 °C.
pub fn cpu_source(inventory: &Inventory, cpu_model: Option<&str>) -> Option<(Key, f64)> {
    let key = cpu_control_temperature(inventory)?;
    let own = sensor(inventory, &key)?.limits.iter().find(|l| matches!(l.kind, LimitKind::Crit | LimitKind::GpuMax)).map(|l| l.celsius);
    let limit = own.or_else(|| cpu_model.and_then(tjmax)).unwrap_or(DEFAULT_EMERGENCY);
    Some((key, limit))
}

/// Curves for every measured fan with a role, in all three profiles, starting from `base` (§5.5). A curve that already exists
/// under a generated id is replaced: running setup again proposes new curves, and saving the draft is what keeps them.
pub fn generate(base: &Config, inventory: &Inventory, measured: &Measured, cpu_model: Option<&str>) -> Generated {
    let mut g = Builder { config: base.clone(), curves: Vec::new(), anchors: BTreeMap::new(), notes: Vec::new() };
    let mut roles: BTreeMap<Role, Vec<(&MeasuredControl, ControlKind, String)>> = BTreeMap::new();
    for m in &measured.controls {
        let Some((kind, label)) = control(inventory, &m.key) else { continue };
        let label = base.control(&m.key).and_then(|c| c.label.clone()).unwrap_or(label);
        if m.write != Some(WriteResult::Ok) || m.empty || !(m.paired || matches!(kind, ControlKind::Nvml { .. })) {
            continue;
        }
        match m.role.or(m.suggested) {
            Some(role) => roles.entry(role).or_default().push((m, kind, label)),
            None => g.notes.push(format!("{label}: no role, so it stays with its firmware")),
        }
    }
    let cpu = cpu_source(inventory, cpu_model).and_then(|(key, limit)| g.cpu_anchors(measured, key, limit));
    let gpus = g.gpu_anchors(inventory, measured);
    for (role, fans) in &roles {
        let fans: Vec<Fan> = fans.iter().map(|(m, kind, label)| g.fan(m, *kind, label, *role, measured)).collect();
        match role {
            Role::Cpu => match &cpu {
                Some(c) => g.cpu(&fans, c),
                None => g.left(&fans, "there is no CPU temperature to follow"),
            },
            Role::Gpu => {
                for (i, gpu) in gpus.iter().enumerate() {
                    let own: Vec<Fan> = fans.iter().filter(|f| f.key.as_str().starts_with(&gpu.device)).cloned().collect();
                    if !own.is_empty() {
                        g.gpu(&own, gpu, i, gpus.len());
                    }
                }
                let lost: Vec<Fan> =
                    fans.iter().filter(|f| !gpus.iter().any(|gpu| f.key.as_str().starts_with(&gpu.device))).cloned().collect();
                g.left(&lost, "its GPU's temperature wasn't measured");
            }
            Role::Case if cpu.is_none() && gpus.is_empty() => g.left(&fans, "there is no CPU or GPU temperature to follow"),
            Role::Case => g.case(&fans, cpu.as_ref(), &gpus),
            Role::Pump => g.pump(&fans),
            Role::Chipset => match chipset_sensor(inventory).and_then(|s| g.chipset_anchors(measured, s)) {
                Some(s) => g.chipset(&fans, &s),
                None => g.left(&fans, "the board has no chipset temperature, so it stays with its firmware"),
            },
            Role::Laptop if cpu.is_none() && gpus.is_empty() => g.left(&fans, "there is no CPU or GPU temperature to follow"),
            Role::Laptop => g.laptop(&fans, cpu.as_ref(), &gpus),
        }
    }
    Generated { config: g.config, curves: g.curves, anchors: g.anchors, notes: g.notes }
}

/// A source the curves follow.
#[derive(Clone, Debug)]
struct Src {
    key: Key,
    a: Anchors,
    /// The device whose fans it cools: `nvml/GPU-…`.
    device: String,
}

/// One fan, as a curve sees it.
#[derive(Clone, Debug)]
struct Fan {
    key: Key,
    label: String,
    /// Where the plan lifts it (§4.5), and the lowest duty at which it reaches its top speed.
    floor: f64,
    top: f64,
    /// Below its floor it stops or goes back to its driver instead.
    may_stop: bool,
    /// Its calibration, made monotonic (§5.4, step 3): duty → RPM.
    table: Vec<[f64; 2]>,
}

impl Fan {
    fn rpm(&self, duty: f64) -> f64 {
        crate::control::graph(&self.table, duty)
    }

    /// Its share of its airflow at `duty`: 0 at its floor, 1 at its top. Uncalibrated, duty stands in for airflow.
    fn airflow(&self, duty: f64) -> f64 {
        if self.top <= self.floor {
            return if duty >= self.top { 1.0 } else { 0.0 };
        }
        if self.table.len() < 2 {
            return ((duty - self.floor) / (self.top - self.floor)).clamp(0.0, 1.0);
        }
        let (lo, hi) = (self.rpm(self.floor), self.rpm(self.top));
        if hi <= lo {
            return if duty >= self.top { 1.0 } else { 0.0 };
        }
        ((self.rpm(duty) - lo) / (hi - lo)).clamp(0.0, 1.0)
    }
}

/// The duty at which `fans` give, on average, this share of their airflow. At 0 it is the lowest floor, raised to the highest of a
/// fan that may stop: below its floor such a fan stops, or goes back to its driver, rather than being lifted (§4.5), so a curve
/// it shares starts where every one of them runs.
fn duty_for(fans: &[Fan], share: f64) -> f64 {
    let lowest = fans.iter().map(|f| f.floor).fold(f64::INFINITY, f64::min);
    let lo = fans.iter().filter(|f| f.may_stop).map(|f| f.floor).fold(lowest, f64::max);
    let hi = fans.iter().map(|f| f.top).fold(0.0, f64::max).max(lo);
    if share <= 0.0 || fans.is_empty() {
        return lo.min(100.0).round();
    }
    let mean = |d: f64| fans.iter().map(|f| f.airflow(d)).sum::<f64>() / fans.len() as f64;
    let (mut a, mut b) = (lo, hi);
    for _ in 0..40 {
        let m = (a + b) / 2.0;
        if mean(m) >= share { b = m } else { a = m }
    }
    b.round()
}

/// A calibration table that never falls as the duty rises: each point at least as fast as every one below it.
pub fn monotonic(table: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut sorted = table.to_vec();
    sorted.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut fastest = f64::NEG_INFINITY;
    sorted
        .into_iter()
        .map(|[d, r]| {
            fastest = fastest.max(r);
            [d, fastest]
        })
        .collect()
}

/// Points at these airflow shares, at temperatures spread evenly between the anchors, as duty for `fans`. Temperatures rise by
/// at least 1 °C, and a point that adds nothing is left out.
fn points(fans: &[Fan], at: &[(f64, f64)]) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::new();
    for &(t, share) in at {
        let t = out.last().map_or(t.round(), |p| t.round().max(p[0] + 1.0));
        let d = duty_for(fans, share);
        if out.len() >= 2 && out[out.len() - 1][1] == d && out[out.len() - 2][1] == d {
            out.last_mut().expect("two points")[0] = t;
            continue;
        }
        out.push([t, d]);
    }
    out
}

/// Points as [`points`] places them, with a 0 % point 1 °C below the first when the fans may stop.
fn resting(fans: &[Fan], at: &[(f64, f64)], rest: bool) -> Vec<[f64; 2]> {
    let mut pts = points(fans, at);
    if let (true, Some(first)) = (rest, pts.first().copied()) {
        pts.insert(0, [first[0] - 1.0, 0.0]);
    }
    pts
}

/// From the floor at `t0` to `share` at `t1`, with points at a quarter, half and three quarters of the way.
fn ramp(t0: f64, t1: f64, share: f64) -> Vec<(f64, f64)> {
    (0..=4).map(|i| i as f64 / 4.0).map(|k| (t0 + (t1 - t0) * k, share * k)).collect()
}

/// The floor at `t0`, 50 % at `t50` and 100 % at `t100`, with a point halfway along each leg.
fn legs(t0: f64, t50: f64, t100: f64) -> Vec<(f64, f64)> {
    vec![(t0, 0.0), ((t0 + t50) / 2.0, 0.25), (t50, 0.5), ((t50 + t100) / 2.0, 0.75), (t100, 1.0)]
}

struct Builder {
    config: Config,
    curves: Vec<String>,
    anchors: BTreeMap<Key, Anchors>,
    notes: Vec<String>,
}

impl Builder {
    fn cpu_anchors(&mut self, measured: &Measured, key: Key, limit: f64) -> Option<Src> {
        let Some(&idle) = measured.idle.get(&key) else {
            self.notes.push("The CPU's idle temperature wasn't measured: its curves need setup's idle sample".into());
            return None;
        };
        let a = Anchors { idle: idle.round(), target: None, limit: Some(limit) };
        self.anchors.insert(key.clone(), a);
        Some(Src { key, a, device: String::new() })
    }

    /// Each GPU's temperature: an NVIDIA GPU's own, and an AMD (or nouveau) GPU's `edge`, the one closest to what NVML reports.
    /// Its limit is NVML's maximum or the sensor's critical temperature, and its target NVML's acoustic threshold or the limit
    /// less 10 °C.
    fn gpu_anchors(&mut self, inventory: &Inventory, measured: &Measured) -> Vec<Src> {
        let mut out = Vec::new();
        let is_gpu = |d: &&crate::Device| match d.source {
            Source::Nvml => true,
            Source::Hwmon => crate::drivers::lookup(&d.driver).is_some_and(|e| e.class == crate::drivers::Class::Gpu),
        };
        for d in inventory.devices.iter().filter(is_gpu) {
            let sensor = match d.source {
                Source::Nvml => d.sensors.iter().find(|s| s.key.channel() == "temp"),
                Source::Hwmon => d.sensors.iter().find(|s| s.label == "edge").or_else(|| d.sensors.first()),
            };
            let Some(s) = sensor else { continue };
            let limit = s
                .limits
                .iter()
                .find(|l| l.kind == LimitKind::GpuMax)
                .or_else(|| s.limits.iter().find(|l| l.kind == LimitKind::Crit))
                .map_or(DEFAULT_EMERGENCY, |l| l.celsius);
            let target = s.limits.iter().find(|l| l.kind == LimitKind::Acoustic).map_or(limit - 10.0, |l| l.celsius);
            let Some(&idle) = measured.idle.get(&s.key) else { continue };
            let a = Anchors { idle: idle.round(), target: Some(target), limit: Some(limit) };
            self.anchors.insert(s.key.clone(), a);
            out.push(Src { key: s.key.clone(), a, device: d.id.clone() });
        }
        out
    }

    fn chipset_anchors(&mut self, measured: &Measured, s: &SensorInfo) -> Option<Src> {
        let idle = *measured.idle.get(&s.key)?;
        // Its curve's temperatures are fixed, not measured: only its idle is an anchor.
        let a = Anchors { idle: idle.round(), target: None, limit: None };
        self.anchors.insert(s.key.clone(), a);
        Some(Src { key: s.key.clone(), a, device: String::new() })
    }

    /// A fan's limits in the draft: what the configuration says, *may stop* for a GPU fan and for a chipset or laptop fan its
    /// firmware stops, and its top speed as its maximum.
    fn fan(&mut self, m: &MeasuredControl, kind: ControlKind, label: &str, role: Role, measured: &Measured) -> Fan {
        let table = monotonic(&m.calibration);
        let mut cfg = self.config.control(&m.key).cloned().unwrap_or_else(|| ControlConfig::defaults(m.key.clone()));
        cfg.may_stop = role == Role::Gpu || (matches!(role, Role::Chipset | Role::Laptop) && m.firmware_stops);
        let fastest = table.last().map_or(0.0, |p| p[1]);
        let top = table.iter().find(|p| p[1] >= fastest * TOP).map_or(100.0, |p| p[0]);
        cfg.max = top;
        let (plan, _) = control_plan(&cfg, Some((kind, label)), measured);
        match self.config.controls.iter_mut().find(|c| c.key == m.key) {
            Some(c) => *c = cfg,
            None => self.config.controls.push(cfg),
        }
        Fan { key: m.key.clone(), label: label.to_owned(), floor: plan.floor, top, may_stop: plan.may_stop, table }
    }

    fn left(&mut self, fans: &[Fan], why: &str) {
        for f in fans {
            self.notes.push(format!("{}: {why}", f.label));
        }
    }

    fn put(&mut self, id: String, label: String, kind: CurveKind, response: Option<f64>, hysteresis: Option<f64>) -> String {
        let curve =
            Curve { id: id.clone(), label: Some(label), kind, response, hysteresis, hysteresis_direction: HysteresisDirection::Falling };
        match self.config.curves.iter_mut().find(|c| c.id == id) {
            Some(c) => *c = curve,
            None => self.config.curves.push(curve),
        }
        if !self.curves.contains(&id) {
            self.curves.push(id.clone());
        }
        id
    }

    fn assign(&mut self, fans: &[Fan], profile: usize, curve: &str) {
        let p = self.config.profiles.entry(PROFILES[profile].to_owned()).or_default();
        for f in fans {
            p.insert(f.key.clone(), curve.to_owned());
        }
    }

    /// The temperature at which a CPU curve reaches 100 % in each profile, which case fans follow too.
    fn cpu_full(c: &Src, p: usize) -> f64 {
        c.a.limit.unwrap_or(DEFAULT_EMERGENCY) - CPU[p][2]
    }

    fn gpu_full(g: &Src, p: usize) -> f64 {
        let (target, limit) = (g.a.target.unwrap_or(83.0), g.a.limit.unwrap_or(93.0));
        [limit - 3.0, limit - 5.0, target][p]
    }

    fn cpu(&mut self, fans: &[Fan], c: &Src) {
        let limit = c.a.limit.unwrap_or(DEFAULT_EMERGENCY);
        for p in 0..3 {
            let [from, half, _] = CPU[p];
            let at = legs(c.a.idle + from, limit - half, Self::cpu_full(c, p));
            let kind = CurveKind::Graph { source: c.key.clone(), points: points(fans, &at) };
            let id = self.put(format!("cpu-{}", PROFILES[p]), format!("CPU · {}", title(p)), kind, Some(5.0), Some(3.0));
            self.assign(fans, p, &id);
        }
    }

    fn gpu(&mut self, fans: &[Fan], g: &Src, i: usize, n: usize) {
        let target = g.a.target.unwrap_or(83.0);
        let (id_part, name) = if n > 1 { (format!("gpu-{}", i + 1), format!("GPU {}", i + 1)) } else { ("gpu".into(), "GPU".into()) };
        for p in 0..3 {
            let [below, half] = GPU[p];
            let t0 = target - below;
            let at = legs(t0, target - half, Self::gpu_full(g, p));
            // Below its floor a fan that may stop goes back to the driver's curve and its zero-RPM mode (§4.5).
            let mut pts = vec![[t0.round() - 1.0, 0.0]];
            pts.extend(points(fans, &at));
            let kind = CurveKind::Graph { source: g.key.clone(), points: pts };
            let id = self.put(format!("{id_part}-{}", PROFILES[p]), format!("{name} · {}", title(p)), kind, Some(2.0), Some(2.0));
            self.assign(fans, p, &id);
        }
    }

    /// Case fans follow the larger of a CPU curve and a curve per GPU: the floor until either source is 15 °C above idle, then
    /// up to the profile's cap where that source's own curve reaches 100 %.
    fn case(&mut self, fans: &[Fan], cpu: Option<&Src>, gpus: &[Src]) {
        for p in 0..3 {
            let cap = CASE_CAP[p];
            let mut parts = Vec::new();
            if let Some(c) = cpu {
                let pts = points(fans, &ramp(c.a.idle + CASE_START, Self::cpu_full(c, p), cap));
                parts.push((format!("case-{}-cpu", PROFILES[p]), "CPU".to_owned(), c.key.clone(), pts, 5.0, 3.0));
            }
            for (i, g) in gpus.iter().enumerate() {
                let pts = points(fans, &ramp(g.a.idle + CASE_START, Self::gpu_full(g, p), cap));
                let (id, name) =
                    if gpus.len() > 1 { (format!("gpu-{}", i + 1), format!("GPU {}", i + 1)) } else { ("gpu".into(), "GPU".into()) };
                parts.push((format!("case-{}-{id}", PROFILES[p]), name, g.key.clone(), pts, 2.0, 2.0));
            }
            let id = format!("case-{}", PROFILES[p]);
            let label = format!("Case · {}", title(p));
            if let [(_, _, source, pts, response, hysteresis)] = parts.as_slice() {
                let kind = CurveKind::Graph { source: source.clone(), points: pts.clone() };
                self.put(id.clone(), label, kind, Some(*response), Some(*hysteresis));
            } else {
                let mut ids = Vec::new();
                for (pid, name, source, pts, response, hysteresis) in parts {
                    let kind = CurveKind::Graph { source, points: pts };
                    ids.push(self.put(pid, format!("{label} · {name}"), kind, Some(response), Some(hysteresis)));
                }
                self.put(id.clone(), label, CurveKind::Mix { op: MixOp::Max, curves: ids }, None, None);
            }
            self.assign(fans, p, &id);
        }
    }

    /// A laptop's fans follow the larger of a CPU curve and a curve per GPU, each placed as that source's own curve would be,
    /// and the loop rounds the duty up to the fan's levels (§5.5). Fans its firmware stops may stop: each part goes to 0 % 1 °C
    /// below its floor, as the embedded controller stops them when cool.
    fn laptop(&mut self, fans: &[Fan], cpu: Option<&Src>, gpus: &[Src]) {
        let rest = fans.iter().all(|f| f.may_stop);
        for p in 0..3 {
            let mut parts = Vec::new();
            if let Some(c) = cpu {
                let [from, half, _] = CPU[p];
                let at = legs(c.a.idle + from, c.a.limit.unwrap_or(DEFAULT_EMERGENCY) - half, Self::cpu_full(c, p));
                parts.push((format!("laptop-{}-cpu", PROFILES[p]), "CPU".to_owned(), c.key.clone(), resting(fans, &at, rest), 5.0, 3.0));
            }
            for (i, g) in gpus.iter().enumerate() {
                let target = g.a.target.unwrap_or(83.0);
                let [below, half] = GPU[p];
                let at = legs(target - below, target - half, Self::gpu_full(g, p));
                let (id, name) =
                    if gpus.len() > 1 { (format!("gpu-{}", i + 1), format!("GPU {}", i + 1)) } else { ("gpu".into(), "GPU".into()) };
                parts.push((format!("laptop-{}-{id}", PROFILES[p]), name, g.key.clone(), resting(fans, &at, rest), 2.0, 2.0));
            }
            let id = format!("laptop-{}", PROFILES[p]);
            let label = format!("Laptop · {}", title(p));
            if let [(_, _, source, pts, response, hysteresis)] = parts.as_slice() {
                let kind = CurveKind::Graph { source: source.clone(), points: pts.clone() };
                self.put(id.clone(), label, kind, Some(*response), Some(*hysteresis));
            } else {
                let mut ids = Vec::new();
                for (pid, name, source, pts, response, hysteresis) in parts {
                    let kind = CurveKind::Graph { source, points: pts };
                    ids.push(self.put(pid, format!("{label} · {name}"), kind, Some(response), Some(hysteresis)));
                }
                self.put(id.clone(), label, CurveKind::Mix { op: MixOp::Max, curves: ids }, None, None);
            }
            self.assign(fans, p, &id);
        }
    }

    fn pump(&mut self, fans: &[Fan]) {
        for p in 0..3 {
            let id =
                self.put(format!("pump-{}", PROFILES[p]), format!("Pump · {}", title(p)), CurveKind::Flat { percent: PUMP[p] }, None, None);
            self.assign(fans, p, &id);
        }
    }

    /// One curve for every profile: a chipset's temperature doesn't change with what the machine is doing.
    fn chipset(&mut self, fans: &[Fan], s: &Src) {
        let [rest, from, full] = CHIPSET;
        let mut pts = points(fans, &ramp(from, full, 1.0));
        if fans.iter().all(|f| self.config.control(&f.key).is_some_and(|c| c.may_stop)) {
            pts.insert(0, [rest, 0.0]);
        }
        let kind = CurveKind::Graph { source: s.key.clone(), points: pts };
        let id = self.put("chipset".into(), "Chipset".into(), kind, None, Some(3.0));
        for p in 0..3 {
            self.assign(fans, p, &id);
        }
    }
}

fn title(p: usize) -> &'static str {
    ["Quiet", "Balanced", "Render"][p]
}

fn control(inventory: &Inventory, key: &Key) -> Option<(ControlKind, String)> {
    inventory.devices.iter().flat_map(|d| &d.controls).find(|c| &c.key == key).map(|c| (c.kind, c.label.clone()))
}

fn sensor<'a>(inventory: &'a Inventory, key: &Key) -> Option<&'a SensorInfo> {
    inventory.devices.iter().flat_map(|d| &d.sensors).find(|s| &s.key == key)
}

/// The chipset's temperature, by the name the board database gives it.
fn chipset_sensor(inventory: &Inventory) -> Option<&SensorInfo> {
    inventory
        .devices
        .iter()
        .filter(|d| d.source == Source::Hwmon)
        .flat_map(|d| &d.sensors)
        .find(|s| !s.below_ambient && ["pch", "chipset"].contains(&s.label.to_ascii_lowercase().as_str()))
}

/// The load check's correction (§5.5): curve `id`'s 100 % point `by` °C earlier, never at or before the point below it.
pub fn earlier(config: &mut Config, id: &str, by: f64) {
    let Some(Curve { kind: CurveKind::Graph { points, .. }, .. }) = config.curves.iter_mut().find(|c| c.id == id) else { return };
    let n = points.len();
    if n >= 2 {
        points[n - 1][0] = (points[n - 1][0] - by).max(points[n - 2][0] + 1.0);
    } else if let Some(p) = points.first_mut() {
        p[0] -= by;
    }
}
