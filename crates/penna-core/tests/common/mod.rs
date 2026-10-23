//! A fake desk for the control loop: one CPU sensor, one motherboard header, one NVIDIA fan, and hardware that does what it is
//! told (or not, when a test says so). Every command `tick` issues is carried out and recorded, as the daemon would.
#![allow(dead_code)]
use penna_core::config::{Config, Measured, MeasuredControl};
use penna_core::control::{Command, LoopState, Outcome, State, tick};
use penna_core::plan::Plan;
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Inventory, Key, Limit, LimitKind, Readings, Sample, SensorInfo, Source,
};
use std::collections::BTreeMap;

pub fn cpu() -> Key {
    Key::parse("hwmon/k10temp/0000:00:18.3/temp1").unwrap()
}
pub fn gpu_temp() -> Key {
    Key::parse("nvml/GPU-1/temp").unwrap()
}
pub fn pwm() -> Key {
    Key::parse("hwmon/it87/it87.2608/pwm1").unwrap()
}
pub fn gpu_fan() -> Key {
    Key::parse("nvml/GPU-1/fan0").unwrap()
}

pub fn inventory() -> Inventory {
    let sensor = |key: Key, label: &str, limits: Vec<Limit>| SensorInfo {
        key,
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
                sensors: vec![sensor(cpu(), "Tctl", vec![])],
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
                sensors: vec![],
                tachs: vec![],
                controls: vec![ControlInfo { key: pwm(), label: "pwm1".into(), kind: ControlKind::Duty { scale: 255 } }],
            },
            Device {
                id: "nvml/GPU-1".into(),
                source: Source::Nvml,
                driver: "nvml".into(),
                label: "GPU".into(),
                location: "0000:09:00.0".into(),
                model: None,
                sensors: vec![sensor(gpu_temp(), "GPU", vec![Limit { kind: LimitKind::GpuMax, celsius: 93.0 }])],
                tachs: vec![],
                controls: vec![ControlInfo { key: gpu_fan(), label: "Fan 1".into(), kind: ControlKind::Nvml { min: 30, max: 100 } }],
            },
        ],
        ..Inventory::default()
    }
}

pub const CONFIG: &str = r#"
version = 1
profile = "balanced"

[[sensor]]
key = "virt/avg"
time_average = { source = "hwmon/k10temp/0000:00:18.3/temp1", seconds = 10 }

[[curve]]
id = "cpu"
graph = { source = "hwmon/k10temp/0000:00:18.3/temp1", points = [[45, 30], [65, 40], [78, 65], [88, 100]] }
response = "5s"
hysteresis = 3.0

[[curve]]
id = "gpu"
graph = { source = "nvml/GPU-1/temp", points = [[45, 0], [55, 35], [70, 60], [83, 100]] }
response = 0
hysteresis = 2.0

[[curve]]
id = "case"
mix = { op = "max", curves = ["cpu", "gpu"] }

[[curve]]
id = "boost"
trigger = { source = "virt/avg", idle = 35, load = 70, on = 60, off = 50 }

[[curve]]
id = "quiet"
flat = { percent = 35 }

[[control]]
key = "hwmon/it87/it87.2608/pwm1"
label = "CPU cooler"
min = 25

[[control]]
key = "nvml/GPU-1/fan0"
label = "GPU fan 1"
may_stop = true

[profiles.balanced]
"hwmon/it87/it87.2608/pwm1" = "cpu"
"nvml/GPU-1/fan0" = "gpu"

[profiles.firmware]
"hwmon/it87/it87.2608/pwm1" = "firmware"
"nvml/GPU-1/fan0" = "firmware"

[profiles.boost]
"hwmon/it87/it87.2608/pwm1" = "boost"
"nvml/GPU-1/fan0" = "case"
"#;

/// The same desk with its motherboard header swapped for a stepped control: a ThinkPad's 8 levels, written every tick when
/// `refresh`, as its watchdog needs (§3.5).
pub fn stepped(refresh: bool, measured: &Measured) -> Rig {
    let kind = ControlKind::Levels { steps: 8, refresh };
    let mut inv = inventory();
    for c in inv.devices.iter_mut().flat_map(|d| &mut d.controls).filter(|c| c.key == pwm()) {
        c.kind = kind;
    }
    let cfg = Config::parse(CONFIG).expect("test config parses");
    let plan = Plan::new(cfg, &inv, measured, Some("AMD Ryzen 9 5950X 16-Core Processor")).expect("test config plans");
    let mut rig = Rig::new(plan);
    let h = rig.hw.get_mut(&pwm()).expect("the header");
    h.kind = kind;
    h.raw = 3;
    rig
}

pub fn measured(stop: f64, start: f64) -> Measured {
    Measured {
        controls: vec![MeasuredControl { stop: Some(stop), start: Some(start), ..MeasuredControl::new(pwm()) }],
        ..Measured::default()
    }
}

pub fn plan(config: &str, measured: &Measured) -> Plan {
    let cfg = Config::parse(config).expect("test config parses");
    Plan::new(cfg, &inventory(), measured, Some("AMD Ryzen 9 5950X 16-Core Processor")).expect("test config plans")
}

#[derive(Clone, Debug)]
pub struct Hw {
    pub kind: ControlKind,
    pub mode: ControlMode,
    pub original: ControlMode,
    pub raw: u32,
    pub fail_writes: bool,
    pub unreadable: bool,
}

pub struct Rig {
    pub plan: Plan,
    pub st: LoopState,
    pub now: f64,
    pub temps: BTreeMap<Key, Sample<f64>>,
    pub hw: BTreeMap<Key, Hw>,
    /// Every command issued, with the tick it came on.
    pub log: Vec<(u64, Command)>,
    pub last: Vec<Command>,
}

impl Rig {
    pub fn new(plan: Plan) -> Rig {
        let st = LoopState::new(&plan);
        let temps = BTreeMap::from([(cpu(), Sample::Value(45.0)), (gpu_temp(), Sample::Value(38.0))]);
        let hw = BTreeMap::from([
            (
                pwm(),
                Hw {
                    kind: ControlKind::Duty { scale: 255 },
                    mode: ControlMode::Automatic(2),
                    original: ControlMode::Automatic(2),
                    raw: 102,
                    fail_writes: false,
                    unreadable: false,
                },
            ),
            (
                gpu_fan(),
                Hw {
                    kind: ControlKind::Nvml { min: 30, max: 100 },
                    mode: ControlMode::Driver,
                    original: ControlMode::Driver,
                    raw: 0,
                    fail_writes: false,
                    unreadable: false,
                },
            ),
        ]);
        Rig { plan, st, now: 0.0, temps, hw, log: vec![], last: vec![] }
    }

    pub fn standard() -> Rig {
        Rig::new(plan(CONFIG, &measured(20.0, 30.0)))
    }

    pub fn set(&mut self, key: &Key, celsius: f64) {
        self.temps.insert(key.clone(), Sample::Value(celsius));
    }

    pub fn readings(&self) -> Readings {
        let mut r = Readings { temps: self.temps.clone(), ..Readings::default() };
        for (k, h) in &self.hw {
            let sample = if h.unreadable {
                Sample::Error("Input/output error".into())
            } else {
                let percent = match h.kind {
                    ControlKind::Duty { scale } => h.raw as f64 * 100.0 / scale as f64,
                    ControlKind::Levels { steps, .. } => penna_core::model::level_duty(steps, h.raw),
                    _ => h.raw as f64,
                };
                let raw = matches!(h.kind, ControlKind::Duty { .. } | ControlKind::Levels { .. }).then_some(h.raw);
                Sample::Value(ControlReading { percent, raw, mode: h.mode })
            };
            r.controls.insert(k.clone(), sample);
        }
        r
    }

    /// One tick at `now`, then one second passes.
    pub fn step(&mut self) -> Vec<Command> {
        let (st, cmds) = tick(&self.plan, &self.st, &self.readings(), self.now);
        self.st = st;
        let mut queue: std::collections::VecDeque<Command> = cmds.iter().cloned().collect();
        while let Some(cmd) = queue.pop_front() {
            let outcome = self.carry_out(&cmd);
            self.log.push((self.st.seq, cmd.clone()));
            if let Some(next) = self.st.record(&self.plan, &cmd, &outcome) {
                queue.push_back(next);
            }
        }
        self.now += 1.0;
        self.last = cmds.clone();
        cmds
    }

    pub fn run(&mut self, ticks: usize) {
        for _ in 0..ticks {
            self.step();
        }
    }

    fn carry_out(&mut self, cmd: &Command) -> Outcome {
        let h = self.hw.get_mut(cmd.key()).expect("commands name known controls");
        match cmd {
            Command::Take { .. } => match h.kind {
                ControlKind::Duty { scale } => {
                    h.mode = ControlMode::Manual;
                    Outcome::Taken { duty: h.raw as f64 * 100.0 / scale as f64 }
                }
                ControlKind::Levels { steps, .. } => {
                    h.mode = ControlMode::Manual;
                    Outcome::Taken { duty: penna_core::model::level_duty(steps, h.raw) }
                }
                _ => Outcome::Taken { duty: h.raw as f64 },
            },
            Command::Write { raw, .. } | Command::Reassert { raw, .. } => {
                if h.fail_writes {
                    return Outcome::Failed("Input/output error".into());
                }
                if let ControlKind::Nvml { min, max } = h.kind
                    && !(min..=max).contains(raw)
                {
                    return Outcome::Failed(format!("invalid argument: {raw} % is outside {min}–{max} %"));
                }
                h.mode = ControlMode::Manual;
                h.raw = *raw;
                Outcome::Done
            }
            Command::Handback { .. } => {
                h.mode = ControlMode::Driver;
                Outcome::Done
            }
            Command::Release { .. } => {
                if h.unreadable {
                    return Outcome::Failed("GPU is lost".into());
                }
                h.mode = h.original;
                Outcome::Done
            }
        }
    }

    pub fn state(&self, key: &Key) -> State {
        self.st.controls.get(key).map(|c| c.state).unwrap_or_default()
    }
    pub fn duty(&self, key: &Key) -> f64 {
        self.st.controls[key].duty
    }
}
