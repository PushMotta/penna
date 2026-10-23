//! Setup (DESIGN.md §5): one run that measures every fan, as amended by design review 03 (§11.3). Pure, like the control loop:
//! [`Setup::step`] takes one tick's readings and says what duty each control it measures should run at. The daemon puts that in
//! [`LoopState::measuring`] before [`crate::control::tick`], which takes the control and runs it there; only an emergency outranks
//! it (§6.3). Time is the loop's clock, so tests run setup on simulated time.
//!
//! The steps: an idle sample before any fan is taken, a write test, pairing, roles, then calibration of every paired fan at once.
//! The load check and the generated curves follow in M8. Guards watch the whole run: a CPU past 85 °C, a GPU past 80 °C, or a
//! sensor that fails to read stops setup, and every fan goes back as it was found.
use crate::config::{Measured, MeasuredControl};
use crate::control::{LoopState, State};
use crate::drivers::Class;
use crate::model::{level_duty, level_of};
use crate::{ControlInfo, ControlKind, ControlMode, ControlReading, Device, Key, Readings, Sample, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The idle sample (§5, step 1).
pub const IDLE_SECONDS: f64 = 30.0;
/// A sensor's idle temperature is this quantile of its readings, not their mean: something that starts during the 30 s lifts the
/// mean and every curve's start with it. On the development desk a mean of 50 °C came from a CPU that rests at 41 °C.
pub const IDLE_QUANTILE: f64 = 0.2;
/// The write test writes these, reading each back a tick later, then watches (§5.1).
pub const WRITE_FIRST: f64 = 60.0;
pub const WRITE_SECOND: f64 = 80.0;
pub const WRITE_SECONDS: f64 = 5.0;
/// Pairing (§5.2): every fan settles at the hold, then one control at a time goes high, then low.
pub const PAIR_HOLD: f64 = 60.0;
pub const PAIR_SETTLE: f64 = 6.0;
pub const PAIR_HIGH: f64 = 100.0;
/// No lower than a pump's floor (§5.3): pairing runs before any fan's role is known, and a pump may be on any CPU header.
pub const PAIR_LOW: f64 = 50.0;
pub const PAIR_HIGH_SECONDS: f64 = 6.0;
pub const PAIR_LOW_SECONDS: f64 = 6.0;
/// A tach pairs when its highest reading at 100 % is this much above its reading just before, both as a share and in RPM.
pub const PAIR_RISE: f64 = 0.15;
pub const PAIR_RISE_RPM: f64 = 150.0;
/// The drop to 40 % confirms when the tach falls this far below where it started.
pub const PAIR_FALL: f64 = 0.05;
/// Calibration (§5.4).
pub const CAL_DOWN: f64 = 5.0;
pub const CAL_UP: f64 = 2.0;
pub const CAL_SETTLE_MIN: f64 = 2.0;
pub const CAL_SETTLE_MAX: f64 = 10.0;
/// A fan still moving one way at `CAL_SETTLE_MAX` gets this long: this desk's SYS_FAN4 overshoots a raise to 100 % by 300 RPM
/// and is still coming back 10 s later.
pub const CAL_DRIFT_MAX: f64 = 30.0;
/// Readings are judged this far apart: it87 reads the chip at most every 1.5 s, so on a 1 s tick every other reading repeats
/// the last, and two that are equal say nothing about whether the fan has stopped moving.
pub const CAL_SPAN: f64 = 2.0;
/// Stable: three readings `CAL_SPAN` apart within this share of their mean, and the last two this close. Moving: all three
/// one way, the first and last further apart than this share or `CAL_STILL_RPM`.
pub const CAL_STABLE: f64 = 0.03;
pub const CAL_STILL: f64 = 0.015;
pub const CAL_STILL_RPM: f64 = 20.0;
pub const CAL_DRIFT: f64 = 0.01;
/// Seconds at each duty while a stopped fan is stepped up.
pub const CAL_UP_WAIT: f64 = 2.0;
/// A CPU fan is never stopped longer than this (§5.3); other fans get up to the second.
pub const CPU_STOPPED_MAX: f64 = 10.0;
pub const STOPPED_MAX: f64 = 30.0;
/// Below this a tach counts as stopped.
pub const STOPPED_RPM: f64 = 50.0;
/// Where calibration's descent ends for fans that must not stop (§5.3, §5.4).
pub const PUMP_LOWEST: f64 = 50.0;
pub const CHIPSET_LOWEST: f64 = 30.0;
/// A fan on a pump header is a pump only if it turns faster than this at 100 % (§5.3).
pub const PUMP_RPM: f64 = 2000.0;
/// A tach the idle sample saw stopped for this long was stopped by its firmware (§5.3).
pub const FIRMWARE_STOP_SECONDS: f64 = 10.0;
/// The guards (§5).
pub const GUARD_CPU: f64 = 85.0;
pub const GUARD_GPU: f64 = 80.0;
/// What the last step of calibration usually takes on a desk: for the time left.
const CAL_ESTIMATE: f64 = 180.0;
/// The load check (§5.5): every CPU thread busy at the lowest priority for 3 minutes, on the proposed Balanced curves. Passing
/// the CPU's limit less 5 °C ends the pass: the 100 % point moves 5 °C earlier and the check runs once more, after the CPU has
/// cooled to 10 °C above idle, or for 2 minutes. The plateau is the mean of the last 30 s.
pub const LOAD_SECONDS: f64 = 180.0;
pub const LOAD_MARGIN: f64 = 5.0;
pub const LOAD_PLATEAU: f64 = 30.0;
pub const LOAD_COOL_ABOVE_IDLE: f64 = 10.0;
pub const LOAD_COOL_MAX: f64 = 120.0;
/// Read-back tolerance, as in the control loop (§6.4).
const READBACK_TOLERANCE: f64 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Idle,
    WriteTest,
    Pairing,
    Roles,
    Calibration,
    /// Optional: the CPU under full load on the proposed curves (§5.5, step 6). What was measured is saved before it starts.
    LoadCheck,
    /// Measuring is over; the roles wait for the user (§5, step 7).
    Review,
}

impl Step {
    pub fn name(self) -> &'static str {
        match self {
            Step::Idle => "Idle sample",
            Step::WriteTest => "Write test",
            Step::Pairing => "Pairing",
            Step::Roles => "Roles",
            Step::Calibration => "Calibration",
            Step::LoadCheck => "Load check",
            Step::Review => "Review",
        }
    }
}

/// What a fan does, which decides its generated curve and how it is calibrated (§5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Cpu,
    Gpu,
    Pump,
    Chipset,
    Laptop,
    Case,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Cpu => "CPU cooler",
            Role::Gpu => "GPU",
            Role::Pump => "Pump",
            Role::Chipset => "Chipset",
            Role::Laptop => "Laptop",
            Role::Case => "Case",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" | "cpu cooler" => Some(Role::Cpu),
            "gpu" => Some(Role::Gpu),
            "pump" => Some(Role::Pump),
            "chipset" => Some(Role::Chipset),
            "laptop" => Some(Role::Laptop),
            "case" => Some(Role::Case),
            _ => None,
        }
    }
}

/// The write test's result (§5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteResult {
    Ok,
    /// Reads back a different value, the same one every time.
    Maps,
    /// Back to its firmware within the test: something else drives it.
    Reverts,
    /// The write failed.
    Refused,
}

impl WriteResult {
    /// Offered for curves and measured further. A map is recorded but not offered until the loop's read-back follows one (M9).
    pub fn offered(self) -> bool {
        self == WriteResult::Ok
    }
}

/// A control setup may measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub key: Key,
    /// How it is called before pairing names it for a header: "IT8688E pwm1", "GPU fan 1".
    pub label: String,
    pub kind: ControlKind,
    /// What its driver is for: a laptop's and a GPU's fans take their role from it (§5.3).
    pub class: Class,
    /// The tachs its driver says it turns, so pairing skips it (§5.2). Empty: pairing finds them.
    pub tachs: Vec<Key>,
}

impl Target {
    /// A control as setup sees it. An NVIDIA fan is its own tach. A laptop's or a GPU's `pwmN` turns its `fanN`, and a device
    /// with one control turns every tach it has: a ThinkPad's `pwm1` runs both its fans. A motherboard's are found by pairing,
    /// because hwmon doesn't say which header a `pwmN` drives (§3.1).
    pub fn of(device: &Device, control: &ControlInfo) -> Target {
        let class = match device.source {
            Source::Nvml => Class::Gpu,
            Source::Hwmon => crate::drivers::lookup(&device.driver).map_or(Class::Other, |d| d.class),
        };
        let tachs = match (device.source, class) {
            (Source::Nvml, _) => vec![control.key.clone()],
            (Source::Hwmon, Class::Laptop | Class::Gpu) if device.controls.len() == 1 => {
                device.tachs.iter().map(|t| t.key.clone()).collect()
            }
            (Source::Hwmon, Class::Laptop | Class::Gpu) => {
                let n = control.key.channel().strip_prefix("pwm").unwrap_or_default();
                device.tachs.iter().filter(|t| t.key.channel() == format!("fan{n}")).map(|t| t.key.clone()).collect()
            }
            _ => Vec::new(),
        };
        Target { key: control.key.clone(), label: control.label.clone(), kind: control.kind, class, tachs }
    }
}

/// A tach, with what the board database says about its header (§3.6).
#[derive(Clone, Debug, PartialEq)]
pub struct TachHint {
    pub key: Key,
    /// The header's name, or the tach's own.
    pub label: String,
    /// The board database's role hint: `cpu`, `pump-header`, `chipset`.
    pub role: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Guard {
    pub key: Key,
    pub label: String,
    pub limit: f64,
}

/// What the daemon knows when setup starts.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Input {
    pub targets: Vec<Target>,
    pub tachs: Vec<TachHint>,
    pub guards: Vec<Guard>,
    /// The CPU's threads: setup waits while the 1-minute load is above a quarter of them.
    pub threads: u32,
    /// Run the load check on this CPU source after calibration (§5.5).
    pub load_check: Option<LoadInput>,
}

/// The CPU source the load check watches, and its limit: TjMax, or its own critical temperature.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadInput {
    pub key: Key,
    pub limit: f64,
}

/// Where the load check is (§5.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadProgress {
    /// The CPU sensor it watches.
    #[serde(default)]
    pub source: Option<Key>,
    /// The pass, 1 or 2, and whether the CPU is loaded now or cooling before the second.
    pub pass: u8,
    pub loading: bool,
    /// Seconds into this pass, or into the cooling.
    pub seconds: f64,
    pub celsius: Option<f64>,
    /// The CPU fans' duty now.
    pub fans: Option<f64>,
    pub limit: f64,
    pub result: Option<LoadResult>,
}

/// What the load check found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadResult {
    /// The CPU's temperature and its fans' duty over the last 30 s of the last pass, and its hottest reading.
    pub plateau: f64,
    pub fans: Option<f64>,
    pub peak: f64,
    /// The 100 % point moved this many °C earlier.
    pub earlier: f64,
    /// The last pass passed the limit less 5 °C too: the cooler is at its limit.
    pub hot: bool,
}

/// The load check while it runs.
#[derive(Clone, Debug, PartialEq)]
struct Load {
    pass: u8,
    loading: bool,
    since: f64,
    /// (seconds, °C) and (seconds, CPU fans' duty) in this pass.
    temps: Vec<(f64, f64)>,
    duties: Vec<(f64, f64)>,
    earlier: f64,
    /// The daemon has yet to move the 100 % point.
    adjust: bool,
    result: Option<LoadResult>,
}

/// What setup suggests a fan is, and why (§5.3, §11.3 call 7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub role: Role,
    pub reason: String,
    /// A dashed chip: held to its role's limits until the user confirms it.
    pub confirm: bool,
    /// Its firmware was seen stopping it, so it may stop (§11.3 call 8).
    pub may_stop: bool,
}

/// Where calibration is with one control.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cal {
    Waiting,
    Down { duty: f64, since: f64 },
    Up { duty: f64, since: f64, stopped_since: f64 },
    Spin { duty: f64, since: f64 },
    Done,
}

#[derive(Clone, Debug, PartialEq)]
struct Ctl {
    target: Target,
    write: Option<WriteResult>,
    /// The tick of the test's first write.
    write_at: Option<f64>,
    /// What the write test read back: written raw → read raw, while it still looked like a map.
    map: Vec<(u32, u32)>,
    write_detail: Option<String>,
    paired: Option<bool>,
    tachs: Vec<Key>,
    /// Pairing's reading of its own tach: before, highest at 100 %, lowest at 40 %.
    rise: Option<[f64; 3]>,
    suggestion: Option<Suggestion>,
    cal: Cal,
    /// The tach since the duty was written: when, and what it read.
    samples: Vec<(f64, f64)>,
    table: Vec<[f64; 2]>,
    stop: Option<f64>,
    start: Option<f64>,
    spin_up: Option<f64>,
    lowest: Option<f64>,
    /// What calibration found that a person should know, such as a header that runs its fan faster at a lower duty.
    note: Option<String>,
    firmware_stops: bool,
    /// The role the user confirmed, when calibration runs again after the review (§11.3 call 7).
    confirmed: Option<Role>,
    demand: Option<f64>,
    status: String,
}

impl Ctl {
    /// The confirmed role, else the suggested one.
    fn role(&self) -> Option<Role> {
        self.confirmed.or(self.suggestion.as_ref().map(|s| s.role))
    }

    /// Its suggestion, quoting calibration's speed at 100 % once there is one: pairing's 6 s at 100 % can end before a fan is up
    /// to speed. SYS_FAN5_PUMP on the development desk read 1,450 RPM in pairing and 1,819 calibrated.
    fn suggested(&self) -> Option<Suggestion> {
        let mut s = self.suggestion.clone()?;
        if let (Some([_, paired, _]), Some([_, top])) = (self.rise, self.table.iter().find(|p| p[0] == 100.0)) {
            s.reason = s.reason.replace(&rpm(paired), &rpm(*top));
        }
        Some(s)
    }
}

/// One control's progress, for the setup view and `penna setup` (§8.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlProgress {
    pub key: Key,
    pub label: String,
    /// Its header, once pairing has found which tach it turns.
    pub header: Option<String>,
    pub status: String,
    pub duty: Option<f64>,
    pub write: Option<WriteResult>,
    pub write_detail: Option<String>,
    /// `null` until pairing reaches it; false: no fan turns on it.
    pub paired: Option<bool>,
    pub tachs: Vec<Key>,
    /// RPM before the rise, highest at 100 %, lowest at 40 %.
    pub rise: Option<[f64; 3]>,
    pub suggestion: Option<Suggestion>,
    pub table: Vec<[f64; 2]>,
    pub stop: Option<f64>,
    pub start: Option<f64>,
    pub spin_up: Option<f64>,
    pub lowest: Option<f64>,
    /// What calibration found that a person should know.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuardProgress {
    pub key: Key,
    pub label: String,
    pub celsius: Option<f64>,
    pub limit: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepProgress {
    pub step: Step,
    /// Expected seconds on this machine.
    pub seconds: f64,
}

/// Everything a surface draws about a setup run, once a tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub step: Step,
    pub steps: Vec<StepProgress>,
    /// Loop seconds since it started, and roughly how many are left.
    pub elapsed: f64,
    pub left: f64,
    /// What is happening, in words.
    pub say: String,
    /// Waiting for the machine to idle, and why.
    pub waiting: Option<String>,
    /// Why it stopped, when it did: every fan went back as it was found.
    pub stopped: Option<String>,
    /// Pairing: the control whose window it is, and the tach moving most with it.
    pub current: Option<Key>,
    pub mover: Option<Key>,
    pub controls: Vec<ControlProgress>,
    pub guards: Vec<GuardProgress>,
    #[serde(default)]
    pub load: Option<LoadProgress>,
    /// Once measuring is over: the curves the daemon proposes, as a draft on setup's connection (§5.5).
    #[serde(default)]
    pub proposed: Option<crate::generate::Proposal>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    input: Input,
    started: f64,
    now: f64,
    step: Step,
    step_at: f64,
    stopped: Option<String>,
    waiting: Option<String>,
    ctls: Vec<Ctl>,
    idle_temps: BTreeMap<Key, Vec<f64>>,
    idle_stopped: BTreeMap<Key, f64>,
    last_idle: Option<f64>,
    /// Pairing: which control's window, and the tachs' readings at its start, highest and lowest.
    window: Option<usize>,
    base: BTreeMap<Key, f64>,
    peak: BTreeMap<Key, f64>,
    low: BTreeMap<Key, f64>,
    claims: BTreeMap<Key, Vec<usize>>,
    mover: Option<Key>,
    say: String,
    guards: BTreeMap<Key, Option<f64>>,
    load: Option<Load>,
}

impl Setup {
    pub fn new(input: Input, now: f64) -> Setup {
        let ctls = input
            .targets
            .iter()
            .map(|t| Ctl {
                target: t.clone(),
                write: None,
                write_at: None,
                map: Vec::new(),
                write_detail: None,
                paired: None,
                tachs: Vec::new(),
                rise: None,
                suggestion: None,
                cal: Cal::Waiting,
                samples: Vec::new(),
                table: Vec::new(),
                stop: None,
                start: None,
                spin_up: None,
                lowest: None,
                note: None,
                firmware_stops: false,
                confirmed: None,
                demand: None,
                status: match t.class {
                    Class::Gpu => "Driver".to_owned(),
                    Class::Laptop => "Firmware".to_owned(),
                    _ => "BIOS".to_owned(),
                },
            })
            .collect();
        Setup {
            input,
            started: now,
            now,
            step: Step::Idle,
            step_at: now,
            stopped: None,
            waiting: None,
            ctls,
            idle_temps: BTreeMap::new(),
            idle_stopped: BTreeMap::new(),
            last_idle: None,
            window: None,
            base: BTreeMap::new(),
            peak: BTreeMap::new(),
            low: BTreeMap::new(),
            claims: BTreeMap::new(),
            mover: None,
            say: String::new(),
            guards: BTreeMap::new(),
            load: None,
        }
    }

    /// Calibration alone, for fans whose pairing is known and whose role the user has just confirmed (§11.3 call 7): a case
    /// fan on a pump header, measured below the 50 % it was held to. `known` is what the last run measured of each, with the
    /// confirmed role. Calibration goes on from where that run stopped and keeps what it measured above.
    pub fn calibrate(input: Input, known: &[MeasuredControl], now: f64) -> Setup {
        let mut s = Setup::new(input, now);
        s.ctls.retain(|c| known.iter().any(|m| m.key == c.target.key && m.role.is_some()));
        for c in &mut s.ctls {
            let m = known.iter().find(|m| m.key == c.target.key).expect("kept above");
            let role = m.role.expect("kept above");
            let from = m.lowest.unwrap_or(100.0);
            c.write = m.write.or(Some(WriteResult::Ok));
            c.paired = Some(true);
            c.tachs = m.tachs.clone();
            c.firmware_stops = m.firmware_stops;
            c.lowest = match role {
                Role::Pump => Some(PUMP_LOWEST),
                Role::Chipset if !m.firmware_stops => Some(CHIPSET_LOWEST),
                _ => None,
            };
            c.suggestion = Some(Suggestion {
                role: m.suggested.unwrap_or(role),
                reason: m.reason.clone().unwrap_or_default(),
                confirm: false,
                may_stop: m.firmware_stops,
            });
            c.confirmed = Some(role);
            c.table = m.calibration.iter().filter(|p| p[0] > from + 1e-9).copied().collect();
            c.cal = Cal::Down { duty: from, since: now };
            c.demand = Some(from);
            c.status = format!("stepping down · {from:.0} %");
        }
        s.enter(Step::Calibration);
        s.say = format!("Stepping down in {CAL_DOWN:.0} % steps from where the last run stopped, waiting for each to settle.");
        s
    }

    pub fn step_now(&self) -> Step {
        self.step
    }

    /// Measuring is over: stopped, or waiting in review. Setup asks for no duty any more.
    pub fn finished(&self) -> bool {
        self.stopped.is_some() || self.step == Step::Review
    }

    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// Stops at once: no control is asked for a duty any more, so the loop gives each back as it was found.
    pub fn stop(&mut self, reason: impl Into<String>) {
        if self.stopped.is_none() && self.step != Step::Review {
            self.stopped = Some(reason.into());
            for c in &mut self.ctls {
                c.demand = None;
            }
            self.say = "Stopped. Every fan is back as it was found.".to_owned();
        }
    }

    /// One tick: what each measured control should run at. `load1` is the 1-minute load average, when it is known.
    pub fn step(&mut self, readings: &Readings, state: &LoopState, load1: Option<f64>, now: f64) -> BTreeMap<Key, f64> {
        self.now = now;
        if !self.finished() {
            self.check_guards(readings);
        }
        if !self.finished() {
            match self.step {
                Step::Idle => self.idle(readings, load1),
                Step::WriteTest => self.write_test(readings, state),
                Step::Pairing => self.pairing(readings),
                Step::Roles => self.roles(),
                Step::Calibration => self.calibration(readings, state),
                Step::LoadCheck => self.load_check(readings, state),
                Step::Review => {}
            }
        }
        self.ctls.iter().filter_map(|c| c.demand.map(|d| (c.target.key.clone(), d))).collect()
    }

    fn enter(&mut self, step: Step) {
        self.step = step;
        self.step_at = self.now;
    }

    fn check_guards(&mut self, readings: &Readings) {
        // Under the load check the CPU is meant to get hot: its limit less 5 °C ends a pass instead (§5.5), and the emergency rule
        // (§6.3) still stands behind it.
        let loaded = self.input.load_check.as_ref().filter(|_| self.step == Step::LoadCheck).map(|l| l.key.clone());
        for g in self.input.guards.clone() {
            match readings.temps.get(&g.key) {
                Some(Sample::Value(v)) => {
                    self.guards.insert(g.key.clone(), Some(*v));
                    if *v >= g.limit && loaded.as_ref() != Some(&g.key) {
                        self.stop(format!("{} reached {:.0} °C, past setup's guard of {:.0} °C.", g.label, v, g.limit));
                        return;
                    }
                }
                Some(Sample::Implausible(v)) => {
                    self.guards.insert(g.key.clone(), None);
                    self.stop(format!("{} read {v}, which is not a temperature.", g.label));
                    return;
                }
                Some(Sample::Error(e)) => {
                    self.guards.insert(g.key.clone(), None);
                    self.stop(format!("{} failed to read: {e}.", g.label));
                    return;
                }
                None => {
                    self.guards.insert(g.key.clone(), None);
                    self.stop(format!("{} was not read.", g.label));
                    return;
                }
            }
        }
    }

    fn idle(&mut self, readings: &Readings, load1: Option<f64>) {
        let limit = self.input.threads as f64 / 4.0;
        if let Some(l) = load1.filter(|l| self.input.threads > 0 && *l > limit) {
            self.waiting =
                Some(format!("The 1-minute load is {l:.1}; setup starts once it is under {limit:.0}, a quarter of the threads."));
            self.say = "Waiting for the machine to idle.".to_owned();
            self.step_at = self.now;
            self.idle_temps.clear();
            self.idle_stopped.clear();
            self.last_idle = None;
            return;
        }
        self.waiting = None;
        let dt = self.last_idle.map_or(0.0, |t| self.now - t);
        self.last_idle = Some(self.now);
        for (k, s) in &readings.temps {
            if let Sample::Value(v) = s {
                self.idle_temps.entry(k.clone()).or_default().push(*v);
            }
        }
        for t in &self.input.tachs {
            if let Some(Sample::Value(r)) = readings.rpms.get(&t.key)
                && (*r as f64) < STOPPED_RPM
            {
                *self.idle_stopped.entry(t.key.clone()).or_insert(0.0) += dt;
            }
        }
        let left = IDLE_SECONDS - (self.now - self.step_at);
        self.say = format!("Watching every sensor before any fan is taken · {:.0} s left", left.max(0.0));
        if left <= 0.0 {
            self.enter(Step::WriteTest);
            for c in &mut self.ctls {
                c.demand = Some(WRITE_FIRST);
                c.status = format!("writing {WRITE_FIRST:.0} %");
            }
            self.say = format!("Writing {WRITE_FIRST:.0} % to every fan, then {WRITE_SECOND:.0} %, and reading each back.");
        }
    }

    fn write_test(&mut self, readings: &Readings, state: &LoopState) {
        let u = self.now - self.step_at;
        let now = self.now;
        for c in &mut self.ctls {
            if c.write.is_some() {
                continue;
            }
            let cs = state.controls.get(&c.target.key);
            if let Some(cs) = cs.filter(|cs| cs.state == State::Fault) {
                c.write = Some(WriteResult::Refused);
                c.write_detail = cs.reason.clone();
                c.demand = None;
                c.status = "refused the write".to_owned();
                continue;
            }
            // The loop takes a control on the tick setup first asks for it, and writes on the next: the test runs from there.
            if c.write_at.is_none() {
                if cs.is_some_and(|cs| cs.held) {
                    c.write_at = Some(now);
                } else {
                    if u >= WRITE_SECONDS {
                        c.write = Some(WriteResult::Refused);
                        c.write_detail = Some("Penna couldn't take it from its firmware".to_owned());
                        c.demand = None;
                        c.status = "couldn't be taken".to_owned();
                    }
                    continue;
                }
            }
            if let Some(cs) = cs.filter(|cs| cs.state == State::Contested) {
                c.write = Some(WriteResult::Reverts);
                c.write_detail = cs.reason.clone();
                c.demand = None;
                c.status = "its firmware writes over it".to_owned();
                continue;
            }
            let v = now - c.write_at.unwrap_or(now);
            // This tick's readings show what the loop wrote a tick ago, and the loop says what that was. The clock can't: a tick
            // wakes a little late, by a different amount each time, and the daemon's tick isn't always a second.
            let asked = cs
                .filter(|cs| cs.state == State::Measuring)
                .and_then(|cs| [WRITE_FIRST, WRITE_SECOND].into_iter().find(|d| (cs.duty - d).abs() < 0.5));
            if asked.is_none() && v >= 2.0 * WRITE_SECONDS {
                c.write = Some(WriteResult::Refused);
                c.write_detail = Some("Penna's writes never reached it".to_owned());
                c.demand = None;
                c.status = "couldn't be written".to_owned();
                continue;
            }
            if let Some(duty) = asked {
                match readback(c.target.kind, readings.controls.get(&c.target.key), duty) {
                    Check::Ok => {}
                    Check::Reverted(why) => {
                        c.write = Some(WriteResult::Reverts);
                        c.write_detail = Some(why);
                        c.demand = None;
                        c.status = "its firmware writes over it".to_owned();
                        continue;
                    }
                    Check::Mapped(want, got) => {
                        if c.map.iter().any(|(w, g)| *w == want && *g != got) {
                            c.write = Some(WriteResult::Reverts);
                            c.write_detail = Some(format!("read back {got} where it had read {}", c.map[0].1));
                            c.demand = None;
                            c.status = "its firmware writes over it".to_owned();
                            continue;
                        }
                        if !c.map.iter().any(|(w, _)| *w == want) {
                            c.map.push((want, got));
                        }
                    }
                    Check::Unread(why) => {
                        c.write = Some(WriteResult::Refused);
                        c.write_detail = Some(why);
                        c.demand = None;
                        c.status = "can't be read back".to_owned();
                        continue;
                    }
                }
            }
            if asked.is_some() {
                c.demand = Some(WRITE_SECOND);
            }
            c.status = match asked {
                None => format!("writing {WRITE_FIRST:.0} %"),
                Some(WRITE_FIRST) => format!("{WRITE_FIRST:.0} % read back · writing {WRITE_SECOND:.0} %"),
                Some(_) => format!("{WRITE_SECOND:.0} % read back · watching"),
            };
            if asked == Some(WRITE_SECOND) && v >= WRITE_SECONDS {
                let maps = !c.map.is_empty();
                c.write = Some(if maps { WriteResult::Maps } else { WriteResult::Ok });
                if maps {
                    let pairs: Vec<String> = c.map.iter().map(|(w, g)| format!("{w} → {g}")).collect();
                    c.write_detail = Some(format!("reads back other values: {}", pairs.join(", ")));
                    c.demand = None;
                    c.status = "reads back other values".to_owned();
                } else {
                    c.status = "write test ok".to_owned();
                }
            }
        }
        if self.ctls.iter().all(|c| c.write.is_some()) {
            self.enter(Step::Pairing);
            for c in &mut self.ctls {
                if c.write.is_some_and(WriteResult::offered) {
                    c.demand = Some(PAIR_HOLD);
                    c.status = format!("settling at {PAIR_HOLD:.0} %");
                }
                if !c.target.tachs.is_empty() && c.write.is_some_and(WriteResult::offered) {
                    // Its driver says which tachs it turns: NVML's own, a laptop's `fanN` (§5.2).
                    c.tachs = c.target.tachs.clone();
                    c.paired = Some(true);
                }
            }
            self.say = format!("Holding every fan at {PAIR_HOLD:.0} % so each rise is measured from a steady speed.");
        }
    }

    /// The controls pairing steps through, in order: those whose driver doesn't say which tachs they turn.
    fn pair_order(&self) -> Vec<usize> {
        (0..self.ctls.len())
            .filter(|&i| self.ctls[i].target.tachs.is_empty() && self.ctls[i].write.is_some_and(WriteResult::offered))
            .collect()
    }

    fn pairing(&mut self, readings: &Readings) {
        let u = self.now - self.step_at;
        let order = self.pair_order();
        let span = PAIR_HIGH_SECONDS + PAIR_LOW_SECONDS;
        if u < PAIR_SETTLE {
            return;
        }
        let n = ((u - PAIR_SETTLE) / span).floor() as usize;
        let k = u - PAIR_SETTLE - n as f64 * span;
        // A window ends: who moved with it.
        if let Some(w) = self.window
            && w != n
        {
            self.close_window(w, &order);
        }
        if n >= order.len() {
            self.finish_pairing();
            return;
        }
        let i = order[n];
        if self.window != Some(n) {
            self.window = Some(n);
            self.base.clear();
            self.peak.clear();
            self.low.clear();
            for t in &self.input.tachs {
                if let Some(Sample::Value(r)) = readings.rpms.get(&t.key) {
                    self.base.insert(t.key.clone(), *r as f64);
                    self.peak.insert(t.key.clone(), *r as f64);
                    self.low.insert(t.key.clone(), *r as f64);
                }
            }
        } else {
            for t in &self.input.tachs {
                if let Some(Sample::Value(r)) = readings.rpms.get(&t.key) {
                    let r = *r as f64;
                    if k <= PAIR_HIGH_SECONDS {
                        self.peak.entry(t.key.clone()).and_modify(|p| *p = p.max(r)).or_insert(r);
                    } else {
                        self.low.entry(t.key.clone()).and_modify(|p| *p = p.min(r)).or_insert(r);
                    }
                }
            }
        }
        let high = k < PAIR_HIGH_SECONDS;
        for &j in &order {
            let c = &mut self.ctls[j];
            if j == i {
                c.demand = Some(if high { PAIR_HIGH } else { PAIR_LOW });
                c.status = format!("{:.0} % for {:.0} s", c.demand.unwrap_or(0.0), if high { PAIR_HIGH_SECONDS } else { PAIR_LOW_SECONDS });
            } else if c.paired.is_none() {
                c.demand = Some(PAIR_HOLD);
                c.status = format!("held at {PAIR_HOLD:.0} %");
            }
        }
        // Say what moves.
        let label = self.ctls[i].target.label.clone();
        let mover = self
            .input
            .tachs
            .iter()
            .filter_map(|t| {
                let (b, now) = (self.base.get(&t.key)?, readings.rpms.get(&t.key).and_then(Sample::value)? as f64);
                Some((t, *b, now))
            })
            .max_by(|a, b| (a.2 - a.1).abs().total_cmp(&(b.2 - b.1).abs()));
        self.mover = mover.filter(|(_, b, now)| (now - b).abs() >= PAIR_RISE_RPM).map(|(t, ..)| t.key.clone());
        self.say = match mover {
            Some((t, b, now)) if (now - b).abs() >= PAIR_RISE_RPM => {
                if high {
                    format!("{label} at {PAIR_HIGH:.0} %: {} {} → {} RPM", t.label, rpm(b), rpm(now))
                } else {
                    format!("{label} at {PAIR_LOW:.0} %: {} fell to {} RPM", t.label, rpm(now))
                }
            }
            _ if high => format!("{label} at {PAIR_HIGH:.0} %: no fan has moved yet"),
            _ => format!("{label} at {PAIR_LOW:.0} %: no fan moved with it"),
        };
    }

    /// Pairs control `order[w]` with every tach that rose with it and fell again (§5.2).
    fn close_window(&mut self, w: usize, order: &[usize]) {
        let Some(&i) = order.get(w) else { return };
        // Every tach that rose with this control, and whether it fell again at the low step. A tach an earlier control turns,
        // still coming back from its low step, rises too: that one is explained, and left out.
        let risers: Vec<(Key, String, [f64; 3], bool)> = self
            .input
            .tachs
            .iter()
            .filter(|t| !self.claims.contains_key(&t.key))
            .filter_map(|t| {
                let (b, p, l) = (*self.base.get(&t.key)?, *self.peak.get(&t.key)?, *self.low.get(&t.key)?);
                let rose = p - b >= PAIR_RISE_RPM && p >= b * (1.0 + PAIR_RISE);
                rose.then(|| (t.key.clone(), t.label.clone(), [b, p, l], l <= b * (1.0 - PAIR_FALL)))
            })
            .collect();
        // One tach that rose is this control's, whether or not it slowed at the low step: a pump barely does. When several
        // rose, only those that fell again are.
        let lone = risers.len() == 1;
        let mut found = Vec::new();
        let mut rose_only = Vec::new();
        for (key, label, r, fell) in risers {
            if fell || lone {
                found.push((key, r));
            } else {
                rose_only.push(label);
            }
        }
        let c = &mut self.ctls[i];
        c.rise = found.first().map(|(_, r)| *r);
        for (k, _) in &found {
            self.claims.entry(k.clone()).or_default().push(i);
        }
        c.tachs = found.into_iter().map(|(k, _)| k).collect();
        c.paired = Some(!c.tachs.is_empty());
        if !rose_only.is_empty() {
            c.write_detail = Some(format!("{} rose with it but didn't fall at {PAIR_LOW:.0} %, so it isn't paired", rose_only.join(", ")));
        }
        if c.tachs.is_empty() {
            c.demand = None;
            c.status = "no fan turns on it".to_owned();
        } else {
            c.demand = Some(PAIR_HOLD);
            c.status = "paired".to_owned();
        }
        self.window = None;
    }

    fn finish_pairing(&mut self) {
        // A tach two controls moved belongs to neither: it is reported, not paired (§5.2).
        for (tach, owners) in self.claims.clone() {
            if owners.len() < 2 {
                continue;
            }
            let names: Vec<String> = owners.iter().map(|&i| self.ctls[i].target.label.clone()).collect();
            for &i in &owners {
                let c = &mut self.ctls[i];
                c.tachs.retain(|t| t != &tach);
                c.write_detail = Some(format!("{tach} moved with {}, so it isn't paired", names.join(" and ")));
                if c.tachs.is_empty() {
                    c.paired = Some(false);
                    c.demand = None;
                    c.status = "no fan of its own".to_owned();
                }
            }
        }
        self.enter(Step::Roles);
        self.roles();
    }

    fn header(&self, c: &Ctl) -> Option<String> {
        if !c.target.tachs.is_empty() {
            return Some(c.target.label.clone());
        }
        c.tachs.first().and_then(|k| self.input.tachs.iter().find(|t| &t.key == k)).map(|t| t.label.clone())
    }

    /// Suggests a role for every paired fan, with its reason (§5.3, §11.3 calls 7 and 8), then calibrates.
    fn roles(&mut self) {
        let idle_stopped = self.idle_stopped.clone();
        for i in 0..self.ctls.len() {
            let c = &self.ctls[i];
            if c.paired != Some(true) {
                continue;
            }
            let hint = c.tachs.first().and_then(|k| self.input.tachs.iter().find(|t| &t.key == k)).cloned();
            let firmware_stops = c.tachs.iter().any(|t| idle_stopped.get(t).is_some_and(|s| *s >= FIRMWARE_STOP_SECONDS));
            let top = c.rise.map(|r| r[1]);
            let header = self.header(c).unwrap_or_else(|| c.target.label.clone());
            let s = if is_nvml(c.target.kind) {
                Suggestion { role: Role::Gpu, reason: "An NVIDIA GPU fan.".into(), confirm: false, may_stop: false }
            } else if c.target.class == Class::Gpu {
                Suggestion { role: Role::Gpu, reason: "A graphics card's fan.".into(), confirm: false, may_stop: false }
            } else if c.target.class == Class::Laptop {
                // The embedded controller runs it until Penna takes it: one it was seen stopping may stop (§11.3 call 8).
                Suggestion {
                    role: Role::Laptop,
                    reason: if firmware_stops {
                        "A laptop's fan. Its firmware stops it when cool, so it may stop.".into()
                    } else {
                        "A laptop's fan.".into()
                    },
                    confirm: false,
                    may_stop: firmware_stops,
                }
            } else {
                match hint.as_ref().and_then(|h| h.role.as_deref()) {
                    // A liquid cooler's pump often sits on a CPU header, CPU_OPT above all: a fast fan there is suggested as a
                    // pump, and anything on CPU_OPT is held at 50 % or more until the user says what it is.
                    Some(h @ ("cpu" | "cpu-opt")) => match top {
                        Some(t) if t > PUMP_RPM => Suggestion {
                            role: Role::Pump,
                            reason: format!(
                                "The board calls this header {header}, and it turns {} RPM at 100 %: a pump, as liquid coolers often have \
                                 there. Held at {PUMP_LOWEST:.0} % or more until you confirm.",
                                rpm(t)
                            ),
                            confirm: true,
                            may_stop: false,
                        },
                        _ if h == "cpu-opt" => Suggestion {
                            role: Role::Cpu,
                            reason: format!(
                                "The board calls this header {header}: a second CPU fan, or a liquid cooler's pump. Held at \
                                 {PUMP_LOWEST:.0} % or more until you confirm."
                            ),
                            confirm: true,
                            may_stop: false,
                        },
                        _ => Suggestion {
                            role: Role::Cpu,
                            reason: format!("The board calls this header {header}."),
                            confirm: false,
                            may_stop: false,
                        },
                    },
                    Some("chipset") => Suggestion {
                        role: Role::Chipset,
                        reason: if firmware_stops {
                            format!("The board calls this header {header}. Its firmware stops it when cool, so it may stop.")
                        } else {
                            format!("The board calls this header {header}.")
                        },
                        confirm: false,
                        may_stop: firmware_stops,
                    },
                    Some("pump-header") => match top {
                        Some(t) if t > PUMP_RPM => Suggestion {
                            role: Role::Pump,
                            reason: format!("A pump header, and {} RPM at 100 %: a pump.", rpm(t)),
                            confirm: false,
                            may_stop: false,
                        },
                        Some(t) => Suggestion {
                            role: Role::Case,
                            reason: format!(
                                "On a pump header, but {} RPM at 100 %: a case fan. Held at {PUMP_LOWEST:.0} % or more until you confirm.",
                                rpm(t)
                            ),
                            confirm: true,
                            may_stop: false,
                        },
                        None => Suggestion { role: Role::Pump, reason: "A pump header.".into(), confirm: true, may_stop: false },
                    },
                    _ if hint.as_ref().is_some_and(|h| h.label != h.key.channel()) => {
                        Suggestion { role: Role::Case, reason: "A system fan header.".into(), confirm: false, may_stop: false }
                    }
                    _ => Suggestion {
                        role: Role::Case,
                        reason: "Not a CPU, pump or chipset header.".into(),
                        confirm: false,
                        may_stop: false,
                    },
                }
            };
            let c = &mut self.ctls[i];
            c.firmware_stops = firmware_stops;
            c.lowest = match (s.role, s.confirm, s.may_stop) {
                (Role::Pump, ..) | (_, true, _) => Some(PUMP_LOWEST),
                (Role::Chipset, _, false) => Some(CHIPSET_LOWEST),
                _ if is_nvml(c.target.kind) => match c.target.kind {
                    ControlKind::Nvml { min, .. } => Some(min as f64),
                    _ => None,
                },
                _ => None,
            };
            c.suggestion = Some(s);
        }
        self.enter(Step::Calibration);
        for c in &mut self.ctls {
            if c.paired == Some(true) {
                c.cal = Cal::Down { duty: 100.0, since: self.now };
                c.demand = Some(100.0);
                c.samples.clear();
                c.status = "stepping down · 100 %".to_owned();
            }
        }
        self.say = format!("Stepping every fan down from 100 % in {CAL_DOWN:.0} % steps, waiting for each to settle.");
    }

    fn calibration(&mut self, readings: &Readings, state: &LoopState) {
        let now = self.now;
        for c in &mut self.ctls {
            if c.paired != Some(true) || matches!(c.cal, Cal::Done | Cal::Waiting) {
                continue;
            }
            // A duty counts once the loop shows it written. A control setup has just asked for is taken on one tick and written
            // on the next, and until then its tach reads what its firmware runs.
            let asked = match c.cal {
                Cal::Down { since, .. } | Cal::Up { since, .. } | Cal::Spin { since, .. } => since,
                Cal::Waiting | Cal::Done => now,
            };
            let running = state
                .controls
                .get(&c.target.key)
                .is_some_and(|cs| cs.state == State::Measuring && c.demand.is_some_and(|d| (cs.duty - d).abs() < 0.5));
            if !running {
                c.samples.clear();
                if now - asked > CAL_SETTLE_MAX {
                    c.cal = Cal::Done;
                    c.demand = None;
                    c.status = "Penna couldn't take it from its firmware".to_owned();
                }
                continue;
            }
            let Some(r) = tach_rpm(readings, &c.tachs) else {
                c.status = "its tach didn't read".to_owned();
                continue;
            };
            c.samples.push((now, r));
            match c.cal {
                Cal::Waiting | Cal::Done => {}
                Cal::Down { duty, since } => {
                    let Some(settled) = settled(&c.samples, now - since) else { continue };
                    if settled < STOPPED_RPM {
                        // It stopped: the stop duty is the last that turned it. A stopped fan never starts below that, so the
                        // climb starts there, which keeps a CPU fan's stop short.
                        c.stop = c.table.first().map(|p| p[0]);
                        let up = step_up(c.target.kind, duty).unwrap_or(100.0);
                        let first = c.stop.unwrap_or(up).max(up);
                        c.cal = Cal::Up { duty: first, since: now, stopped_since: now };
                        c.demand = Some(first);
                        c.status = format!("stopped at {duty:.0} % · starting · {first:.0} %");
                        c.samples.clear();
                        continue;
                    }
                    if let Some(&[above, above_rpm]) = c.table.first()
                        && settled >= above_rpm * (1.0 + PAIR_RISE)
                        && settled - above_rpm >= PAIR_RISE_RPM
                    {
                        // Faster at a lower duty, by as much as pairing calls a rise: down here the header runs its fan at
                        // full speed, as this desk's SYS_FAN1 does at 0 %. Penna never runs it below the step above (§5.4).
                        c.lowest = Some(above);
                        c.note = Some(format!(
                            "At {duty:.0} % it turned faster, {} RPM, than at {above:.0} %: Penna never runs it below {above:.0} %.",
                            rpm(settled)
                        ));
                        c.cal = Cal::Done;
                        c.demand = Some(PAIR_HOLD.max(above));
                        c.status = format!("measured down to {above:.0} %");
                        continue;
                    }
                    c.table.insert(0, [duty, settled.round()]);
                    let next = step_down(c.target.kind, duty).filter(|n| *n >= c.lowest.unwrap_or(0.0) - 1e-9);
                    let Some(next) = next else {
                        c.cal = Cal::Done;
                        c.demand = Some(PAIR_HOLD.max(c.lowest.unwrap_or(0.0)));
                        c.status = match c.lowest {
                            Some(l) => format!("measured down to {l:.0} %"),
                            None => format!("still turning at {duty:.0} %"),
                        };
                        if c.lowest.is_none() {
                            c.stop = Some(duty);
                        }
                        continue;
                    };
                    c.cal = Cal::Down { duty: next, since: now };
                    c.demand = Some(next);
                    c.samples.clear();
                    c.status = format!("stepping down · {next:.0} %");
                }
                Cal::Up { duty, since, stopped_since } => {
                    if r >= STOPPED_RPM {
                        c.start = Some(duty);
                        c.cal = Cal::Spin { duty, since };
                        c.samples.clear();
                        c.samples.push((now, r));
                        c.status = format!("spinning up at {duty:.0} %");
                        continue;
                    }
                    let cap = if c.role() == Some(Role::Cpu) { CPU_STOPPED_MAX } else { STOPPED_MAX };
                    let up = step_up(c.target.kind, duty);
                    if now - stopped_since >= cap || up.is_none() {
                        // Never stopped longer than that: back up to the hold, the start duty unknown.
                        c.cal = Cal::Done;
                        c.demand = Some(PAIR_HOLD);
                        c.status = format!("didn't start by {duty:.0} %");
                        continue;
                    }
                    if now - since >= CAL_UP_WAIT
                        && let Some(next) = up
                    {
                        c.cal = Cal::Up { duty: next, since: now, stopped_since };
                        c.demand = Some(next);
                        c.status = format!("starting · {next:.0} %");
                    }
                }
                Cal::Spin { since, .. } => {
                    if settled(&c.samples, now - since).is_some() {
                        c.spin_up = Some(((now - since) * 10.0).round() / 10.0);
                        c.cal = Cal::Done;
                        c.demand = Some(PAIR_HOLD);
                        c.status = match (c.stop, c.start) {
                            (Some(s), Some(st)) => format!("stops below {s:.0} %, starts at {st:.0} %"),
                            _ => "measured".to_owned(),
                        };
                    }
                }
            }
        }
        let measuring: Vec<&Ctl> = self.ctls.iter().filter(|c| c.paired == Some(true)).collect();
        let done = measuring.iter().filter(|c| c.cal == Cal::Done).count();
        let starting = measuring.iter().find(|c| matches!(c.cal, Cal::Up { .. }));
        self.say = match starting {
            Some(c) => format!(
                "{} stopped below {:.0} %. Starting it again in {CAL_UP:.0} % steps{}",
                self.header(c).unwrap_or_else(|| c.target.label.clone()),
                c.stop.unwrap_or(0.0),
                if c.role() == Some(Role::Cpu) {
                    format!("; a CPU fan is never stopped for more than {CPU_STOPPED_MAX:.0} s.")
                } else {
                    ".".to_owned()
                }
            ),
            None => format!(
                "Stepping every fan down from 100 % in {CAL_DOWN:.0} % steps, waiting for each to settle · {done} of {} done",
                measuring.len()
            ),
        };
        if done == measuring.len() {
            for c in &mut self.ctls {
                c.demand = None;
            }
            if self.input.load_check.is_some() {
                self.enter(Step::LoadCheck);
                self.load = Some(Load {
                    pass: 1,
                    loading: true,
                    since: self.now,
                    temps: Vec::new(),
                    duties: Vec::new(),
                    earlier: 0.0,
                    adjust: false,
                    result: None,
                });
                self.say =
                    format!("Every CPU thread busy at the lowest priority for {}, on the proposed Balanced curves.", mmss(LOAD_SECONDS));
            } else {
                self.review();
            }
        }
    }

    fn review(&mut self) {
        self.enter(Step::Review);
        let load = self.load.as_ref().and_then(|l| l.result.as_ref()).map(|r| format!(" {}", load_words(r))).unwrap_or_default();
        self.say = format!("Done in {}.{load} Confirm the roles.", mmss(self.now - self.started));
    }

    /// The load check (§5.5): the daemon loads the CPU while [`Setup::load_wanted`] says so, with the proposed Balanced curves
    /// live. A pass ends after 3 minutes, or when the CPU passes its limit less 5 °C: then once [`Setup::take_adjust`] has moved
    /// the 100 % point, a second pass runs after the CPU has cooled.
    fn load_check(&mut self, readings: &Readings, state: &LoopState) {
        let Some(input) = self.input.load_check.clone() else { return self.review() };
        let now = self.now;
        let idle = self.idle_of(&input.key);
        let cpu_fans: Vec<f64> = self
            .ctls
            .iter()
            .filter(|c| matches!(c.role(), Some(Role::Cpu | Role::Laptop)))
            .filter_map(|c| state.controls.get(&c.target.key).filter(|cs| cs.held).map(|cs| cs.duty))
            .collect();
        let duty = (!cpu_fans.is_empty()).then(|| cpu_fans.iter().sum::<f64>() / cpu_fans.len() as f64);
        let t = readings.temps.get(&input.key).and_then(Sample::value);
        let Some(l) = self.load.as_mut() else { return self.review() };
        let near = input.limit - LOAD_MARGIN;
        if !l.loading {
            let cool = t.zip(idle).is_some_and(|(t, i)| t <= i + LOAD_COOL_ABOVE_IDLE);
            if cool || now - l.since >= LOAD_COOL_MAX {
                *l = Load { pass: 2, loading: true, since: now, temps: Vec::new(), duties: Vec::new(), result: None, adjust: false, ..*l };
                self.say = format!("Second pass: {}, with the 100 % point {:.0} °C earlier.", mmss(LOAD_SECONDS), l.earlier);
            } else {
                self.say = format!("Cooling before the second pass · {}", t.map_or("—".into(), |t| format!("{t:.0} °C")));
            }
            return;
        }
        if let Some(t) = t {
            l.temps.push((now, t));
        }
        if let Some(d) = duty {
            l.duties.push((now, d));
        }
        let hot = t.is_some_and(|t| t >= near);
        if hot && l.pass == 1 {
            l.loading = false;
            l.since = now;
            l.adjust = true;
            l.earlier += LOAD_MARGIN;
            self.say = format!(
                "The CPU reached {:.0} °C, within {LOAD_MARGIN:.0} °C of its limit: the 100 % point moves {LOAD_MARGIN:.0} °C earlier, \
                 and the check runs again once it has cooled.",
                t.unwrap_or(near)
            );
            return;
        }
        if hot || now - l.since >= LOAD_SECONDS {
            let last = |v: &[(f64, f64)]| {
                let tail: Vec<f64> = v.iter().filter(|(at, _)| now - at <= LOAD_PLATEAU).map(|(_, x)| *x).collect();
                (!tail.is_empty()).then(|| tail.iter().sum::<f64>() / tail.len() as f64)
            };
            let peak = l.temps.iter().map(|(_, t)| *t).fold(f64::NEG_INFINITY, f64::max);
            l.result = Some(LoadResult { plateau: last(&l.temps).unwrap_or(peak), fans: last(&l.duties), peak, earlier: l.earlier, hot });
            l.loading = false;
            return self.review();
        }
        self.say = format!(
            "Full CPU load, pass {} · {} · fans at {} · {} left",
            l.pass,
            t.map_or("—".into(), |t| format!("{t:.0} °C")),
            duty.map_or("—".into(), |d| format!("{d:.0} %")),
            mmss(LOAD_SECONDS - (now - l.since))
        );
    }

    /// The load check wants every CPU thread busy now.
    pub fn load_wanted(&self) -> bool {
        self.stopped.is_none() && self.step == Step::LoadCheck && self.load.as_ref().is_some_and(|l| l.loading)
    }

    /// Once, after a pass passed the limit less 5 °C: the proposed CPU curves' 100 % point should move this many °C earlier in
    /// all, before the second pass.
    pub fn take_adjust(&mut self) -> Option<f64> {
        let l = self.load.as_mut().filter(|l| l.adjust)?;
        l.adjust = false;
        Some(l.earlier)
    }

    /// How many °C earlier the load check moved the 100 % point.
    pub fn earlier(&self) -> f64 {
        self.load.as_ref().map_or(0.0, |l| l.earlier)
    }

    /// Measuring is over: what setup measured can be saved, and curves proposed (§5.5).
    pub fn measured_all(&self) -> bool {
        self.stopped.is_none() && self.step >= Step::LoadCheck
    }

    /// Where a sensor rests: [`IDLE_QUANTILE`] of the idle sample's readings.
    fn idle_of(&self, key: &Key) -> Option<f64> {
        let mut v = self.idle_temps.get(key).filter(|v| !v.is_empty())?.clone();
        v.sort_by(f64::total_cmp);
        Some(v[((v.len() - 1) as f64 * IDLE_QUANTILE).round() as usize])
    }

    /// The results, for `measured.toml`. The idle sample, the write test, pairing and calibration; roles stay suggestions until
    /// the user confirms them.
    pub fn measured(&self, unix: f64) -> Measured {
        let idle = self.idle_temps.keys().filter_map(|k| self.idle_of(k).map(|t| (k.clone(), (t * 10.0).round() / 10.0))).collect();
        let controls = self
            .ctls
            .iter()
            .filter(|c| c.write.is_some())
            .map(|c| {
                let mut m = MeasuredControl::new(c.target.key.clone());
                m.write = c.write;
                // NVML pairs itself (§5.2); a laptop's or a GPU's fan is paired by its driver, and keeps its tachs for the RPM.
                m.paired = !is_nvml(c.target.kind) && c.paired == Some(true);
                m.empty = !is_nvml(c.target.kind) && c.paired == Some(false);
                m.tachs = if is_nvml(c.target.kind) { Vec::new() } else { c.tachs.clone() };
                m.stop = c.stop;
                m.start = c.start;
                m.spin_up = c.spin_up;
                m.lowest = c.lowest;
                m.calibration = c.table.clone();
                m.firmware_stops = c.firmware_stops;
                m.suggested = c.suggestion.as_ref().map(|s| s.role);
                m.reason = c.suggested().map(|s| s.reason);
                m.role = c.confirmed;
                m
            })
            .collect();
        Measured { at: Some(unix), idle, controls }
    }

    pub fn progress(&self) -> Progress {
        let order = self.pair_order().len().max(self.ctls.iter().filter(|c| c.target.tachs.is_empty()).count());
        let steps = vec![
            StepProgress { step: Step::Idle, seconds: IDLE_SECONDS },
            StepProgress { step: Step::WriteTest, seconds: WRITE_SECONDS },
            StepProgress { step: Step::Pairing, seconds: PAIR_SETTLE + order as f64 * (PAIR_HIGH_SECONDS + PAIR_LOW_SECONDS) },
            StepProgress { step: Step::Roles, seconds: 0.0 },
            StepProgress { step: Step::Calibration, seconds: CAL_ESTIMATE },
        ];
        let mut steps = steps;
        if self.input.load_check.is_some() {
            steps.push(StepProgress { step: Step::LoadCheck, seconds: LOAD_SECONDS });
        }
        steps.push(StepProgress { step: Step::Review, seconds: 0.0 });
        let mut left = 0.0;
        for s in &steps {
            if s.step > self.step {
                left += s.seconds;
            } else if s.step == self.step {
                left += (s.seconds - (self.now - self.step_at)).max(0.0);
            }
        }
        if self.finished() {
            left = 0.0;
        }
        Progress {
            step: self.step,
            steps,
            elapsed: self.now - self.started,
            left,
            say: self.say.clone(),
            waiting: self.waiting.clone(),
            stopped: self.stopped.clone(),
            current: match (self.step, self.window) {
                (Step::Pairing, Some(w)) => self.pair_order().get(w).map(|&i| self.ctls[i].target.key.clone()),
                _ => None,
            },
            mover: if self.step == Step::Pairing { self.mover.clone() } else { None },
            controls: self
                .ctls
                .iter()
                .map(|c| ControlProgress {
                    key: c.target.key.clone(),
                    label: c.target.label.clone(),
                    header: self.header(c),
                    status: c.status.clone(),
                    duty: c.demand,
                    write: c.write,
                    write_detail: c.write_detail.clone(),
                    paired: c.paired,
                    tachs: c.tachs.clone(),
                    rise: c.rise,
                    suggestion: c.suggested(),
                    table: c.table.clone(),
                    stop: c.stop,
                    start: c.start,
                    spin_up: c.spin_up,
                    lowest: c.lowest,
                    note: c.note.clone(),
                })
                .collect(),
            guards: self
                .input
                .guards
                .iter()
                .map(|g| GuardProgress {
                    key: g.key.clone(),
                    label: g.label.clone(),
                    celsius: self.guards.get(&g.key).copied().flatten(),
                    limit: g.limit,
                })
                .collect(),
            load: self.load.as_ref().zip(self.input.load_check.as_ref()).map(|(l, input)| LoadProgress {
                source: Some(input.key.clone()),
                pass: l.pass,
                loading: l.loading,
                seconds: self.now - l.since,
                celsius: l.temps.last().map(|(_, t)| *t),
                fans: l.duties.last().map(|(_, d)| *d),
                limit: input.limit,
                result: l.result.clone(),
            }),
            proposed: None,
        }
    }
}

/// A load check's result in words (§5.5): "Under 3 minutes of full load the CPU levelled at 78 °C, with its fans at 64 %."
pub fn load_words(r: &LoadResult) -> String {
    let fans = r.fans.map_or(String::new(), |f| format!(", with its fans at {f:.0} %"));
    let earlier = if r.earlier > 0.0 { format!(" The 100 % point moved {:.0} °C earlier.", r.earlier) } else { String::new() };
    if r.hot {
        format!("Under full load the CPU reached {:.0} °C{fans}: its cooler is at its limit.{earlier}", r.peak)
    } else {
        format!("Under {} of full load the CPU levelled at {:.0} °C{fans}.{earlier}", minutes(LOAD_SECONDS), r.plateau)
    }
}

fn minutes(s: f64) -> String {
    let m = (s / 60.0).round() as u64;
    if m == 1 { "a minute".into() } else { format!("{m} minutes") }
}

fn is_nvml(kind: ControlKind) -> bool {
    matches!(kind, ControlKind::Nvml { .. })
}

enum Check {
    Ok,
    Reverted(String),
    Mapped(u32, u32),
    Unread(String),
}

/// What the control reads back after `duty` was written (§5.1).
fn readback(kind: ControlKind, reading: Option<&Sample<ControlReading>>, duty: f64) -> Check {
    let r = match reading {
        Some(Sample::Value(r)) => r,
        Some(Sample::Error(e)) => return Check::Unread(e.clone()),
        Some(Sample::Implausible(v)) => return Check::Unread(format!("read {v}")),
        None => return Check::Unread("not read".to_owned()),
    };
    match kind {
        ControlKind::Nvml { .. } => {
            if r.mode == ControlMode::Manual {
                Check::Ok
            } else {
                Check::Reverted("the driver took it back".to_owned())
            }
        }
        ControlKind::Duty { scale } => {
            let want = (duty * scale as f64 / 100.0 - 1e-9).ceil().clamp(0.0, scale as f64) as u32;
            // it87 and nct6775 read a manual channel at 100 % as "no control" (§3.5).
            let mode_ok = r.mode == ControlMode::Manual || (r.mode == ControlMode::Full && want >= scale);
            if !mode_ok {
                let mode = match r.mode {
                    ControlMode::Automatic(v) => format!("pwm_enable {v}, automatic"),
                    ControlMode::Full => "pwm_enable 0, full speed".to_owned(),
                    other => format!("{other:?}"),
                };
                return Check::Reverted(format!("its firmware took it back ({mode})"));
            }
            let tolerance = (scale as f64 * READBACK_TOLERANCE).round().max(1.0) as u32;
            match r.raw {
                Some(got) if got.abs_diff(want) > tolerance => Check::Mapped(want, got),
                _ => Check::Ok,
            }
        }
        ControlKind::Levels { steps, .. } => {
            if r.mode != ControlMode::Manual && !(r.mode == ControlMode::Full && level_of(steps, duty) + 1 >= steps) {
                return Check::Reverted("its firmware took it back".to_owned());
            }
            let want = level_of(steps, duty);
            match r.raw {
                Some(got) if got != want => Check::Mapped(want, got),
                _ => Check::Ok,
            }
        }
        ControlKind::Speed { min, max } => {
            if r.mode != ControlMode::Manual {
                return Check::Reverted("its firmware took it back".to_owned());
            }
            let want = crate::model::speed_rpm(min, max, duty);
            // A target comes back rounded: `lenovo_wmi_other` rounds down to 100 RPM.
            let tolerance = (max as f64 * READBACK_TOLERANCE).max(100.0) as u32;
            match r.raw {
                Some(got) if got.abs_diff(want) > tolerance => Check::Mapped(want, got),
                _ => Check::Ok,
            }
        }
        _ => Check::Unread("not a kind setup measures yet".to_owned()),
    }
}

/// The next duty down that calibration measures (§5.4): 5 % lower, or a stepped control's level below. `None` below 0.
fn step_down(kind: ControlKind, duty: f64) -> Option<f64> {
    match kind {
        ControlKind::Levels { steps, .. } => level_of(steps, duty).checked_sub(1).map(|l| level_duty(steps, l)),
        _ => Some(duty - CAL_DOWN).filter(|d| *d >= -1e-9),
    }
}

/// The next duty up while a stopped fan is started again: 2 % higher, or the level above. `None` past 100 %.
fn step_up(kind: ControlKind, duty: f64) -> Option<f64> {
    match kind {
        ControlKind::Levels { steps, .. } => Some(level_of(steps, duty) + 1).filter(|l| *l < steps).map(|l| level_duty(steps, l)),
        _ => Some(duty + CAL_UP).filter(|d| *d <= 100.0 + 1e-9),
    }
}

/// The mean of the paired tachs that read, or `None`.
fn tach_rpm(readings: &Readings, tachs: &[Key]) -> Option<f64> {
    let vals: Vec<f64> = tachs.iter().filter_map(|t| readings.rpms.get(t).and_then(Sample::value)).map(f64::from).collect();
    (!vals.is_empty()).then(|| vals.iter().sum::<f64>() / vals.len() as f64)
}

/// A steady reading once a duty has had time to settle (§5.4), judged on three readings `CAL_SPAN` apart, the latest last: all
/// within 3 % of their mean and the last two within 1.5 %, after at least 2 s. A fan still closing on its speed can pass the 3 %
/// while it reads low. After 10 s the latest reading counts whatever it is, unless the three move one way: then up to 30 s.
/// `samples` are (seconds, RPM), `held` the seconds since the duty was asked for.
fn settled(samples: &[(f64, f64)], held: f64) -> Option<f64> {
    let &(t, a) = samples.last()?;
    // Ticks wake a little late, by a different amount each time: a reading 1.9 s before is the one 2 s before.
    let before = |t: f64| samples.iter().rev().find(|(s, _)| *s <= t - CAL_SPAN + 0.1).copied();
    let Some([c, b]) = before(t).and_then(|(tb, b)| before(tb).map(|(_, c)| [c, b])) else {
        return (held >= CAL_SETTLE_MAX).then_some(a);
    };
    let mean = (a + b + c) / 3.0;
    let moving = ((c < b && b < a) || (c > b && b > a)) && (a - c).abs() > (a * CAL_DRIFT).max(CAL_STILL_RPM);
    let stable = if mean < STOPPED_RPM {
        [a, b, c].iter().all(|r| *r < STOPPED_RPM)
    } else {
        !moving && [a, b, c].iter().all(|r| (r - mean).abs() <= mean * CAL_STABLE) && (a - b).abs() <= (a * CAL_STILL).max(CAL_STILL_RPM)
    };
    let done = (stable && held >= CAL_SETTLE_MIN) || (held >= CAL_SETTLE_MAX && !moving) || held >= CAL_DRIFT_MAX;
    done.then_some(a)
}

fn rpm(v: f64) -> String {
    let n = (v / 10.0).round() as i64 * 10;
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn mmss(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Readings a second apart from `t`.
    fn tach(t: f64, rpms: &[f64]) -> Vec<(f64, f64)> {
        rpms.iter().enumerate().map(|(i, r)| (t + i as f64, *r)).collect()
    }

    #[test]
    fn settling_waits_for_three_close_readings_2_s_apart() {
        assert_eq!(settled(&tach(1.0, &[1000.0, 1000.0, 1000.0]), 3.0), None, "three readings 2 s apart first");
        assert_eq!(settled(&tach(1.0, &[900.0, 1000.0, 1010.0, 1005.0, 1000.0]), 1.0), None, "two seconds first");
        assert_eq!(settled(&tach(1.0, &[1000.0, 1000.0, 1010.0, 1010.0, 995.0]), 5.0), Some(995.0));
        assert_eq!(settled(&tach(1.0, &[1500.0, 1400.0, 1200.0, 1100.0, 1000.0]), 5.0), None, "still falling");
        assert_eq!(settled(&tach(1.0, &[1537.0, 1560.0, 1586.0, 1600.0, 1616.0]), 5.0), None, "within 3 %, but still rising");
        assert_eq!(settled(&tach(1.0, &[0.0, 0.0, 0.0, 0.0, 0.0]), 5.0), Some(0.0));
        // it87 on a 1 s tick: each reading twice. The last two are equal, and the fan is still falling.
        let sys4 = tach(1.0, &[1607.0, 1607.0, 2149.0, 2149.0, 2045.0, 2045.0, 1934.0, 1934.0, 1854.0, 1854.0]);
        assert_eq!(settled(&sys4[..9], 9.0), None, "a repeat isn't a fan that stopped moving");
        assert_eq!(settled(&sys4, 10.0), None, "still coming back from its overshoot at 10 s: it gets longer");
        assert_eq!(settled(&sys4, 30.0), Some(1854.0), "after 30 s, whatever it reads");
        let noisy = tach(1.0, &[1500.0, 1400.0, 1560.0, 1420.0, 1550.0, 1410.0, 1540.0, 1430.0, 1560.0, 1400.0]);
        assert_eq!(settled(&noisy, 10.0), Some(1400.0), "one that only wanders counts at 10 s");
    }

    #[test]
    fn rpm_is_rounded_to_tens_with_thousands() {
        assert_eq!(rpm(1562.0), "1,560");
        assert_eq!(rpm(5625.0), "5,630");
        assert_eq!(rpm(940.0), "940");
    }

    #[test]
    fn roles_read_their_names() {
        assert_eq!(Role::parse("CPU cooler"), Some(Role::Cpu));
        assert_eq!(Role::parse(" case "), Some(Role::Case));
        assert_eq!(Role::parse("blower"), None);
    }
}
