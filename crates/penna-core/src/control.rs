//! The control loop's pure heart (DESIGN.md §4, §6.3, §6.4, §7.1). [`tick`] turns one pass of readings into commands;
//! [`LoopState::record`] takes back what the hardware said about each. Time is seconds on a monotonic clock, so tests run on
//! simulated time and the daemon on real time with the same code.
use crate::config::{CurveKind, DEFAULTS, HysteresisDirection, MixOp, VirtualKind};
use crate::plan::{ControlPlan, CurvePlan, Plan, RESUME_MARGIN};
use crate::{ControlKind, ControlMode, ControlReading, Key, Readings, Sample};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// How long a stopped fan is kicked at its start duty (§4.5).
pub const KICK_SECONDS: f64 = 2.0;
/// An emergency ends when the sensor is this far below its limit (§6.3).
pub const EMERGENCY_RELEASE: f64 = 5.0;
/// Read-back must match for this many ticks running before a control counts as overridden (§6.4).
pub const MISMATCH_TICKS: u32 = 2;
/// Re-assertions allowed in [`REASSERT_WINDOW`] before a control is contested (§6.4).
pub const MAX_REASSERTS: usize = 3;
pub const REASSERT_WINDOW: f64 = 30.0;
/// Duty read-back tolerance, as a share of the control's scale: 2 %, which is 5 of 255 (§6.4).
pub const READBACK_TOLERANCE: f64 = 0.02;
/// A smoothed value older than this starts again from the next reading rather than gliding from the past.
const SMOOTH_RESET: f64 = 10.0;

/// The states every surface renders (§4.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Firmware,
    Curve,
    /// Setup is measuring it (§5): it runs at the duty setup asks for. Only an emergency outranks that.
    Measuring,
    Handback,
    Override,
    Failsafe,
    Emergency,
    Contested,
    Absent,
    Fault,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Firmware => "firmware",
            State::Curve => "curve",
            State::Measuring => "measuring",
            State::Handback => "handback",
            State::Override => "override",
            State::Failsafe => "failsafe",
            State::Emergency => "emergency",
            State::Contested => "contested",
            State::Absent => "absent",
            State::Fault => "fault",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trend {
    #[default]
    Steady,
    /// Held back by the step-up limit.
    Rising,
    /// Held back by the step-down limit.
    Falling,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Leave firmware: the daemon writes the way back to `held.json` first (§6.1), then switches to manual without a jump.
    Take {
        key: Key,
    },
    Write {
        key: Key,
        raw: u32,
    },
    /// Switch to manual again and write: something changed the control under us (§6.4, §6.5).
    Reassert {
        key: Key,
        raw: u32,
    },
    /// NVML below its floor: the driver's curve runs the fan, and Penna still holds it (§4.5).
    Handback {
        key: Key,
    },
    /// Restore the original mode and stop holding the control.
    Release {
        key: Key,
    },
}

impl Command {
    pub fn key(&self) -> &Key {
        match self {
            Command::Take { key }
            | Command::Write { key, .. }
            | Command::Reassert { key, .. }
            | Command::Handback { key }
            | Command::Release { key } => key,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// A take succeeded; `duty` is what the firmware was running, so the ramp starts there.
    Taken {
        duty: f64,
    },
    Done,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    All,
    Control(Key),
}

/// Something a person asked for, applied between ticks.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Profile(String),
    /// `duty: None` clears the override. `seconds: None` holds until cleared.
    Override {
        scope: Scope,
        duty: Option<f64>,
        seconds: Option<f64>,
    },
    /// Every fan at 100 % until turned off (design review, decision 4).
    FullSpeed(bool),
    /// Try a contested or faulted control again.
    Retry(Key),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Override {
    pub duty: f64,
    pub until: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlState {
    pub state: State,
    /// Why the control is in a safety state, in words.
    pub reason: Option<String>,
    /// Penna holds it: it is in `held.json`.
    pub held: bool,
    /// NVML only: on the driver's policy while held.
    pub handback: bool,
    /// The duty Penna is giving the fan now, after limits and step rates.
    pub duty: f64,
    /// Where the step rates are taking it.
    pub target: f64,
    pub trend: Trend,
    /// The raw value last written and not since overridden.
    pub written: Option<u32>,
    pub kick_until: Option<f64>,
    pub mismatch: u32,
    pub reasserts: Vec<f64>,
    force_reassert: bool,
    taking: bool,
    releasing: bool,
    /// A contested control's reason, kept while an emergency outranks the contest.
    parked: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CurveState {
    pub smoothed: Option<f64>,
    smoothed_at: f64,
    pub out: Option<f64>,
    pub anchor: f64,
    /// Hysteresis is holding the output above (or below) the curve.
    pub held: bool,
    /// Trigger only.
    pub loaded: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoopState {
    pub seq: u64,
    pub last: Option<f64>,
    pub profile: String,
    pub full_speed: bool,
    pub override_all: Option<Override>,
    pub overrides: BTreeMap<Key, Override>,
    /// Every source this tick: physical sensors and virtual ones, value or why not.
    pub sources: BTreeMap<Key, Result<f64, String>>,
    windows: BTreeMap<Key, VecDeque<(f64, f64)>>,
    pub curves: BTreeMap<String, CurveState>,
    /// Curve id → its output this tick, or why there is none.
    pub outputs: BTreeMap<String, Result<f64, String>>,
    pub controls: BTreeMap<Key, ControlState>,
    /// Sensors in emergency, with the limit they crossed.
    pub emergency: BTreeMap<Key, f64>,
    /// The duty setup asks of each control it measures (§5), set by the daemon before every tick. A control here is taken whether
    /// or not a profile adopts it, and runs at exactly this duty, below its floor too.
    pub measuring: BTreeMap<Key, f64>,
}

impl LoopState {
    pub fn new(plan: &Plan) -> LoopState {
        LoopState { profile: plan.config.profile.clone(), ..LoopState::default() }
    }

    pub fn apply(&mut self, plan: &Plan, request: Request, now: f64) -> Result<(), String> {
        match request {
            Request::Profile(p) => {
                if p != DEFAULTS && !plan.config.profiles.contains_key(&p) {
                    return Err(format!("there is no profile {p:?}"));
                }
                self.profile = p;
            }
            Request::FullSpeed(on) => self.full_speed = on,
            Request::Override { scope, duty, seconds } => {
                let o = duty.map(|d| Override { duty: d.clamp(0.0, 100.0), until: seconds.map(|s| now + s) });
                match scope {
                    Scope::All => self.override_all = o,
                    Scope::Control(k) => {
                        if !plan.controls.contains_key(&k) {
                            return Err(format!("{k} is not a control Penna knows"));
                        }
                        match o {
                            Some(o) => self.overrides.insert(k, o),
                            None => self.overrides.remove(&k),
                        };
                    }
                }
            }
            Request::Retry(k) => {
                let cs = self.controls.get_mut(&k).ok_or_else(|| format!("{k} is not a control Penna knows"))?;
                match cs.state {
                    State::Contested | State::Fault => {
                        cs.mismatch = 0;
                        cs.reasserts.clear();
                        cs.reason = None;
                        if cs.held {
                            cs.state = State::Curve;
                            cs.force_reassert = !cs.handback;
                        } else {
                            cs.state = State::Firmware;
                        }
                    }
                    _ => return Err(format!("{k} is {}, there is nothing to retry", cs.state.name())),
                }
            }
        }
        Ok(())
    }

    /// What the hardware said about a command. A failed write or hand-back asks for the control to be released at once.
    pub fn record(&mut self, plan: &Plan, cmd: &Command, outcome: &Outcome) -> Option<Command> {
        let key = cmd.key();
        let cs = self.controls.entry(key.clone()).or_default();
        let nvml = plan.controls.get(key).is_some_and(ControlPlan::is_nvml);
        match (cmd, outcome) {
            (Command::Take { .. }, Outcome::Taken { duty }) => {
                cs.taking = false;
                cs.held = true;
                cs.state = State::Curve;
                cs.mismatch = 0;
                cs.reasserts.clear();
                cs.duty = *duty;
                cs.target = *duty;
                if nvml {
                    // Taking an NVML fan changes nothing: it stays on the driver's curve until the first write.
                    cs.handback = true;
                    cs.written = None;
                    cs.state = State::Handback;
                } else {
                    cs.handback = false;
                    cs.written = plan.controls.get(key).map(|c| c.raw(*duty));
                }
                // Taken in the middle of an emergency or a failed source: do not wait a tick to be safe.
                let c = plan.controls.get(key)?;
                let failed = plan.assignment(&self.profile, key).is_some_and(|id| self.outputs.get(id).is_some_and(Result::is_err));
                let (state, duty) = if !self.emergency.is_empty() {
                    (State::Emergency, 100.0)
                } else if failed {
                    (State::Failsafe, c.failsafe)
                } else {
                    return None;
                };
                cs.state = state;
                cs.duty = duty;
                cs.target = duty;
                Some(Command::Write { key: key.clone(), raw: c.raw(duty) })
            }
            (Command::Take { .. }, Outcome::Done) => {
                cs.taking = false;
                cs.state = State::Fault;
                cs.reason = Some("taking control reported no duty".to_owned());
                None
            }
            (Command::Take { .. }, Outcome::Failed(e)) => {
                cs.taking = false;
                cs.state = State::Fault;
                cs.reason = Some(format!("could not take control: {e}"));
                None
            }
            (Command::Write { raw, .. } | Command::Reassert { raw, .. }, Outcome::Done | Outcome::Taken { .. }) => {
                cs.written = Some(*raw);
                cs.handback = false;
                None
            }
            (Command::Handback { .. }, Outcome::Done | Outcome::Taken { .. }) => {
                cs.handback = true;
                cs.written = None;
                None
            }
            (Command::Write { .. } | Command::Reassert { .. } | Command::Handback { .. }, Outcome::Failed(e)) => {
                cs.state = State::Fault;
                cs.reason = Some(format!("write failed: {e}"));
                cs.releasing = true;
                Some(Command::Release { key: key.clone() })
            }
            (Command::Release { .. }, Outcome::Done | Outcome::Taken { .. }) => {
                cs.releasing = false;
                cs.held = false;
                cs.handback = false;
                cs.written = None;
                if !matches!(cs.state, State::Fault | State::Contested) {
                    cs.state = State::Firmware;
                }
                None
            }
            (Command::Release { .. }, Outcome::Failed(e)) => {
                cs.releasing = false;
                cs.state = State::Fault;
                cs.reason = Some(match cs.reason.take() {
                    Some(r) => format!("{r}; could not hand back: {e}"),
                    None => format!("could not hand back: {e}"),
                });
                None
            }
        }
    }
}

/// One tick. Pure: the same plan, state, readings and time always give the same commands.
pub fn tick(plan: &Plan, prev: &LoopState, readings: &Readings, now: f64) -> (LoopState, Vec<Command>) {
    let mut st = prev.clone();
    let dt = prev.last.map_or(0.0, |l| (now - l).clamp(0.0, 10.0));
    st.seq += 1;
    st.last = Some(now);
    if st.override_all.is_some_and(|o| o.until.is_some_and(|u| now >= u)) {
        st.override_all = None;
    }
    st.overrides.retain(|_, o| o.until.is_none_or(|u| now < u));

    resolve_sources(plan, &mut st, readings, now);
    update_emergency(plan, &mut st);
    for c in &plan.curves {
        let out = eval_curve(c, &mut st, now);
        st.outputs.insert(c.id.clone(), out);
    }

    let mut cmds = Vec::new();
    let emergency = !st.emergency.is_empty();
    for (key, c) in &plan.controls {
        let measuring = st.measuring.get(key).copied();
        let curve = plan.assignment(&st.profile, key);
        let output = match measuring {
            Some(d) => Some(Ok(d)),
            None => curve.map(|id| st.outputs.get(id).cloned().unwrap_or_else(|| Err(format!("curve {id:?} has no output")))),
        };
        let ov = if measuring.is_some() {
            None
        } else if st.full_speed {
            Some(100.0)
        } else {
            st.overrides.get(key).or(st.override_all.as_ref()).map(|o| o.duty)
        };
        let cs = st.controls.entry(key.clone()).or_default();
        let reason = if emergency { st.emergency.iter().next().map(|(k, l)| format!("{k} at or above {l} °C")) } else { None };
        step_control(c, cs, output, readings.controls.get(key), reason, ov, measuring.is_some(), now, dt, &mut cmds);
    }
    (st, cmds)
}

/// Every source the loop reads, this tick. A sensor that is missing, unreadable or implausible is a failed read at once: the
/// daemon reads every source on every tick, so there is no older value to fall back on (§6.3).
fn resolve_sources(plan: &Plan, st: &mut LoopState, readings: &Readings, now: f64) {
    st.sources.clear();
    for key in &plan.physical {
        let v = match readings.temps.get(key) {
            Some(Sample::Value(v)) => Ok(*v),
            Some(Sample::Implausible(v)) => Err(format!("{key} read {v}, which is not a temperature")),
            Some(Sample::Error(e)) => Err(format!("{key}: {e}")),
            None => Err(format!("{key} was not read")),
        };
        st.sources.insert(key.clone(), v);
    }
    for (key, kind) in &plan.virtuals {
        let get = |k: &Key, st: &LoopState| st.sources.get(k).cloned().unwrap_or_else(|| Err(format!("{k} was not read")));
        let v = match kind {
            VirtualKind::Max(ins) | VirtualKind::Min(ins) | VirtualKind::Average(ins) => {
                let vals: Result<Vec<f64>, String> = ins.iter().map(|k| get(k, st)).collect();
                vals.map(|v| match kind {
                    VirtualKind::Max(_) => v.iter().copied().fold(f64::MIN, f64::max),
                    VirtualKind::Min(_) => v.iter().copied().fold(f64::MAX, f64::min),
                    _ => v.iter().sum::<f64>() / v.len() as f64,
                })
            }
            VirtualKind::TimeAverage { source, seconds } => {
                let window = st.windows.entry(key.clone()).or_default();
                let cur = st.sources.get(source).cloned().unwrap_or_else(|| Err(format!("{source} was not read")));
                if let Ok(v) = cur {
                    window.push_back((now, v));
                }
                while window.front().is_some_and(|(t, _)| now - t > *seconds) {
                    window.pop_front();
                }
                cur.map(|_| window.iter().map(|(_, v)| v).sum::<f64>() / window.len() as f64)
            }
        };
        st.sources.insert(key.clone(), v.map_err(|e| format!("{key}: {e}")));
    }
}

fn update_emergency(plan: &Plan, st: &mut LoopState) {
    for (key, limit) in &plan.emergency {
        match st.sources.get(key) {
            Some(Ok(v)) if *v >= *limit => {
                st.emergency.insert(key.clone(), *limit);
            }
            Some(Ok(v)) if *v < limit - EMERGENCY_RELEASE => {
                st.emergency.remove(key);
            }
            // A failed read keeps whatever it was: an emergency does not end because the sensor went quiet.
            _ => {}
        }
    }
}

fn eval_curve(c: &CurvePlan, st: &mut LoopState, now: f64) -> Result<f64, String> {
    let source = |k: &Key, st: &LoopState| st.sources.get(k).cloned().unwrap_or_else(|| Err(format!("{k} was not read")));
    match &c.kind {
        CurveKind::Flat { percent } => Ok(*percent),
        CurveKind::Mix { op, curves } => {
            let vals: Result<Vec<f64>, String> =
                curves.iter().map(|id| st.outputs.get(id).cloned().unwrap_or_else(|| Err(format!("curve {id:?} has no output")))).collect();
            vals.map(|v| match op {
                MixOp::Max => v.iter().copied().fold(f64::MIN, f64::max),
                MixOp::Min => v.iter().copied().fold(f64::MAX, f64::min),
                MixOp::Average => v.iter().sum::<f64>() / v.len() as f64,
            })
        }
        CurveKind::Graph { source: k, points } => {
            let raw = source(k, st);
            let cs = st.curves.entry(c.id.clone()).or_default();
            let s = match raw {
                Ok(v) => smooth(cs, v, c.response, now),
                Err(e) => {
                    cs.smoothed = None;
                    cs.out = None;
                    return Err(e);
                }
            };
            let f = graph(points, s);
            match cs.out {
                None => {
                    cs.out = Some(f);
                    cs.anchor = s;
                    cs.held = false;
                }
                Some(out) => {
                    // Falling, past the band the output follows the curve H °C behind the temperature, not the curve itself:
                    // dropping the band's worth at once overcools, the temperature climbs back, and under a steady load the fans
                    // hunt (§4.4). Both ways, it moves in steps of the band.
                    let h = c.hysteresis;
                    let next = match c.direction {
                        HysteresisDirection::Falling if f >= out => Some((f, s)),
                        HysteresisDirection::Falling if s <= cs.anchor - h => Some((graph(points, s + h).clamp(f, out), s + h)),
                        HysteresisDirection::Both if (f > out && s >= cs.anchor + h) || (f < out && s <= cs.anchor - h) || f == out => {
                            Some((f, s))
                        }
                        _ => None,
                    };
                    match next {
                        Some((o, anchor)) => {
                            cs.out = Some(o);
                            cs.anchor = anchor;
                            cs.held = false;
                        }
                        None => cs.held = true,
                    }
                }
            }
            Ok(cs.out.expect("set above"))
        }
        CurveKind::Trigger { source: k, idle, load, on, off } => {
            let raw = source(k, st);
            let cs = st.curves.entry(c.id.clone()).or_default();
            let s = match raw {
                Ok(v) => smooth(cs, v, c.response, now),
                Err(e) => {
                    cs.smoothed = None;
                    return Err(e);
                }
            };
            if s >= *on {
                cs.loaded = true;
            } else if s <= *off {
                cs.loaded = false;
            }
            let out = if cs.loaded { *load } else { *idle };
            cs.out = Some(out);
            Ok(out)
        }
    }
}

fn smooth(cs: &mut CurveState, v: f64, tau: f64, now: f64) -> f64 {
    let s = match cs.smoothed {
        Some(prev) if tau > 0.0 && now - cs.smoothed_at <= SMOOTH_RESET => {
            prev + (v - prev) * (1.0 - (-(now - cs.smoothed_at) / tau).exp())
        }
        _ => v,
    };
    cs.smoothed = Some(s);
    cs.smoothed_at = now;
    s
}

/// Linear between points, flat beyond the ends.
pub fn graph(points: &[[f64; 2]], x: f64) -> f64 {
    let Some(first) = points.first() else { return 100.0 };
    if x <= first[0] {
        return first[1];
    }
    for w in points.windows(2) {
        let ([x0, y0], [x1, y1]) = (w[0], w[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    points[points.len() - 1][1]
}

#[allow(clippy::too_many_arguments)]
fn step_control(
    c: &ControlPlan,
    cs: &mut ControlState,
    output: Option<Result<f64, String>>,
    reading: Option<&Sample<ControlReading>>,
    emergency: Option<String>,
    ov: Option<f64>,
    measuring: bool,
    now: f64,
    dt: f64,
    cmds: &mut Vec<Command>,
) {
    let key = &c.key;
    if cs.releasing {
        return;
    }
    if cs.state == State::Fault && !cs.force_reassert {
        return;
    }
    if (cs.state == State::Contested || cs.parked.is_some()) && !cs.force_reassert {
        contested(c, cs, emergency, cmds);
        return;
    }
    let Some(kind) = c.kind else {
        cs.state = State::Absent;
        return;
    };
    if !kind.drivable() {
        // Listed for what it is, never written (§3.5). Requests refuse to assign one; a configuration written by hand that does
        // is shown with the reason instead of failing the whole plan.
        if cs.held {
            cs.releasing = true;
            cmds.push(Command::Release { key: key.clone() });
        }
        cs.state = State::Firmware;
        cs.reason = output.is_some().then(|| kind.undrivable_reason()).flatten();
        return;
    }
    let Some(output) = output else {
        // Not adopted by the active profile: give it back to the firmware.
        if cs.held {
            cs.releasing = true;
            cmds.push(Command::Release { key: key.clone() });
        } else {
            cs.state = State::Firmware;
        }
        cs.reason = None;
        return;
    };
    if !cs.held {
        if !cs.taking {
            cs.taking = true;
            cs.state = State::Firmware;
            cmds.push(Command::Take { key: key.clone() });
        }
        return;
    }

    let reading = match reading {
        Some(Sample::Value(r)) => *r,
        other => {
            let why = match other {
                Some(Sample::Error(e)) => e.clone(),
                Some(Sample::Implausible(v)) => format!("read {v}"),
                _ => "not read".to_owned(),
            };
            cs.state = State::Fault;
            cs.reason = Some(format!("cannot read the control back: {why}"));
            cs.releasing = true;
            cmds.push(Command::Release { key: key.clone() });
            return;
        }
    };

    // §6.4: is the control still doing what we last told it?
    if let (false, Some(raw)) = (cs.handback || cs.force_reassert, cs.written) {
        let scale = c.scale();
        // it87 and nct6775 read a manual channel at 100 % back as "no control" (§3.5): that is what was written.
        let mode_ok = reading.mode == ControlMode::Manual || (reading.mode == ControlMode::Full && raw >= scale);
        let value_ok = match kind {
            // NVML percent is not duty (§0): only the policy says who is in charge.
            ControlKind::Nvml { .. } => true,
            _ => {
                let tolerance = (scale as f64 * READBACK_TOLERANCE).round().max(1.0) as u32;
                reading.raw.is_some_and(|r| r.abs_diff(raw) <= tolerance)
            }
        };
        if mode_ok && value_ok {
            cs.mismatch = 0;
        } else {
            cs.mismatch += 1;
        }
        if cs.mismatch >= MISMATCH_TICKS {
            cs.mismatch = 0;
            cs.reasserts.retain(|t| now - t < REASSERT_WINDOW);
            if cs.reasserts.len() < MAX_REASSERTS {
                cs.reasserts.push(now);
                cs.force_reassert = true;
            } else {
                cs.state = State::Contested;
                let read = match (kind, reading.raw) {
                    (ControlKind::Duty { scale }, Some(r)) => format!("read back {:.0} %", r as f64 * 100.0 / scale as f64),
                    _ => format!("read back {:.0} %", reading.percent),
                };
                cs.reason = Some(format!(
                    "wrote {:.0} %, {read} in mode {:?}, {} times in {REASSERT_WINDOW:.0} s; stopped writing",
                    cs.duty, reading.mode, MAX_REASSERTS
                ));
                return;
            }
        }
    }

    let (state, desired, immediate, reason) = if let Some(why) = emergency {
        (State::Emergency, 100.0, true, Some(why))
    } else {
        match output {
            Ok(v) if measuring => (State::Measuring, v, true, None),
            Err(e) => (State::Failsafe, c.failsafe, true, Some(e)),
            Ok(_) if ov.is_some() => (State::Override, ov.expect("checked"), true, None),
            Ok(v) => (State::Curve, v + c.offset, false, None),
        }
    };
    cs.reason = reason;
    if state == State::Measuring {
        // Setup's own duty, exactly: no floor, limit, step rate or kick (§5.4 measures below the floor).
        let duty = match kind {
            ControlKind::Nvml { min, max } => desired.clamp(min as f64, max as f64),
            _ => desired.clamp(0.0, 100.0),
        };
        cs.kick_until = None;
        cs.trend = Trend::Steady;
        cs.state = state;
        cs.duty = duty;
        cs.target = duty;
        let raw = c.raw(duty);
        if cs.force_reassert {
            cs.force_reassert = false;
            cmds.push(Command::Reassert { key: key.clone(), raw });
        } else if cs.handback || cs.written != Some(raw) {
            cmds.push(Command::Write { key: key.clone(), raw });
        }
        return;
    }

    let mut duty;
    match kind {
        ControlKind::Nvml { min, max } => {
            let floor = c.floor.max(min as f64);
            let top = c.max.min(max as f64);
            if c.may_stop && matches!(state, State::Curve | State::Override) {
                let go_back = if cs.handback { desired < floor + RESUME_MARGIN } else { desired < floor };
                if go_back {
                    cs.state = State::Handback;
                    cs.target = desired.max(0.0);
                    cs.duty = 0.0;
                    cs.trend = Trend::Steady;
                    cs.force_reassert = false;
                    if !cs.handback {
                        cmds.push(Command::Handback { key: key.clone() });
                    }
                    return;
                }
            }
            let target = desired.clamp(floor, top.max(floor));
            duty = if immediate || cs.handback { target } else { step(cs.duty.max(floor), target, c, dt) };
            duty = duty.max(floor);
            cs.target = target;
        }
        _ => {
            let stop = c.stop.unwrap_or(0.0);
            let top = if immediate { 100.0 } else { c.max };
            let mut target = desired.clamp(0.0, top);
            target = if c.may_stop && target < stop { 0.0 } else { target.max(c.floor) };
            let from_stopped = c.may_stop && target > 0.0 && cs.duty <= 0.0;
            duty = if immediate || from_stopped { target } else { step(cs.duty, target, c, dt) };
            if target == 0.0 && duty < c.floor {
                duty = 0.0;
            }
            if from_stopped {
                cs.kick_until = Some(now + KICK_SECONDS);
            }
            match cs.kick_until {
                Some(k) if now < k && target > 0.0 => duty = duty.max(c.start),
                _ => cs.kick_until = None,
            }
            if duty > 0.0 {
                duty = duty.max(c.floor);
            }
            cs.target = target;
        }
    }
    cs.trend = if (cs.target - duty).abs() <= 0.5 {
        Trend::Steady
    } else if cs.target > duty {
        Trend::Rising
    } else {
        Trend::Falling
    };
    cs.state = state;
    cs.duty = duty;
    let raw = c.raw(duty);
    if cs.force_reassert {
        cs.force_reassert = false;
        cmds.push(Command::Reassert { key: key.clone(), raw });
    } else if cs.handback || cs.written != Some(raw) {
        cmds.push(Command::Write { key: key.clone(), raw });
    }
}

/// A contested control is not written, except in an emergency, which outranks the contest: Penna takes the fan back at 100 %
/// every tick, and the control is contested again once the emergency ends.
fn contested(c: &ControlPlan, cs: &mut ControlState, emergency: Option<String>, cmds: &mut Vec<Command>) {
    match emergency {
        Some(why) if cs.held => {
            if cs.parked.is_none() {
                cs.parked = Some(cs.reason.take().unwrap_or_else(|| "contested".to_owned()));
            }
            cs.state = State::Emergency;
            cs.reason = Some(why);
            cs.duty = 100.0;
            cs.target = 100.0;
            cs.trend = Trend::Steady;
            cmds.push(Command::Reassert { key: c.key.clone(), raw: c.raw(100.0) });
        }
        _ => {
            if let Some(reason) = cs.parked.take() {
                cs.state = State::Contested;
                cs.reason = Some(reason);
            }
        }
    }
}

fn step(from: f64, to: f64, c: &ControlPlan, dt: f64) -> f64 {
    if dt <= 0.0 {
        return to;
    }
    from + (to - from).clamp(-c.step_down * dt, c.step_up * dt)
}
