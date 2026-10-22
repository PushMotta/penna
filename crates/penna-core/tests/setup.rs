//! Setup (DESIGN.md §5, §11.3) driving the real control loop against a modelled desk, on simulated time: the daemon's own
//! sequence of read, setup, tick, carry out, record. The desk is shaped on the one Penna was built on: a CPU fan, a case fan, a
//! chipset fan its BIOS stops when cool, a case fan on a pump header, an empty header, a header whose firmware takes it back, and
//! an NVIDIA fan.
use penna_core::config::Measured;
use penna_core::control::{Command, LoopState, Outcome, tick};
use penna_core::plan::Plan;
use penna_core::setup::{Guard, Input, LoadInput, Role, Setup, Step, TachHint, Target, WriteResult};
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Inventory, Key, Readings, Sample, SensorInfo, Source, TachInfo,
};
use std::collections::BTreeMap;

const CHIP: &str = "hwmon/it8688/it87.2624";

fn pwm(n: u32) -> Key {
    Key::parse(&format!("{CHIP}/pwm{n}")).unwrap()
}
fn fan(n: u32) -> Key {
    Key::parse(&format!("{CHIP}/fan{n}")).unwrap()
}
fn cpu() -> Key {
    Key::parse("hwmon/k10temp/0000:00:18.3/temp1").unwrap()
}
fn gpu_fan() -> Key {
    Key::parse("nvml/GPU-1/fan0").unwrap()
}
fn gpu_temp() -> Key {
    Key::parse("nvml/GPU-1/temp").unwrap()
}

/// One header: its PWM, the fan on it if any, and what the firmware does with it.
#[derive(Clone)]
struct Header {
    n: u32,
    name: &'static str,
    role: Option<&'static str>,
    fan: Option<FanModel>,
    /// The firmware's own duty while it runs the header.
    firmware: f64,
    /// The firmware writes the header back to automatic within a second of a manual write.
    reverts: bool,
}

#[derive(Clone, Copy)]
struct FanModel {
    max: f64,
    /// Turns down to this duty, and starts again at `start`.
    stop: f64,
    start: f64,
    /// RPM at the stop duty, and the curve's shape above it.
    min: f64,
    g: f64,
    /// Full speed at 0 %, as this desk's SYS_FAN1 runs.
    races_at_zero: bool,
    /// Raised, it overshoots by this many RPM per % and comes back over `surge_tau` seconds, as this desk's SYS_FAN4 does: 2,150
    /// RPM a few seconds after 100 %, 1,850 ten seconds later and still falling (22 Oct 2026).
    surge: f64,
    surge_tau: f64,
}

impl FanModel {
    fn rpm(&self, duty: f64, spinning: bool) -> f64 {
        if self.races_at_zero && duty <= 0.0 {
            self.max
        } else if duty < self.stop || (!spinning && duty < self.start) {
            0.0
        } else {
            self.min + (self.max - self.min) * ((duty - self.stop) / (100.0 - self.stop)).powf(self.g)
        }
    }
}

struct Channel {
    h: Header,
    mode: ControlMode,
    raw: u32,
    rpm: f64,
    /// What the tach reads: it87 reads the chip at most every 1.5 s, so on a 1 s tick a reading is a second or two old, and
    /// every other one repeats the last.
    shown: f64,
    spinning: bool,
    writes: u32,
    /// The duty it last ran at, and the overshoot still to come back from.
    duty: f64,
    boost: f64,
}

struct Desk {
    channels: Vec<Channel>,
    /// Seconds since the desk was built, and when it87 last read the chip.
    clock: f64,
    read_at: f64,
    gpu_manual: bool,
    gpu_percent: f64,
    gpu_rpm: f64,
    cpu_temp: f64,
}

fn headers() -> Vec<Header> {
    let f = |max, stop, start, min, g| Some(FanModel { max, stop, start, min, g, races_at_zero: false, surge: 0.0, surge_tau: 1.0 });
    vec![
        Header { n: 1, name: "CPU_FAN", role: Some("cpu"), fan: f(1660.0, 18.0, 24.0, 360.0, 0.95), firmware: 27.0, reverts: false },
        // Never stops: 830 RPM down to 5 %, and full speed at 0 % (setup on this desk, 22 Oct 2026).
        Header {
            n: 2,
            name: "SYS_FAN1",
            role: None,
            fan: Some(FanModel { races_at_zero: true, ..f(2190.0, 0.0, 0.0, 830.0, 1.2).unwrap() }),
            firmware: 27.0,
            reverts: false,
        },
        Header { n: 3, name: "PCH_FAN", role: Some("chipset"), fan: f(5620.0, 22.0, 28.0, 1500.0, 0.54), firmware: 16.0, reverts: false },
        Header {
            n: 4,
            name: "SYS_FAN5_PUMP",
            role: Some("pump-header"),
            fan: f(1730.0, 20.0, 26.0, 420.0, 0.93),
            firmware: 60.0,
            reverts: false,
        },
        Header { n: 5, name: "SYS_FAN6_PUMP", role: Some("pump-header"), fan: None, firmware: 60.0, reverts: false },
        Header { n: 6, name: "SYS_FAN3", role: None, fan: f(2140.0, 18.0, 24.0, 520.0, 0.69), firmware: 40.0, reverts: true },
        // A liquid cooler's pump on CPU_OPT: fast, and flat across most duties.
        Header { n: 7, name: "CPU_OPT", role: Some("cpu-opt"), fan: f(2600.0, 20.0, 26.0, 1600.0, 0.8), firmware: 70.0, reverts: false },
        Header {
            n: 8,
            name: "SYS_FAN4",
            role: None,
            fan: Some(FanModel { surge: 9.0, surge_tau: 8.0, ..f(1800.0, 20.0, 24.0, 700.0, 0.8).unwrap() }),
            firmware: 40.0,
            reverts: false,
        },
    ]
}

impl Desk {
    fn new() -> Desk {
        let channels = headers()
            .into_iter()
            .map(|h| {
                let rpm = h.fan.map_or(0.0, |f| f.rpm(h.firmware, true));
                let spinning = rpm > 0.0;
                Channel {
                    raw: raw(h.firmware),
                    mode: ControlMode::Automatic(2),
                    rpm,
                    shown: rpm,
                    spinning,
                    writes: 0,
                    duty: h.firmware,
                    boost: 0.0,
                    h,
                }
            })
            .collect();
        Desk { channels, clock: 0.0, read_at: -2.0, gpu_manual: false, gpu_percent: 0.0, gpu_rpm: 0.0, cpu_temp: 45.0 }
    }

    fn inventory(&self) -> Inventory {
        let sensor =
            |key: Key, label: &str| SensorInfo { key, label: label.into(), limits: vec![], ignored_limits: vec![], below_ambient: false };
        Inventory {
            devices: vec![
                Device {
                    id: "hwmon/k10temp/0000:00:18.3".into(),
                    source: Source::Hwmon,
                    driver: "k10temp".into(),
                    label: "AMD CPU".into(),
                    location: "0000:00:18.3".into(),
                    model: None,
                    sensors: vec![sensor(cpu(), "Tctl")],
                    tachs: vec![],
                    controls: vec![],
                },
                Device {
                    id: CHIP.into(),
                    source: Source::Hwmon,
                    driver: "it8688".into(),
                    label: "Motherboard".into(),
                    location: "it87.2624".into(),
                    model: Some("IT8688E".into()),
                    sensors: vec![],
                    tachs: self.channels.iter().map(|c| TachInfo { key: fan(c.h.n), label: c.h.name.into() }).collect(),
                    controls: self
                        .channels
                        .iter()
                        .map(|c| ControlInfo {
                            key: pwm(c.h.n),
                            label: format!("IT8688E pwm{}", c.h.n),
                            kind: ControlKind::Duty { scale: 255 },
                        })
                        .collect(),
                },
                Device {
                    id: "nvml/GPU-1".into(),
                    source: Source::Nvml,
                    driver: "nvml".into(),
                    label: "GPU".into(),
                    location: String::new(),
                    model: None,
                    sensors: vec![sensor(gpu_temp(), "GPU")],
                    tachs: vec![],
                    controls: vec![ControlInfo {
                        key: gpu_fan(),
                        label: "GPU fan 1".into(),
                        kind: ControlKind::Nvml { min: 30, max: 100 },
                    }],
                },
            ],
            ..Inventory::default()
        }
    }

    fn input(&self) -> Input {
        let inv = self.inventory();
        Input {
            targets: inv
                .devices
                .iter()
                .flat_map(|d| &d.controls)
                .map(|c| Target { key: c.key.clone(), label: c.label.clone(), kind: c.kind })
                .collect(),
            tachs: self
                .channels
                .iter()
                .map(|c| TachHint { key: fan(c.h.n), label: c.h.name.into(), role: c.h.role.map(str::to_owned) })
                .collect(),
            guards: vec![
                Guard { key: cpu(), label: "CPU · Tctl".into(), limit: 85.0 },
                Guard { key: gpu_temp(), label: "GPU".into(), limit: 80.0 },
            ],
            threads: 32,
            load_check: None,
        }
    }

    fn readings(&self) -> Readings {
        let mut r = Readings::default();
        r.temps.insert(cpu(), Sample::Value(self.cpu_temp));
        r.temps.insert(gpu_temp(), Sample::Value(40.0));
        for c in &self.channels {
            r.rpms.insert(fan(c.h.n), Sample::Value(c.shown.round() as u32));
            r.controls.insert(
                pwm(c.h.n),
                Sample::Value(ControlReading { percent: c.raw as f64 * 100.0 / 255.0, raw: Some(c.raw), mode: c.mode }),
            );
        }
        r.rpms.insert(gpu_fan(), Sample::Value(self.gpu_rpm.round() as u32));
        let mode = if self.gpu_manual { ControlMode::Manual } else { ControlMode::Driver };
        r.controls.insert(gpu_fan(), Sample::Value(ControlReading { percent: self.gpu_percent, raw: None, mode }));
        r
    }

    fn channel(&mut self, key: &Key) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| &pwm(c.h.n) == key)
    }

    fn carry_out(&mut self, cmd: &Command) -> Outcome {
        let key = cmd.key().clone();
        if key == gpu_fan() {
            return match cmd {
                Command::Take { .. } => Outcome::Taken { duty: 0.0 },
                Command::Write { raw, .. } | Command::Reassert { raw, .. } => {
                    self.gpu_manual = true;
                    self.gpu_percent = *raw as f64;
                    Outcome::Done
                }
                Command::Handback { .. } | Command::Release { .. } => {
                    self.gpu_manual = false;
                    Outcome::Done
                }
            };
        }
        let c = self.channel(&key).expect("a header of this desk");
        match cmd {
            Command::Take { .. } => {
                c.mode = ControlMode::Manual;
                Outcome::Taken { duty: c.h.firmware }
            }
            Command::Write { raw, .. } | Command::Reassert { raw, .. } => {
                c.mode = if *raw >= 255 { ControlMode::Full } else { ControlMode::Manual };
                c.raw = *raw;
                c.writes += 1;
                Outcome::Done
            }
            Command::Handback { .. } => Outcome::Done,
            Command::Release { .. } => {
                c.mode = ControlMode::Automatic(2);
                c.raw = raw(c.h.firmware);
                Outcome::Done
            }
        }
    }

    /// One second passes: firmware that takes headers back does, and every fan moves a second towards its duty.
    fn advance(&mut self) {
        let read = self.clock - self.read_at >= 1.5;
        if read {
            self.read_at = self.clock;
        }
        self.clock += 1.0;
        for c in &mut self.channels {
            if c.h.reverts && c.mode != ControlMode::Automatic(2) {
                c.mode = ControlMode::Automatic(2);
                c.raw = raw(c.h.firmware);
            }
            let duty = match c.mode {
                ControlMode::Full => 100.0,
                ControlMode::Manual => c.raw as f64 * 100.0 / 255.0,
                _ => c.h.firmware,
            };
            if read {
                c.shown = c.rpm;
            }
            let Some(f) = c.h.fan else { continue };
            if c.spinning && duty < f.stop {
                c.spinning = false;
            } else if !c.spinning && duty >= f.start {
                c.spinning = true;
            }
            c.boost = (c.boost + f.surge * (duty - c.duty).max(0.0)) * (-1.0 / f.surge_tau).exp();
            c.duty = duty;
            let target = f.rpm(duty, c.spinning);
            let target = if target > 0.0 { target + c.boost } else { 0.0 };
            c.rpm += (target - c.rpm) * (1.0 - (-1.0f64 / 1.4).exp());
            if target == 0.0 && c.rpm < 20.0 {
                c.rpm = 0.0;
            }
        }
        // The 3090's fans stand still in manual below 35–40 % (setup on this desk, 22 Oct 2026).
        let target = if !self.gpu_manual || self.gpu_percent < 38.0 { 0.0 } else { 500.0 + (self.gpu_percent - 40.0) / 60.0 * 2330.0 };
        self.gpu_rpm += (target - self.gpu_rpm) * (1.0 - (-1.0f64 / 2.0).exp());
        if target == 0.0 && self.gpu_rpm < 20.0 {
            self.gpu_rpm = 0.0;
        }
    }
}

fn raw(duty: f64) -> u32 {
    (duty * 255.0 / 100.0 - 1e-9).ceil().clamp(0.0, 255.0) as u32
}

/// The daemon's loop, one tick a second: read, setup, tick, carry out, record.
struct Rig {
    desk: Desk,
    plan: Plan,
    state: LoopState,
    setup: Setup,
    now: f64,
    load: Option<f64>,
    /// Where the CPU levels under the load check's full load; idle, it goes back to 45 °C.
    heat: f64,
    /// Seconds between ticks: pennad's `--tick`.
    dt: f64,
    /// How late each tick wakes, in seconds. The daemon's clock is the wall clock: each tick runs a little after it was due, by
    /// a different amount every time.
    late: Option<u64>,
    ticks: u64,
}

impl Rig {
    fn new() -> Rig {
        let desk = Desk::new();
        let inv = desk.inventory();
        let mut plan = Plan::new(penna_core::starter::starter(&inv), &inv, &Measured::default(), None).expect("the starter fits");
        let input = desk.input();
        let keys: Vec<Key> = input.targets.iter().map(|t| t.key.clone()).collect();
        plan.carry(&keys, &inv, &Measured::default());
        let state = LoopState::new(&plan);
        Rig { setup: Setup::new(input, 0.0), desk, plan, state, now: 0.0, load: None, heat: 45.0, dt: 1.0, late: None, ticks: 0 }
    }

    /// With the load check on (§5.5), on a CPU that levels at `heat` under full load: the 5950X's limit is 90 °C.
    fn loaded(heat: f64) -> Rig {
        let mut input = Desk::new().input();
        input.load_check = Some(LoadInput { key: cpu(), limit: 90.0 });
        Rig { setup: Setup::new(input, 0.0), heat, ..Rig::new() }
    }

    /// The rig on the wall clock: each tick wakes up to 2 ms late, by an amount drawn from `seed`.
    fn late(seed: u64) -> Rig {
        Rig { late: Some(seed), ..Rig::new() }
    }

    /// When this tick runs: on time, or late by up to 2 ms.
    fn clock(&self) -> f64 {
        let Some(seed) = self.late else { return self.now };
        let x = (seed ^ self.ticks).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 33;
        self.now + (x % 2000) as f64 * 1e-6
    }

    fn tick(&mut self) {
        // Until calibration, setup runs no fan below 50 %: a pump may be on any CPU header (§5.2, §5.3). A fan just taken keeps
        // its firmware's own duty for a tick, which is the firmware's choice.
        if self.setup.step_now() < Step::Calibration {
            for c in self.desk.channels.iter().filter(|c| c.mode == ControlMode::Manual) {
                assert!(c.raw >= raw(50.0) || c.raw == raw(c.h.firmware), "{} at raw {} before calibration", c.h.name, c.raw);
            }
        }
        let readings = self.desk.readings();
        let now = self.clock();
        self.state.measuring = self.setup.step(&readings, &self.state, self.load, now);
        let (next, cmds) = tick(&self.plan, &self.state, &readings, now);
        self.state = next;
        let mut queue: std::collections::VecDeque<Command> = cmds.into();
        while let Some(cmd) = queue.pop_front() {
            let outcome = self.desk.carry_out(&cmd);
            if let Some(follow) = self.state.record(&self.plan, &cmd, &outcome) {
                queue.push_back(follow);
            }
        }
        if self.setup.finished() {
            self.plan.end_setup();
        }
        self.plan.drop_released(&self.state);
        for _ in 0..self.dt.round() as u32 {
            self.desk.advance();
            let to = if self.setup.load_wanted() { self.heat } else { 45.0 };
            self.desk.cpu_temp += (to - self.desk.cpu_temp) * 0.08;
        }
        self.now += self.dt;
        self.ticks += 1;
    }

    fn until(&mut self, step: Step, max: f64) {
        while self.setup.step_now() < step && self.setup.stopped().is_none() {
            assert!(self.now < max, "setup never reached {step:?} by {max} s; it says: {}", self.setup.progress().say);
            self.tick();
        }
    }

    fn progress(&self, key: &Key) -> penna_core::setup::ControlProgress {
        self.setup.progress().controls.into_iter().find(|c| &c.key == key).unwrap()
    }
}

#[test]
fn setup_measures_this_desk_and_hands_every_fan_back() {
    let mut rig = Rig::new();
    rig.until(Step::WriteTest, 40.0);
    assert!(rig.now >= 30.0, "the idle sample takes 30 s");
    assert!(rig.desk.channels.iter().all(|c| c.writes == 0), "no fan is taken during the idle sample");

    rig.until(Step::Pairing, 50.0);
    assert_eq!(rig.progress(&pwm(1)).write, Some(WriteResult::Ok), "{:?}", rig.progress(&pwm(1)));
    let sys3 = rig.progress(&pwm(6));
    assert_eq!(sys3.write, Some(WriteResult::Reverts), "{sys3:?}");
    assert!(sys3.write_detail.as_deref().unwrap().contains("its firmware took it back (pwm_enable 2"), "{sys3:?}");

    rig.until(Step::Calibration, 200.0);
    let p = rig.setup.progress();
    for (n, name) in [(1, "CPU_FAN"), (2, "SYS_FAN1"), (3, "PCH_FAN"), (4, "SYS_FAN5_PUMP")] {
        let c = p.controls.iter().find(|c| c.key == pwm(n)).unwrap();
        assert_eq!(c.tachs, vec![fan(n)], "pwm{n} drives its own fan: {c:?}");
        assert_eq!(c.header.as_deref(), Some(name));
    }
    let empty = rig.progress(&pwm(5));
    assert_eq!((empty.paired, empty.tachs.len(), empty.duty), (Some(false), 0, None), "no fan on SYS_FAN6_PUMP: {empty:?}");
    assert_eq!(rig.progress(&pwm(6)).paired, None, "a header that reverts isn't paired");
    assert_eq!(rig.progress(&gpu_fan()).tachs, vec![gpu_fan()], "NVML pairs itself");

    // Roles, with their reasons (§11.3 calls 7 and 8).
    let role = |n: u32| rig.progress(&pwm(n)).suggestion.unwrap_or_else(|| panic!("{:?}", rig.progress(&pwm(n))));
    assert_eq!((role(1).role, role(1).reason.as_str()), (Role::Cpu, "The board calls this header CPU_FAN."));
    assert_eq!(role(2).role, Role::Case);
    let pch = role(3);
    assert!(pch.role == Role::Chipset && pch.may_stop, "the BIOS stopped it during the idle sample: {pch:?}");
    let pump = role(4);
    assert!(pump.role == Role::Case && pump.confirm, "{pump:?}");
    // Pairing's highest reading in its 6 s at 100 %, a little under the fan's top: the tach reads 2 s at a time.
    assert!(pump.reason.starts_with("On a pump header, but 1,6") && pump.reason.contains(": a case fan."), "{pump:?}");
    assert_eq!(rig.progress(&gpu_fan()).suggestion.unwrap().role, Role::Gpu);
    assert_eq!(rig.progress(&pwm(4)).lowest, Some(50.0), "held at 50 % until confirmed");
    let opt = role(7);
    assert!(opt.role == Role::Pump && opt.confirm, "a fast fan on CPU_OPT is suggested as a pump: {opt:?}");
    assert!(
        opt.reason.starts_with("The board calls this header CPU_OPT, and it turns 2,5"),
        "paired on its rise alone, flat as it is: {opt:?}"
    );
    assert_eq!(rig.progress(&pwm(7)).lowest, Some(50.0), "a pump is never stopped");
    assert_eq!(rig.progress(&pwm(3)).lowest, None, "measured to a stop, as its BIOS stops it");

    rig.until(Step::Review, 400.0);
    assert!(rig.now < 330.0, "about five minutes without the load check; took {} s", rig.now);
    let m = rig.setup.measured(1.0e9);
    let cal = |n: u32| m.get(&pwm(n)).unwrap().clone();
    // The stop duty is the lowest 5 % step that still turns the fan; the start is the first 2 % step up that spins it.
    assert_eq!((cal(1).stop, cal(1).start), (Some(20.0), Some(24.0)), "{:?}", cal(1));
    // SYS_FAN1 turns faster at 0 % than at 5 %: no 0 % point, no stop, and never run below 5 %.
    assert_eq!((cal(2).stop, cal(2).lowest, cal(2).calibration[0][0]), (None, Some(5.0), 5.0), "{:?}", cal(2));
    let note = rig.progress(&pwm(2)).note.unwrap_or_default();
    assert!(note.starts_with("At 0 % it turned faster, 2,1") && note.ends_with("Penna never runs it below 5 %."), "{note}");
    assert_eq!(cal(3).stop, Some(25.0), "PCH_FAN measured down to its stop");
    assert_eq!((cal(4).stop, cal(4).lowest), (None, Some(50.0)), "the pump header stops at 50 %");
    assert!(cal(7).stop.is_none() && cal(7).calibration.iter().all(|p| p[0] >= 50.0), "the pump is never taken below 50 %: {:?}", cal(7));
    assert_eq!(cal(4).calibration.first().unwrap()[0], 50.0);
    // Each point is the fan's steady speed: SYS_FAN4's 100 % too, which it overshoots by 400 RPM and comes back from slowly.
    for n in [1, 2, 3, 4, 8] {
        let t = &cal(n).calibration;
        let f = headers()[n as usize - 1].fan.unwrap();
        assert!(t.windows(2).all(|w| w[0][0] < w[1][0] && w[0][1] < w[1][1] + 30.0), "pwm{n}'s table rises: {t:?}");
        for p in t {
            let steady = f.rpm(p[0], true);
            assert!((p[1] - steady).abs() < steady * 0.03, "pwm{n} at {} %: {} RPM, where it settles at {steady:.0}: {t:?}", p[0], p[1]);
        }
    }
    assert!(cal(1).spin_up.is_some_and(|s| s > 0.0 && s <= 10.0), "{:?}", cal(1));
    let gpu = m.get(&gpu_fan()).unwrap();
    assert_eq!((gpu.stop, gpu.start, gpu.paired), (Some(40.0), Some(40.0), false), "it stood still at 35 %: {gpu:?}");
    assert_eq!(gpu.calibration.first().unwrap()[0], 40.0);
    let empty = m.get(&pwm(5)).unwrap();
    assert!(empty.empty && !empty.paired && empty.tachs.is_empty(), "the empty header is recorded as empty: {empty:?}");
    assert!(m.get(&pwm(1)).is_some_and(|c| c.paired && !c.empty));

    // What the plan makes of it: never below a measured stop, the GPU's included, nor below what setup went down to.
    let floor = |key: Key, may_stop: bool| {
        let kind = if key == gpu_fan() { ControlKind::Nvml { min: 30, max: 100 } } else { ControlKind::Duty { scale: 255 } };
        let cfg = penna_core::config::ControlConfig { may_stop, ..penna_core::config::ControlConfig::defaults(key) };
        penna_core::plan::control_plan(&cfg, Some((kind, "")), &m).0.floor
    };
    assert_eq!(floor(gpu_fan(), true), 40.0, "handed to its driver below its stop");
    assert_eq!(floor(gpu_fan(), false), 45.0, "or kept clear of it");
    assert_eq!(floor(pwm(2), false), 5.0, "SYS_FAN1: measured down to 5 % without stopping");
    assert_eq!(floor(pwm(7), false), 50.0, "the pump: never below 50 %");
    assert_eq!(floor(pwm(1), false), 25.0, "CPU_FAN: its stop and the margin");
    assert!(m.get(&pwm(3)).unwrap().firmware_stops);
    assert!((m.idle[&cpu()] - 45.0).abs() < 0.5, "{:?}", m.idle);

    // Measuring is over: nothing is asked of any fan, and the loop gives every one back as it was found.
    for _ in 0..3 {
        rig.tick();
    }
    assert!(rig.state.measuring.is_empty());
    assert!(rig.desk.channels.iter().all(|c| c.mode == ControlMode::Automatic(2)), "every header back on its BIOS");
    assert!(!rig.desk.gpu_manual, "the GPU fan back on its driver");
    assert!(rig.plan.carried.is_empty(), "nothing left in the plan that the configuration doesn't name");
}

/// The write test on the daemon's clock (§5.1): each tick wakes a little late, and the 22 Oct 2026 run on this desk found every
/// header "reads back other values" when the tick that wrote 60 % woke later than the next. What was written is the loop's to say.
#[test]
fn the_write_test_reads_back_what_the_loop_wrote_whenever_the_tick_wakes() {
    for seed in 0..24 {
        let mut rig = Rig::late(seed);
        rig.until(Step::Pairing, 50.0);
        for n in [1, 2, 3, 4, 5, 7, 8] {
            let c = rig.progress(&pwm(n));
            assert_eq!(c.write, Some(WriteResult::Ok), "seed {seed}, pwm{n}: {c:?}");
        }
        assert_eq!(rig.progress(&pwm(6)).write, Some(WriteResult::Reverts), "seed {seed}");
    }
}

/// And on a tick other than a second: `pennad --tick` takes 0.25 to 5 s.
#[test]
fn the_write_test_holds_on_a_two_second_tick() {
    let mut rig = Rig { dt: 2.0, ..Rig::new() };
    rig.until(Step::Pairing, 60.0);
    for n in [1, 2, 3, 4, 5, 7, 8] {
        let c = rig.progress(&pwm(n));
        assert_eq!(c.write, Some(WriteResult::Ok), "pwm{n}: {c:?}");
    }
    assert_eq!(rig.progress(&pwm(6)).write, Some(WriteResult::Reverts));
    assert_eq!(rig.progress(&gpu_fan()).write, Some(WriteResult::Ok));
}

/// The load check (§5.5): 3 minutes of full CPU load after calibration, measured before it starts, then the plateau.
#[test]
fn the_load_check_reports_where_the_cpu_levels() {
    let mut rig = Rig::loaded(76.0);
    rig.until(Step::LoadCheck, 400.0);
    assert!(rig.setup.measured_all() && !rig.setup.finished(), "what was measured can be saved before the load");
    assert!(rig.setup.load_wanted());
    let start = rig.now;
    rig.until(Step::Review, start + 200.0);
    assert!((rig.now - start - 180.0).abs() <= 2.0, "three minutes; took {} s", rig.now - start);
    assert!(!rig.setup.load_wanted(), "the load stops with the check");
    let r = rig.setup.progress().load.unwrap().result.unwrap();
    assert!((r.plateau - 76.0).abs() < 1.5 && !r.hot && r.earlier == 0.0, "{r:?}");
    assert!(rig.setup.progress().say.contains("Under 3 minutes of full load the CPU levelled at 7"), "{}", rig.setup.progress().say);
}

/// A CPU that passes its limit less 5 °C ends the pass, not setup: past the 85 °C guard, the 100 % point moves 5 °C earlier and
/// a second pass runs once it has cooled.
#[test]
fn a_hot_cpu_moves_the_100_percent_point_and_runs_once_more() {
    let mut rig = Rig::loaded(89.0);
    rig.until(Step::LoadCheck, 400.0);
    while rig.setup.load_wanted() {
        assert!(rig.now < 800.0);
        rig.tick();
    }
    assert!(rig.setup.stopped().is_none(), "past setup's 85 °C guard, the check goes on: {:?}", rig.setup.stopped());
    assert_eq!(rig.setup.take_adjust(), Some(5.0));
    assert_eq!(rig.setup.take_adjust(), None, "once");
    assert!(rig.setup.progress().say.contains("moves 5 °C earlier"), "{}", rig.setup.progress().say);
    let cooled = rig.now;
    while !rig.setup.load_wanted() {
        assert!(rig.now - cooled <= 121.0, "cooling is at most 2 minutes");
        rig.tick();
    }
    assert_eq!(rig.setup.progress().load.unwrap().pass, 2);
    rig.until(Step::Review, rig.now + 400.0);
    let r = rig.setup.progress().load.unwrap().result.unwrap();
    assert!(r.hot && r.earlier == 5.0 && r.peak >= 85.0, "{r:?}");
    assert!(rig.setup.progress().say.contains("its cooler is at its limit"), "{}", rig.setup.progress().say);
}

#[test]
fn a_case_fan_confirmed_on_a_pump_header_is_measured_on_below_50_percent() {
    let mut rig = Rig::new();
    rig.until(Step::Review, 400.0);
    let first = rig.setup.measured(1.0e9);
    for _ in 0..3 {
        rig.tick();
    }
    assert_eq!(rig.desk.channels[3].mode, ControlMode::Automatic(2), "SYS_FAN5_PUMP back on its BIOS, which runs it at 60 %");

    // Confirmed as a case fan, as pennad's setup.roles does it (§11.3 call 7): measured below the 50 % it was held to.
    let mut sys5 = first.get(&pwm(4)).unwrap().clone();
    sys5.role = Some(Role::Case);
    rig.plan.carry(&[pwm(4)], &rig.desk.inventory(), &first);
    rig.setup = Setup::calibrate(rig.desk.input(), std::slice::from_ref(&sys5), rig.now);
    let start = rig.now;
    rig.until(Step::Review, start + 120.0);
    assert!(rig.now - start <= 90.0, "about a minute and a half, from where the first run stopped; took {} s", rig.now - start);
    let again = rig.setup.measured(1.0e9 + 300.0);
    let after = again.get(&pwm(4)).unwrap();
    let above = |t: &[[f64; 2]]| t.iter().filter(|p| p[0] > 50.0).copied().collect::<Vec<_>>();
    assert_eq!(above(&after.calibration), above(&sys5.calibration), "what the first run measured above 50 % stays");
    // 50 % counts once it is written, not while the tach still reads the 60 % its BIOS ran when it was taken.
    let at = |t: &[[f64; 2]], d: f64| t.iter().find(|p| p[0] == d).map(|p| p[1]).unwrap();
    let (was, now) = (at(&sys5.calibration, 50.0), at(&after.calibration, 50.0));
    assert!((now - was).abs() < was * 0.03, "50 %: {now} RPM, where the first run read {was}");
    assert_eq!((after.stop, after.start, after.lowest), (Some(20.0), Some(26.0), None), "{after:?}");
    assert_eq!(after.role, Some(Role::Case));
    assert!(after.reason.as_deref().is_some_and(|r| r.starts_with("On a pump header, but 1,6")), "its reason stays: {after:?}");
    for _ in 0..3 {
        rig.tick();
    }
    assert_eq!(rig.desk.channels[3].mode, ControlMode::Automatic(2), "and back on its BIOS");
}

#[test]
fn a_guard_stops_setup_and_every_fan_goes_back() {
    let mut rig = Rig::new();
    rig.until(Step::Calibration, 200.0);
    for _ in 0..10 {
        rig.tick();
    }
    rig.desk.cpu_temp = 86.0;
    rig.tick();
    let stopped = rig.setup.stopped().expect("stopped").to_owned();
    assert_eq!(stopped, "CPU · Tctl reached 86 °C, past setup's guard of 85 °C.");
    assert!(rig.state.measuring.is_empty());
    rig.desk.cpu_temp = 50.0;
    for _ in 0..2 {
        rig.tick();
    }
    assert!(rig.desk.channels.iter().all(|c| c.mode == ControlMode::Automatic(2)), "every header back on its BIOS");
    assert!(!rig.desk.gpu_manual);
}

#[test]
fn a_busy_machine_waits_and_stop_is_at_once() {
    let mut rig = Rig::new();
    rig.load = Some(12.0);
    for _ in 0..40 {
        rig.tick();
    }
    let p = rig.setup.progress();
    assert_eq!(p.step, Step::Idle);
    assert_eq!(p.waiting.as_deref(), Some("The 1-minute load is 12.0; setup starts once it is under 8, a quarter of the threads."));
    rig.load = Some(0.4);
    rig.until(Step::Pairing, 120.0);
    for _ in 0..8 {
        rig.tick();
    }
    assert!(rig.desk.channels.iter().any(|c| c.mode == ControlMode::Manual), "setup holds the fans");
    rig.setup.stop("You stopped setup.");
    rig.tick();
    rig.tick();
    assert!(rig.desk.channels.iter().all(|c| c.mode == ControlMode::Automatic(2)));
    assert_eq!(rig.setup.progress().stopped.as_deref(), Some("You stopped setup."));
}

#[test]
fn an_emergency_outranks_setup() {
    let mut rig = Rig::new();
    rig.until(Step::Calibration, 200.0);
    // A CPU at its TjMax: every fan Penna holds goes to 100 % that tick, measuring or not (§6.3). The guard stops setup too.
    rig.desk.cpu_temp = 96.0;
    let readings = rig.desk.readings();
    rig.state.measuring = rig.setup.step(&readings, &rig.state, None, rig.now);
    assert!(rig.setup.stopped().is_some());
    let mut st = rig.state.clone();
    st.measuring = BTreeMap::from([(pwm(1), 20.0)]);
    let (next, cmds) = tick(&rig.plan, &st, &readings, rig.now);
    assert_eq!(next.controls[&pwm(1)].state, penna_core::State::Emergency, "{cmds:?}");
}
