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
use crate::{ControlKind, ControlMode, ControlReading, Key, Readings, Sample};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The idle sample (§5, step 1).
pub const IDLE_SECONDS: f64 = 30.0;
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
/// Stable: the last three readings within this share of their mean, and the last two this close.
pub const CAL_STABLE: f64 = 0.03;
pub const CAL_STILL: f64 = 0.015;
pub const CAL_STILL_RPM: f64 = 20.0;
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
const CAL_ESTIMATE: f64 = 90.0;
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
    samples: Vec<f64>,
    table: Vec<[f64; 2]>,
    stop: Option<f64>,
    start: Option<f64>,
    spin_up: Option<f64>,
    lowest: Option<f64>,
    firmware_stops: bool,
    demand: Option<f64>,
    status: String,
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
    idle_sum: BTreeMap<Key, (f64, u32)>,
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
                firmware_stops: false,
                demand: None,
                status: if is_nvml(t.kind) { "Driver".to_owned() } else { "BIOS".to_owned() },
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
            idle_sum: BTreeMap::new(),
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
        }
    }

    /// Calibration alone, for fans whose pairing is known and whose role the user has just confirmed (§11.3 call 7): a case
    /// fan on a pump header, measured below the 50 % it was held to. `known` is each control with its tachs and role.
    pub fn calibrate(input: Input, known: &[(Key, Vec<Key>, Role)], now: f64) -> Setup {
        let mut s = Setup::new(input, now);
        s.ctls.retain(|c| known.iter().any(|(k, ..)| k == &c.target.key));
        for c in &mut s.ctls {
            let (_, tachs, role) = known.iter().find(|(k, ..)| k == &c.target.key).expect("kept above");
            c.write = Some(WriteResult::Ok);
            c.paired = Some(true);
            c.tachs = tachs.clone();
            c.lowest = match role {
                Role::Pump => Some(PUMP_LOWEST),
                Role::Chipset => Some(CHIPSET_LOWEST),
                _ => None,
            };
            c.suggestion = Some(Suggestion { role: *role, reason: "Confirmed by you.".into(), confirm: false, may_stop: false });
        }
        s.enter(Step::Calibration);
        for c in &mut s.ctls {
            c.cal = Cal::Down { duty: 100.0, since: now };
            c.demand = Some(100.0);
            c.status = "stepping down · 100 %".to_owned();
        }
        s.say = format!("Stepping down from 100 % in {CAL_DOWN:.0} % steps, waiting for each to settle.");
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
                Step::Calibration => self.calibration(readings),
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
        for g in self.input.guards.clone() {
            match readings.temps.get(&g.key) {
                Some(Sample::Value(v)) => {
                    self.guards.insert(g.key.clone(), Some(*v));
                    if *v >= g.limit {
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
            self.idle_sum.clear();
            self.idle_stopped.clear();
            self.last_idle = None;
            return;
        }
        self.waiting = None;
        let dt = self.last_idle.map_or(0.0, |t| self.now - t);
        self.last_idle = Some(self.now);
        for (k, s) in &readings.temps {
            if let Sample::Value(v) = s {
                let e = self.idle_sum.entry(k.clone()).or_insert((0.0, 0));
                e.0 += v;
                e.1 += 1;
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
            let v = now - c.write_at.unwrap_or(now);
            // This tick's readings show what was written a tick ago.
            let asked = if v < 1.0 {
                None
            } else if v < 2.0 {
                Some(WRITE_FIRST)
            } else {
                Some(WRITE_SECOND)
            };
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
            if v >= 1.0 {
                c.demand = Some(WRITE_SECOND);
            }
            c.status = match asked {
                None => format!("writing {WRITE_FIRST:.0} %"),
                Some(WRITE_FIRST) => format!("{WRITE_FIRST:.0} % read back · writing {WRITE_SECOND:.0} %"),
                Some(_) => format!("{WRITE_SECOND:.0} % read back · watching"),
            };
            if v >= WRITE_SECONDS {
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
                if is_nvml(c.target.kind) && c.write.is_some_and(WriteResult::offered) {
                    // NVML pairs control i with its own tach (§5.2).
                    c.tachs = vec![c.target.key.clone()];
                    c.paired = Some(true);
                }
            }
            self.say = format!("Holding every fan at {PAIR_HOLD:.0} % so each rise is measured from a steady speed.");
        }
    }

    /// The hwmon controls pairing steps through, in order.
    fn pair_order(&self) -> Vec<usize> {
        (0..self.ctls.len())
            .filter(|&i| !is_nvml(self.ctls[i].target.kind) && self.ctls[i].write.is_some_and(WriteResult::offered))
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
        if is_nvml(c.target.kind) {
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

    fn calibration(&mut self, readings: &Readings) {
        let now = self.now;
        for c in &mut self.ctls {
            if c.paired != Some(true) || c.cal == Cal::Done {
                continue;
            }
            let Some(r) = tach_rpm(readings, &c.tachs) else {
                c.status = "its tach didn't read".to_owned();
                continue;
            };
            c.samples.push(r);
            match c.cal {
                Cal::Waiting | Cal::Done => {}
                Cal::Down { duty, since } => {
                    let Some(settled) = settled(&c.samples, now - since) else { continue };
                    if settled < STOPPED_RPM {
                        // It stopped: the stop duty is the last that turned it. A stopped fan never starts below that, so the
                        // climb starts there, which keeps a CPU fan's stop short.
                        c.stop = c.table.first().map(|p| p[0]);
                        let first = c.stop.unwrap_or(duty + CAL_UP).max(duty + CAL_UP);
                        c.cal = Cal::Up { duty: first, since: now, stopped_since: now };
                        c.demand = Some(first);
                        c.status = format!("stopped at {duty:.0} % · starting · {first:.0} %");
                        c.samples.clear();
                        continue;
                    }
                    c.table.insert(0, [duty, settled.round()]);
                    let next = duty - CAL_DOWN;
                    if next < c.lowest.unwrap_or(0.0) - 1e-9 || next < 0.0 {
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
                    }
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
                        c.samples.push(r);
                        c.status = format!("spinning up at {duty:.0} %");
                        continue;
                    }
                    let cap = if c.suggestion.as_ref().is_some_and(|s| s.role == Role::Cpu) { CPU_STOPPED_MAX } else { STOPPED_MAX };
                    if now - stopped_since >= cap || duty + CAL_UP > 100.0 {
                        // Never stopped longer than that: back up to the hold, the start duty unknown.
                        c.cal = Cal::Done;
                        c.demand = Some(PAIR_HOLD);
                        c.status = format!("didn't start by {duty:.0} %");
                        continue;
                    }
                    if now - since >= CAL_UP_WAIT {
                        let next = duty + CAL_UP;
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
                if c.suggestion.as_ref().is_some_and(|s| s.role == Role::Cpu) {
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
            self.enter(Step::Review);
            for c in &mut self.ctls {
                c.demand = None;
            }
            self.say = format!("Done in {}. Confirm the roles.", mmss(self.now - self.started));
        }
    }

    /// The results, for `measured.toml`. The idle sample, the write test, pairing and calibration; roles stay suggestions until
    /// the user confirms them.
    pub fn measured(&self, unix: f64) -> Measured {
        let idle = self
            .idle_sum
            .iter()
            .filter(|(_, (_, n))| *n > 0)
            .map(|(k, (s, n))| (k.clone(), (s / *n as f64 * 10.0).round() / 10.0))
            .collect();
        let controls = self
            .ctls
            .iter()
            .filter(|c| c.write.is_some())
            .map(|c| {
                let mut m = MeasuredControl::new(c.target.key.clone());
                m.write = c.write;
                // NVML pairs itself (§5.2); for hwmon, pairing ran, so no tachs means no fan on the header.
                m.paired = !is_nvml(c.target.kind) && c.paired.is_some();
                m.tachs = if is_nvml(c.target.kind) { Vec::new() } else { c.tachs.clone() };
                m.stop = c.stop;
                m.start = c.start;
                m.spin_up = c.spin_up;
                m.lowest = c.lowest;
                m.calibration = c.table.clone();
                m.firmware_stops = c.firmware_stops;
                m.suggested = c.suggestion.as_ref().map(|s| s.role);
                m.reason = c.suggestion.as_ref().map(|s| s.reason.clone());
                m
            })
            .collect();
        Measured { at: Some(unix), idle, controls }
    }

    pub fn progress(&self) -> Progress {
        let order = self.pair_order().len().max(self.ctls.iter().filter(|c| !is_nvml(c.target.kind)).count());
        let steps = vec![
            StepProgress { step: Step::Idle, seconds: IDLE_SECONDS },
            StepProgress { step: Step::WriteTest, seconds: WRITE_SECONDS },
            StepProgress { step: Step::Pairing, seconds: PAIR_SETTLE + order as f64 * (PAIR_HIGH_SECONDS + PAIR_LOW_SECONDS) },
            StepProgress { step: Step::Roles, seconds: 0.0 },
            StepProgress { step: Step::Calibration, seconds: CAL_ESTIMATE },
            StepProgress { step: Step::Review, seconds: 0.0 },
        ];
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
                    suggestion: c.suggestion.clone(),
                    table: c.table.clone(),
                    stop: c.stop,
                    start: c.start,
                    spin_up: c.spin_up,
                    lowest: c.lowest,
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
        }
    }
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
        _ => Check::Unread("not a kind setup measures yet".to_owned()),
    }
}

/// The mean of the paired tachs that read, or `None`.
fn tach_rpm(readings: &Readings, tachs: &[Key]) -> Option<f64> {
    let vals: Vec<f64> = tachs.iter().filter_map(|t| readings.rpms.get(t).and_then(Sample::value)).map(f64::from).collect();
    (!vals.is_empty()).then(|| vals.iter().sum::<f64>() / vals.len() as f64)
}

/// A steady reading once a duty has had time to settle (§5.4): the last three within 3 % of their mean and no longer moving,
/// after at least 2 s, or after 10 s whatever they do; then the latest of them. A fan still closing on its speed can pass the 3 %
/// while it reads low.
fn settled(samples: &[f64], held: f64) -> Option<f64> {
    if held < CAL_SETTLE_MIN || samples.len() < 3 {
        return None;
    }
    let last = &samples[samples.len() - 3..];
    let mean = last.iter().sum::<f64>() / 3.0;
    let stable = if mean < STOPPED_RPM {
        last.iter().all(|r| *r < STOPPED_RPM)
    } else {
        last.iter().all(|r| (r - mean).abs() <= mean * CAL_STABLE) && (last[2] - last[1]).abs() <= (last[2] * CAL_STILL).max(CAL_STILL_RPM)
    };
    (stable || held >= CAL_SETTLE_MAX).then_some(last[2])
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

    #[test]
    fn settling_waits_for_three_close_readings_or_ten_seconds() {
        assert_eq!(settled(&[1000.0, 1000.0], 3.0), None, "three readings first");
        assert_eq!(settled(&[900.0, 1000.0, 1010.0], 1.0), None, "two seconds first");
        assert_eq!(settled(&[1500.0, 1200.0, 1000.0], 3.0), None, "still falling");
        assert_eq!(settled(&[1537.0, 1586.0, 1616.0], 5.0), None, "within 3 %, but still rising");
        assert_eq!(settled(&[1000.0, 1010.0, 995.0], 3.0), Some(995.0));
        assert_eq!(settled(&[1500.0, 1200.0, 1000.0], 10.0), Some(1000.0), "after 10 s, whatever it reads");
        assert_eq!(settled(&[0.0, 0.0, 0.0], 2.0), Some(0.0));
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
