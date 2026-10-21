//! The simulator (DESIGN.md §9.1). A scenario builds a fake sysfs tree, with it87-like controls and tachs, k10temp and NVMe sensors,
//! and a file-backed NVML GPU, then updates it in place as time passes. Fans follow their duty through a stop and start duty and a
//! first-order lag. Heat follows `C·dT/dt = P − (k_passive + k_air·airflow)·(T − T_ambient)` under a load script. Faults happen on
//! a timeline. The real hwmon backend reads and writes the tree unmodified.
//!
//! The simulator runs in lockstep with whoever drives it ([`Sim::advance`]), so a run is the same every time.
use crate::hwmon::device_component;
use crate::mock_nvml::{DRIVER, MANUAL};
use anyhow::{Context, Result, anyhow, bail};
use penna_core::Key;
use penna_core::config::{Measured, MeasuredControl};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

const STEP: f64 = 0.1;

/// The scenarios that ship with Penna: `(scenario, configuration)`.
pub fn builtin(name: &str) -> Option<(&'static str, &'static str)> {
    const DESK: &str = include_str!("../../../test/fixtures/scenarios/mock-desk.config.toml");
    match name {
        "render-burst" => Some((include_str!("../../../test/fixtures/scenarios/render-burst.toml"), DESK)),
        "faults" => Some((include_str!("../../../test/fixtures/scenarios/faults.toml"), DESK)),
        _ => None,
    }
}

pub const BUILTIN: &[&str] = &["render-burst", "faults"];

/// A built-in scenario by name, or a scenario file and the configuration it names beside it.
pub fn load(name_or_path: &str) -> Result<(Scenario, String)> {
    if let Some((scenario, config)) = builtin(name_or_path) {
        return Ok((Scenario::parse(scenario)?, config.to_owned()));
    }
    let path = Path::new(name_or_path);
    if !path.exists() {
        bail!("no scenario {name_or_path:?}: built in are {}, or give a path to a scenario file", BUILTIN.join(", "));
    }
    let scenario = Scenario::parse(&fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)?;
    let config_path = path.parent().unwrap_or(Path::new(".")).join(&scenario.config);
    let config = fs::read_to_string(&config_path).with_context(|| format!("reading {}", config_path.display()))?;
    Ok((scenario, config))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub cpu_model: String,
    pub ambient: f64,
    #[serde(default)]
    pub seed: u64,
    /// The configuration file, beside the scenario, that drives this desk.
    pub config: String,
    /// How long the scenario runs before everything has been shown.
    pub seconds: f64,
    pub load: LoadScript,
    #[serde(rename = "body")]
    pub bodies: Vec<Body>,
    #[serde(rename = "chip")]
    pub chips: Vec<Chip>,
    #[serde(default, rename = "gpu")]
    pub gpus: Vec<GpuSpec>,
    #[serde(default, rename = "fault")]
    pub faults: Vec<Fault>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadScript {
    /// The script repeats every this many seconds.
    pub repeat: Option<f64>,
    pub points: Vec<LoadPoint>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadPoint {
    pub at: f64,
    #[serde(default)]
    pub cpu: f64,
    #[serde(default)]
    pub gpu: f64,
}

/// A thermal mass.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub id: String,
    /// `cpu` or `gpu`: which load channel heats it.
    pub load: String,
    /// J/°C
    pub capacity: f64,
    pub idle_watts: f64,
    pub load_watts: f64,
    /// W/°C with no airflow
    pub passive: f64,
    /// W/°C added at full airflow
    pub airflow: f64,
    #[serde(default, rename = "fan")]
    pub fans: Vec<BodyFan>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyFan {
    pub id: String,
    pub weight: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chip {
    pub driver: String,
    /// The device under `devices/`, e.g. `devices/platform/it87.2608`.
    pub path: String,
    /// The `hwmonN` it appears as.
    pub hwmon: u32,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, rename = "temp")]
    pub temps: Vec<TempSpec>,
    #[serde(default, rename = "pwm")]
    pub pwms: Vec<PwmSpec>,
    #[serde(default, rename = "fan")]
    pub fans: Vec<FanSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempSpec {
    pub channel: u32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub noise: f64,
    /// Zen's boost: occasional jumps of up to this many degrees that fade within seconds.
    #[serde(default)]
    pub spikes: f64,
    /// A constant reading, such as the 127 °C of an unconnected input.
    #[serde(default)]
    pub fixed: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub crit: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PwmSpec {
    pub channel: u32,
    /// The firmware's `pwmN_enable`: what Penna must restore.
    pub enable: i64,
    /// What the firmware does with it in automatic mode: a curve on a body's temperature.
    pub firmware: FirmwareCurve,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareCurve {
    pub body: String,
    pub points: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FanSpec {
    pub channel: u32,
    pub id: String,
    /// The `pwmN` that drives it: what pairing would find.
    pub pwm: u32,
    pub max_rpm: f64,
    pub stop: f64,
    pub start: f64,
    #[serde(default = "two")]
    pub tau: f64,
}

fn two() -> f64 {
    2.0
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuSpec {
    /// Its fans are `<id>-fan0`, `<id>-fan1`… for the bodies they cool.
    pub id: String,
    pub uuid: String,
    pub name: String,
    pub body: String,
    pub fans: u32,
    pub min: u32,
    pub max: u32,
    pub max_rpm: f64,
    /// NVML reports percent of this speed, so the percent is not duty and can pass 100.
    pub noise_rpm: f64,
    pub driver_on: f64,
    pub driver_off: f64,
    pub driver_percent: f64,
    pub gpu_max: f64,
    pub slowdown: f64,
    pub shutdown: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub at: f64,
    /// Seconds; a one-shot fault (`resume-reset`) has none.
    #[serde(default, rename = "for")]
    pub duration: f64,
    pub kind: FaultKind,
    /// `k10temp/temp1`, `it87/pwm2`, `it87/fan5`, a body id for `heat`, a GPU id for `gpu-lost`.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub watts: Option<f64>,
    #[serde(default)]
    pub value: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultKind {
    /// The sensor's file disappears.
    SensorGone,
    /// The sensor reads something that is not a number.
    Garbage,
    /// Reading the sensor fails (permission denied).
    Unreadable,
    /// The firmware keeps setting `pwmN_enable` (default 2): it takes the control back.
    PwmEnable,
    /// The tach reads 0 whatever the duty: a dead fan.
    TachStuck,
    /// What a resume does: every control back to its firmware mode and duty.
    ResumeReset,
    /// The GPU falls off the bus.
    GpuLost,
    /// Extra heat into a body.
    Heat,
}

impl FaultKind {
    fn name(self) -> &'static str {
        match self {
            FaultKind::SensorGone => "sensor-gone",
            FaultKind::Garbage => "garbage",
            FaultKind::Unreadable => "unreadable",
            FaultKind::PwmEnable => "pwm-enable",
            FaultKind::TachStuck => "tach-stuck",
            FaultKind::ResumeReset => "resume-reset",
            FaultKind::GpuLost => "gpu-lost",
            FaultKind::Heat => "heat",
        }
    }
}

impl Scenario {
    pub fn parse(text: &str) -> Result<Scenario> {
        let s: Scenario = toml::from_str(text).map_err(|e| anyhow!("scenario: {e}"))?;
        for f in &s.faults {
            if f.kind != FaultKind::ResumeReset && f.duration <= 0.0 {
                bail!("scenario {}: the {} fault at {} s needs a duration", s.name, f.kind.name(), f.at);
            }
        }
        Ok(s)
    }

    fn load_at(&self, t: f64) -> LoadPoint {
        let pts = &self.load.points;
        let t = match self.load.repeat {
            Some(r) if r > 0.0 => t.rem_euclid(r),
            _ => t,
        };
        let Some(first) = pts.first() else { return LoadPoint { at: 0.0, cpu: 0.0, gpu: 0.0 } };
        if t <= first.at {
            return *first;
        }
        for w in pts.windows(2) {
            if t <= w[1].at {
                let k = (t - w[0].at) / (w[1].at - w[0].at).max(1e-9);
                return LoadPoint { at: t, cpu: w[0].cpu + (w[1].cpu - w[0].cpu) * k, gpu: w[0].gpu + (w[1].gpu - w[0].gpu) * k };
            }
        }
        *pts.last().expect("not empty")
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FanState {
    rpm: f64,
    spinning: bool,
}

pub struct Sim {
    sc: Scenario,
    root: PathBuf,
    t: f64,
    rng: u64,
    temps: BTreeMap<String, f64>,
    fans: BTreeMap<String, FanState>,
    gpu_on: Vec<bool>,
    boosts: BTreeMap<(usize, u32), f64>,
    active: BTreeSet<usize>,
    fired: BTreeSet<usize>,
}

impl Sim {
    /// Builds the tree under `root`, which must be empty or absent.
    pub fn create(sc: Scenario, root: &Path) -> Result<Sim> {
        if root.exists() && fs::read_dir(root)?.next().is_some() {
            bail!("{} is not empty", root.display());
        }
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        let mut temps = BTreeMap::new();
        for b in &sc.bodies {
            // Start near the idle equilibrium with the fans at about a third.
            temps.insert(b.id.clone(), sc.ambient + b.idle_watts / (b.passive + b.airflow * 0.35));
        }
        let mut sim = Sim {
            rng: sc.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            gpu_on: vec![false; sc.gpus.len()],
            sc,
            root,
            t: 0.0,
            temps,
            fans: BTreeMap::new(),
            boosts: BTreeMap::new(),
            active: BTreeSet::new(),
            fired: BTreeSet::new(),
        };
        sim.build()?;
        Ok(sim)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn mock_nvml_root(&self) -> PathBuf {
        self.root.join("penna-mock/nvml")
    }
    pub fn scenario(&self) -> &Scenario {
        &self.sc
    }
    pub fn now(&self) -> f64 {
        self.t
    }

    fn chip_dir(&self, chip: &Chip) -> PathBuf {
        self.root.join(&chip.path).join("hwmon").join(format!("hwmon{}", chip.hwmon))
    }
    fn gpu_dir(&self, g: &GpuSpec) -> PathBuf {
        self.mock_nvml_root().join(&g.uuid)
    }

    fn build(&mut self) -> Result<()> {
        let class = self.root.join("class/hwmon");
        fs::create_dir_all(&class)?;
        for chip in self.sc.chips.clone() {
            let dir = self.chip_dir(&chip);
            fs::create_dir_all(&dir)?;
            put(&dir.join("name"), &chip.driver)?;
            symlink("../..", dir.join("device"))?;
            symlink(format!("../../{}/hwmon/hwmon{}", chip.path, chip.hwmon), class.join(format!("hwmon{}", chip.hwmon)))?;
            if let Some(m) = &chip.model {
                put(&self.root.join(&chip.path).join("model"), m)?;
            }
            for t in &chip.temps {
                if let Some(l) = &t.label {
                    put(&dir.join(format!("temp{}_label", t.channel)), l)?;
                }
                for (attr, v) in [("max", t.max), ("crit", t.crit)] {
                    if let Some(v) = v {
                        put(&dir.join(format!("temp{}_{attr}", t.channel)), &((v * 1000.0).round() as i64).to_string())?;
                    }
                }
            }
            for p in &chip.pwms {
                put(&dir.join(format!("pwm{}_enable", p.channel)), &p.enable.to_string())?;
                let duty = self.firmware_duty(&p.firmware);
                put(&dir.join(format!("pwm{}", p.channel)), &raw(duty).to_string())?;
            }
            for f in &chip.fans {
                put(&dir.join(format!("fan{}_input", f.channel)), "0")?;
                self.fans.insert(f.id.clone(), FanState::default());
            }
        }
        for g in self.sc.gpus.clone() {
            let dir = self.gpu_dir(&g);
            fs::create_dir_all(&dir)?;
            put(&dir.join("name"), &g.name)?;
            for (f, v) in [
                ("fans", g.fans as f64),
                ("min", g.min as f64),
                ("max", g.max as f64),
                ("gpu_max", g.gpu_max),
                ("slowdown", g.slowdown),
                ("shutdown", g.shutdown),
            ] {
                put(&dir.join(f), &v.to_string())?;
            }
            for i in 0..g.fans {
                put(&dir.join(format!("fan{i}_policy")), DRIVER)?;
                put(&dir.join(format!("fan{i}_target")), &g.min.to_string())?;
                put(&dir.join(format!("fan{i}_percent")), "0")?;
                put(&dir.join(format!("fan{i}_rpm")), "0")?;
                self.fans.insert(format!("{}-fan{i}", g.id), FanState::default());
            }
        }
        self.write_sensors(0.0)?;
        Ok(())
    }

    /// What pairing and calibration would have found: the tachs each PWM drives, their stop and start duties, and the duty → RPM
    /// table (§5.2) from the simulator's own fan model, so the editor's RPM axis says what the fans here actually do.
    pub fn measured(&self) -> Measured {
        let mut controls = Vec::new();
        for chip in &self.sc.chips {
            let device = device_component(Path::new(&chip.path));
            for p in &chip.pwms {
                let fans: Vec<&FanSpec> = chip.fans.iter().filter(|f| f.pwm == p.channel).collect();
                if fans.is_empty() {
                    continue;
                }
                let stop = fans.iter().map(|f| f.stop).reduce(f64::max);
                let calibration = calibration_steps(stop.unwrap_or(0.0))
                    .into_iter()
                    .map(|d| {
                        let rpm = fans.iter().map(|f| f.max_rpm * (d / 100.0).powf(0.8)).sum::<f64>() / fans.len() as f64;
                        [d, rpm.round()]
                    })
                    .collect();
                controls.push(MeasuredControl {
                    key: Key::hwmon(&chip.driver, &device, &format!("pwm{}", p.channel)),
                    stop,
                    start: fans.iter().map(|f| f.start).reduce(f64::max),
                    tachs: fans.iter().map(|f| Key::hwmon(&chip.driver, &device, &format!("fan{}", f.channel))).collect(),
                    calibration,
                });
            }
        }
        // NVML fans need no pairing; calibration only records percent → RPM over the settable range, for the axis (§5.2).
        for g in &self.sc.gpus {
            for i in 0..g.fans {
                let calibration = calibration_steps(g.min as f64).into_iter().map(|d| [d, gpu_rpm(g, d).round()]).collect();
                controls.push(MeasuredControl { key: Key::nvml_fan(&g.uuid, i), stop: None, start: None, tachs: vec![], calibration });
            }
        }
        Measured { controls }
    }

    /// Moves time on by `dt` seconds. Returns what happened, in words: faults starting and ending.
    pub fn advance(&mut self, dt: f64) -> Result<Vec<String>> {
        let mut events = Vec::new();
        let end = self.t + dt;
        while self.t < end - 1e-9 {
            let h = STEP.min(end - self.t);
            self.faults(&mut events)?;
            self.step(h)?;
            self.t += h;
        }
        self.write_sensors(dt)?;
        Ok(events)
    }

    fn faults(&mut self, events: &mut Vec<String>) -> Result<()> {
        let t = self.t;
        for (i, f) in self.sc.faults.clone().iter().enumerate() {
            let what = format!("{} {}", f.kind.name(), f.target.as_deref().unwrap_or(""));
            if f.kind == FaultKind::ResumeReset {
                if t >= f.at && self.fired.insert(i) {
                    events.push(format!("fault: {} — every control back to its firmware mode", what.trim()));
                    self.resume_reset()?;
                }
                continue;
            }
            let on = t >= f.at && t < f.at + f.duration;
            if on && self.active.insert(i) {
                events.push(format!("fault: {} for {:.0} s", what.trim(), f.duration));
            } else if !on && self.active.remove(&i) {
                events.push(format!("fault over: {}", what.trim()));
                self.end_fault(f)?;
            }
        }
        Ok(())
    }

    fn active(&self, kind: FaultKind) -> impl Iterator<Item = &Fault> {
        self.active.iter().map(|i| &self.sc.faults[*i]).filter(move |f| f.kind == kind)
    }

    fn targeted(&self, kind: FaultKind, target: &str) -> Option<&Fault> {
        self.active(kind).find(|f| f.target.as_deref() == Some(target))
    }

    fn resume_reset(&mut self) -> Result<()> {
        for chip in self.sc.chips.clone() {
            let dir = self.chip_dir(&chip);
            for p in &chip.pwms {
                put(&dir.join(format!("pwm{}_enable", p.channel)), &p.enable.to_string())?;
                put(&dir.join(format!("pwm{}", p.channel)), &raw(self.firmware_duty(&p.firmware)).to_string())?;
            }
        }
        for g in self.sc.gpus.clone() {
            for i in 0..g.fans {
                put(&self.gpu_dir(&g).join(format!("fan{i}_policy")), DRIVER)?;
            }
        }
        Ok(())
    }

    fn end_fault(&mut self, f: &Fault) -> Result<()> {
        if let (FaultKind::Unreadable, Some(target)) = (f.kind, &f.target)
            && let Some(path) = self.sensor_path(target)
        {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        }
        if f.kind == FaultKind::GpuLost {
            for g in self.sc.gpus.clone() {
                let _ = fs::remove_file(self.gpu_dir(&g).join("lost"));
            }
        }
        Ok(())
    }

    fn sensor_path(&self, target: &str) -> Option<PathBuf> {
        let (driver, channel) = target.split_once('/')?;
        let chip = self.sc.chips.iter().find(|c| c.driver == driver)?;
        Some(self.chip_dir(chip).join(format!("{channel}_input")))
    }

    fn firmware_duty(&self, fw: &FirmwareCurve) -> f64 {
        let t = self.temps.get(&fw.body).copied().unwrap_or(self.sc.ambient);
        penna_core::control::graph(&fw.points, t).clamp(0.0, 100.0)
    }

    fn step(&mut self, h: f64) -> Result<()> {
        // Controls: what each PWM is really doing.
        let mut duty_of: BTreeMap<(usize, u32), f64> = BTreeMap::new();
        for (ci, chip) in self.sc.chips.clone().iter().enumerate() {
            let dir = self.chip_dir(chip);
            for p in &chip.pwms {
                let enable_path = dir.join(format!("pwm{}_enable", p.channel));
                let pwm_path = dir.join(format!("pwm{}", p.channel));
                if let Some(f) = self.targeted(FaultKind::PwmEnable, &format!("{}/pwm{}", chip.driver, p.channel)) {
                    put(&enable_path, &f.value.unwrap_or(2).to_string())?;
                }
                let enable: i64 = get(&enable_path).unwrap_or(p.enable);
                let duty = match enable {
                    0 => 100.0,
                    1 => get::<i64>(&pwm_path).map_or(100.0, |r| r.clamp(0, 255) as f64 * 100.0 / 255.0),
                    _ => {
                        let d = self.firmware_duty(&p.firmware);
                        put(&pwm_path, &raw(d).to_string())?;
                        d
                    }
                };
                duty_of.insert((ci, p.channel), duty);
            }
        }
        // Motherboard fans.
        for (ci, chip) in self.sc.chips.clone().iter().enumerate() {
            let dir = self.chip_dir(chip);
            for f in &chip.fans {
                let duty = duty_of.get(&(ci, f.pwm)).copied().unwrap_or(0.0);
                let stuck = self.targeted(FaultKind::TachStuck, &format!("{}/fan{}", chip.driver, f.channel)).is_some();
                let st = self.fans.entry(f.id.clone()).or_default();
                if st.spinning && duty < f.stop {
                    st.spinning = false;
                } else if !st.spinning && duty >= f.start {
                    st.spinning = true;
                }
                let target = if st.spinning { f.max_rpm * (duty / 100.0).powf(0.8) } else { 0.0 };
                st.rpm += (target - st.rpm) * (1.0 - (-h / f.tau).exp());
                let shown = if stuck { 0.0 } else { st.rpm };
                put(&dir.join(format!("fan{}_input", f.channel)), &(shown.round() as i64).to_string())?;
            }
        }
        // GPUs.
        for (gi, g) in self.sc.gpus.clone().iter().enumerate() {
            let dir = self.gpu_dir(g);
            let lost = self.targeted(FaultKind::GpuLost, &g.id).is_some();
            if lost {
                put(&dir.join("lost"), "1")?;
                continue;
            }
            let temp = self.temps.get(&g.body).copied().unwrap_or(self.sc.ambient);
            if temp > g.driver_on {
                self.gpu_on[gi] = true;
            } else if temp < g.driver_off {
                self.gpu_on[gi] = false;
            }
            for i in 0..g.fans {
                let policy = fs::read_to_string(dir.join(format!("fan{i}_policy"))).unwrap_or_default();
                let duty = if policy.trim() == MANUAL {
                    get::<f64>(&dir.join(format!("fan{i}_target"))).unwrap_or(g.min as f64)
                } else if self.gpu_on[gi] {
                    g.driver_percent
                } else {
                    0.0
                };
                let target = gpu_rpm(g, duty);
                let st = self.fans.entry(format!("{}-fan{i}", g.id)).or_default();
                st.spinning = target > 0.0;
                st.rpm += (target - st.rpm) * (1.0 - (-h / 2.0f64).exp());
                if target == 0.0 && st.rpm < 20.0 {
                    st.rpm = 0.0;
                }
                let rpm = st.rpm;
                put(&dir.join(format!("fan{i}_rpm")), &(rpm.round() as i64).to_string())?;
                put(&dir.join(format!("fan{i}_percent")), &((rpm / g.noise_rpm * 100.0).round() as i64).to_string())?;
            }
        }
        // Heat.
        let load = self.sc.load_at(self.t);
        for b in self.sc.bodies.clone() {
            let l = if b.load == "gpu" { load.gpu } else { load.cpu };
            let extra: f64 = self.active(FaultKind::Heat).filter(|f| f.target.as_deref() == Some(&b.id)).filter_map(|f| f.watts).sum();
            let p = b.idle_watts + (b.load_watts - b.idle_watts) * l + extra;
            let total: f64 = b.fans.iter().map(|f| f.weight).sum::<f64>().max(1e-9);
            let airflow: f64 = b.fans.iter().map(|f| f.weight * self.airflow(&f.id)).sum::<f64>() / total;
            let t = self.temps.get(&b.id).copied().unwrap_or(self.sc.ambient);
            let dt = (p - (b.passive + b.airflow * airflow) * (t - self.sc.ambient)) / b.capacity * h;
            self.temps.insert(b.id.clone(), t + dt);
        }
        Ok(())
    }

    fn airflow(&self, fan: &str) -> f64 {
        let st = self.fans.get(fan).copied().unwrap_or_default();
        let max = self
            .sc
            .chips
            .iter()
            .flat_map(|c| &c.fans)
            .find(|f| f.id == fan)
            .map(|f| f.max_rpm)
            .or_else(|| self.sc.gpus.iter().find(|g| fan.starts_with(&format!("{}-fan", g.id))).map(|g| g.max_rpm))
            .unwrap_or(1.0);
        (st.rpm / max).clamp(0.0, 1.0)
    }

    fn write_sensors(&mut self, dt: f64) -> Result<()> {
        for (ci, chip) in self.sc.chips.clone().iter().enumerate() {
            let dir = self.chip_dir(chip);
            for t in &chip.temps {
                let path = dir.join(format!("temp{}_input", t.channel));
                let target = format!("{}/temp{}", chip.driver, t.channel);
                if self.targeted(FaultKind::SensorGone, &target).is_some() {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                if self.targeted(FaultKind::Garbage, &target).is_some() {
                    put(&path, "garbage")?;
                    continue;
                }
                let unreadable = self.targeted(FaultKind::Unreadable, &target).is_some();
                let v = match (t.fixed, &t.body) {
                    (Some(v), _) => v,
                    (None, Some(b)) => {
                        let base = self.temps.get(b).copied().unwrap_or(self.sc.ambient) + t.offset;
                        let noise = t.noise * self.gauss();
                        let boost = self.boosts.entry((ci, t.channel)).or_insert(0.0);
                        if t.spikes > 0.0 && dt > 0.0 {
                            *boost *= 0.4f64.powf(dt);
                        }
                        let mut b = *boost;
                        if t.spikes > 0.0 && dt > 0.0 && self.uniform() < 0.25 * dt {
                            b += t.spikes * (0.4 + 0.6 * self.uniform());
                            self.boosts.insert((ci, t.channel), b);
                        }
                        base + noise + b
                    }
                    (None, None) => self.sc.ambient,
                };
                if unreadable {
                    let locked = fs::metadata(&path).is_ok_and(|m| m.permissions().mode() & 0o777 == 0);
                    if !locked {
                        put(&path, &((v * 1000.0).round() as i64).to_string())?;
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))?;
                    }
                    continue;
                }
                put(&path, &((v * 1000.0).round() as i64).to_string())?;
            }
        }
        for g in self.sc.gpus.clone() {
            let dir = self.gpu_dir(&g);
            if dir.join("lost").exists() {
                continue;
            }
            let t = self.temps.get(&g.body).copied().unwrap_or(self.sc.ambient) + 0.3 * self.gauss();
            put(&dir.join("temp"), &format!("{}", t.round()))?;
        }
        Ok(())
    }

    fn next(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
    fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let (u, v) = (self.uniform().max(1e-12), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

fn raw(duty: f64) -> i64 {
    (duty.clamp(0.0, 100.0) * 255.0 / 100.0).round() as i64
}

/// A mock GPU fan's RPM at an NVML percent: what the driver spins it to.
fn gpu_rpm(g: &GpuSpec, duty: f64) -> f64 {
    if duty <= 0.0 {
        0.0
    } else if duty < g.min as f64 {
        600.0 * duty / g.min as f64
    } else {
        600.0 + (duty - g.min as f64) / (100.0 - g.min as f64) * (g.max_rpm - 600.0)
    }
}

/// The duties calibration measures (§5.2): down from 100 % in 5 % steps to the lowest that still turns the fan, ascending.
fn calibration_steps(stop: f64) -> Vec<f64> {
    let mut d: Vec<f64> = (0..=20).map(|i| 100.0 - 5.0 * i as f64).filter(|d| *d >= stop && *d > 0.0).collect();
    if stop > 0.0 && d.last() != Some(&stop) {
        d.push(stop);
    }
    d.reverse();
    d
}

fn put(path: &Path, s: &str) -> Result<()> {
    fs::write(path, format!("{s}\n")).with_context(|| format!("writing {}", path.display()))
}

fn get<T: std::str::FromStr>(path: &Path) -> Option<T> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}
