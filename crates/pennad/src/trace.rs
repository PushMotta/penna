//! What the daemon says while it runs. Mock mode is meant to be read by a person: one line a tick, and a line for everything
//! that changes. Every line that says something changed is also kept for [`Trace::take_events`], which the socket sends to
//! subscribers as `event`s (DESIGN.md §7.3).
use penna_core::config::{Measured, VirtualKind};
use penna_core::control::{Command, LoopState, Outcome, SourceValue, State, Trend};
use penna_core::plan::Plan;
use penna_core::{Key, Readings, Sample};
use penna_hw::Machine;
use penna_hw::sim::Sim;
use penna_proto::Level;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Ticks of 0 RPM at a duty that should spin the fan before saying so.
const STALL_TICKS: u32 = 3;

pub struct Trace {
    quiet: bool,
    labels: BTreeMap<Key, String>,
    sources: Vec<Key>,
    tachs: BTreeMap<Key, Vec<Key>>,
    last: BTreeMap<Key, (State, Option<String>)>,
    emergency: bool,
    stalls: BTreeMap<Key, u32>,
    stalled: BTreeSet<Key>,
    events: Vec<(Level, Option<Key>, String)>,
    /// Device ids as last seen, to say what came and went (§3.6).
    devices: BTreeMap<String, String>,
}

/// "it8688 (Motherboard): 5 fan controls, 5 fans, 6 temperatures".
fn describe(d: &penna_core::Device) -> String {
    let count = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    format!(
        "{} ({}): {}, {}, {}",
        d.driver,
        d.label,
        count(d.controls.len(), "fan control"),
        count(d.tachs.len(), "fan"),
        count(d.sensors.len(), "temperature")
    )
}

fn t(now: f64) -> String {
    format!("t={now:>5.0}")
}

impl Trace {
    pub fn new(plan: &Plan, measured: &Measured, quiet: bool) -> Trace {
        let mut tachs: BTreeMap<Key, Vec<Key>> = measured.controls.iter().map(|m| (m.key.clone(), m.tachs.clone())).collect();
        for (k, c) in &plan.controls {
            if c.is_nvml() {
                tachs.insert(k.clone(), vec![k.clone()]);
            }
        }
        let mut sources: Vec<Key> = plan.config.curves.iter().filter_map(|c| c.kind.source().cloned()).collect();
        sources.sort();
        sources.dedup();
        Trace {
            quiet,
            labels: BTreeMap::new(),
            sources,
            tachs,
            last: BTreeMap::new(),
            emergency: false,
            stalls: BTreeMap::new(),
            stalled: BTreeSet::new(),
            events: Vec::new(),
            devices: BTreeMap::new(),
        }
    }

    fn learn(&mut self, inventory: &penna_core::Inventory) {
        for d in &inventory.devices {
            for s in &d.sensors {
                let short = if d.source == penna_core::Source::Nvml { "GPU".to_owned() } else { s.label.clone() };
                self.labels.entry(s.key.clone()).or_insert(short);
            }
        }
        self.devices = inventory.devices.iter().map(|d| (d.id.clone(), describe(d))).collect();
    }

    /// The hwmon devices changed while running (§3.6): what appeared and what went.
    pub fn devices_changed(&mut self, now: f64, inventory: &penna_core::Inventory) {
        let before = self.devices.clone();
        self.learn(inventory);
        let appeared: Vec<String> = self.devices.iter().filter(|(id, _)| !before.contains_key(*id)).map(|(_, d)| d.clone()).collect();
        let gone: Vec<String> = before.iter().filter(|(id, _)| !self.devices.contains_key(*id)).map(|(_, d)| d.clone()).collect();
        let mut parts = Vec::new();
        if !appeared.is_empty() {
            parts.push(format!("appeared: {}", appeared.join("; ")));
        }
        if !gone.is_empty() {
            parts.push(format!("gone: {}", gone.join("; ")));
        }
        if !parts.is_empty() {
            self.note(now, Level::Info, None, format!("Devices changed. {}", parts.join(". ")));
        }
    }

    fn label<'a>(&'a self, plan: &'a Plan, key: &'a Key) -> &'a str {
        plan.controls.get(key).map(|c| c.label.as_str()).or_else(|| self.labels.get(key).map(String::as_str)).unwrap_or(key.as_str())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn banner(
        &mut self,
        plan: &Plan,
        machine: &Machine,
        sim: Option<&Sim>,
        paths: (&Path, &Path),
        tick: f64,
        speed: f64,
        seconds: Option<f64>,
    ) {
        let (held, socket) = paths;
        self.learn(&machine.inventory);
        for v in &plan.config.sensors {
            self.labels.insert(v.key.clone(), v.label.clone().unwrap_or_else(|| v.key.channel().to_owned()));
        }
        match sim {
            Some(sim) => {
                let sc = sim.scenario();
                eprintln!("pennad {}, mock: {} · {}", env!("PENNA_VERSION"), sc.name, sc.cpu_model);
                eprintln!("  tree       {}", sim.root().display());
            }
            None => eprintln!("pennad {}: {} devices", env!("PENNA_VERSION"), machine.inventory.devices.len()),
        }
        eprintln!("  held.json  {}", held.display());
        eprintln!("  socket     {}", socket.display());
        let length = seconds.map_or("until stopped".to_owned(), |s| format!("{s:.0} s"));
        let pace = if sim.is_some() && speed != 1.0 { format!(", {speed}× real time") } else { String::new() };
        eprintln!("  tick       {tick} s{pace}, {length}");
        let adopted: Vec<String> = plan
            .controls
            .iter()
            .filter_map(|(k, c)| plan.assignment(&plan.config.profile, k).map(|curve| format!("{} → {curve}", c.label)))
            .collect();
        eprintln!("  profile    {}: {}", plan.config.profile, adopted.join(" · "));
        let firmware: Vec<&str> = machine
            .inventory
            .devices
            .iter()
            .flat_map(|d| &d.controls)
            .filter(|c| plan.assignment(&plan.config.profile, &c.key).is_none())
            .map(|c| c.key.as_str())
            .collect();
        if !firmware.is_empty() {
            eprintln!("  firmware   {}", firmware.join(" · "));
        }
        let limits: Vec<String> =
            plan.emergency.iter().map(|(k, l)| format!("{} {l} °C", self.labels.get(k).map_or(k.as_str(), String::as_str))).collect();
        eprintln!("  emergency  {}", limits.join(" · "));
        for v in &plan.config.sensors {
            if let VirtualKind::Max(ins) = &v.kind {
                let names: Vec<&str> = ins.iter().map(|k| self.labels.get(k).map_or(k.as_str(), String::as_str)).collect();
                eprintln!("  virtual    {} = max({})", self.labels[&v.key], names.join(", "));
            }
        }
        eprintln!();
    }

    pub fn sim_events(&self, now: f64, events: &[String]) {
        for e in events {
            eprintln!("{}  ⚡ {e}", t(now));
        }
    }

    /// What changed since the last call, for subscribers.
    pub fn take_events(&mut self) -> Vec<(Level, Option<Key>, String)> {
        std::mem::take(&mut self.events)
    }

    /// Something a person asked for, said like everything else.
    pub fn note(&mut self, now: f64, level: Level, control: Option<Key>, text: String) {
        eprintln!("{}  {text}", t(now));
        self.events.push((level, control, text));
    }

    pub fn command(&mut self, now: f64, plan: &Plan, st: &LoopState, cmd: &Command, outcome: &Outcome) {
        let label = self.label(plan, cmd.key()).to_owned();
        let line = match (cmd, outcome) {
            (Command::Take { key }, Outcome::Taken { duty }) if plan.controls.get(key).is_some_and(|c| c.is_nvml()) => {
                format!("{label}: taken; the way back is in held.json. It stays on the driver's curve until the first write")
            }
            (Command::Take { .. }, Outcome::Taken { duty }) => {
                format!("{label}: taken at the firmware's {duty:.0} %; the way back is in held.json")
            }
            (Command::Reassert { key, .. }, Outcome::Done) => match st.controls.get(key) {
                // A contested fan taken back for the emergency: the state line already says so, every tick.
                Some(c) if c.state == State::Emergency => return,
                c => format!(
                    "{label}: something changed it; manual control re-asserted ({} of 3 in 30 s)",
                    c.map_or(0, |c| c.reasserts.len())
                ),
            },
            (Command::Handback { key }, Outcome::Done) => {
                let floor = plan.controls.get(key).map_or(0.0, |c| c.floor);
                format!("{label}: below its {floor:.0} % floor: handed to the driver's curve, still held")
            }
            (Command::Release { .. }, Outcome::Done) => format!("{label}: handed back to its firmware, exactly as found"),
            (Command::Write { .. }, Outcome::Done) => return,
            (cmd, Outcome::Failed(e)) => format!("{label}: {} failed: {e}", verb(cmd)),
            (cmd, o) => format!("{label}: {} → {o:?}", verb(cmd)),
        };
        let level = match (cmd, outcome) {
            (_, Outcome::Failed(_)) => Level::Alert,
            (Command::Reassert { .. }, _) => Level::Warn,
            _ => Level::Info,
        };
        eprintln!("{}  {line}", t(now));
        self.events.push((level, Some(cmd.key().clone()), line));
    }

    pub fn tick(&mut self, now: f64, plan: &Plan, st: &LoopState, readings: &Readings) {
        if st.emergency.is_empty() == self.emergency {
            self.emergency = !st.emergency.is_empty();
            if self.emergency {
                let (k, l) = st.emergency.iter().next().expect("not empty");
                let v = st.sources.get(k).and_then(SourceValue::celsius).unwrap_or(f64::NAN);
                let text = format!(
                    "EMERGENCY  {} at {v:.1} °C, limit {l} °C: every fan to 100 % until {:.0} °C",
                    self.labels.get(k).map_or(k.as_str(), String::as_str),
                    l - 5.0
                );
                eprintln!("{}  {text}", t(now));
                self.events.push((Level::Alert, None, text.replacen("EMERGENCY  ", "Emergency: ", 1)));
            } else {
                eprintln!("{}  emergency over", t(now));
                self.events.push((Level::Info, None, "Emergency over".to_owned()));
            }
        }
        for (k, cs) in &st.controls {
            let was = self.last.get(k).cloned();
            let now_is = (cs.state, cs.reason.clone());
            let changed = match &was {
                None => cs.state != State::Firmware,
                Some((s, r)) => *s != cs.state || (r != &cs.reason && cs.state != State::Emergency),
            };
            if changed {
                let from = was.map_or(State::Firmware, |w| w.0);
                let why = cs.reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default();
                let mark =
                    if matches!(cs.state, State::Failsafe | State::Emergency | State::Contested | State::Fault) { "⚠ " } else { "" };
                if from != cs.state || !why.is_empty() {
                    let text = format!("{}: {} → {}{why}", self.label(plan, k), from.name(), cs.state.name());
                    eprintln!("{}  {mark}{text}", t(now));
                    self.events.push((if mark.is_empty() { Level::Info } else { Level::Alert }, Some(k.clone()), text));
                }
            }
            self.last.insert(k.clone(), now_is);
            self.stall(now, plan, k, cs.duty, cs.state, readings);
        }
        if self.quiet {
            return;
        }
        let mut line = t(now);
        let temps: Vec<String> = self
            .sources
            .iter()
            .map(|k| {
                let name = self.labels.get(k).map_or(k.channel(), String::as_str);
                match st.sources.get(k) {
                    Some(SourceValue::Celsius(v)) => format!("{name} {v:.1}"),
                    Some(SourceValue::Asleep) => format!("{name} asleep"),
                    _ => format!("{name} --"),
                }
            })
            .collect();
        line.push_str(&format!(" │ {}", temps.join("  ")));
        for (k, c) in &plan.controls {
            let Some(cs) = st.controls.get(k) else { continue };
            let rpm = self.rpm(k, readings).map_or("--".to_owned(), |r| format!("{r}"));
            let curve = plan.assignment(&st.profile, k).and_then(|id| st.curves.get(id));
            let mark = match cs.trend {
                Trend::Rising => "↑",
                Trend::Falling => "↓",
                Trend::Steady if curve.is_some_and(|c| c.held) && cs.state == State::Curve => "=",
                Trend::Steady => " ",
            };
            let what = match cs.state {
                State::Curve | State::Override if !c.is_nvml() || !cs.handback => format!("{:>3.0}%{mark}", cs.duty),
                State::Handback => "drv ".to_owned(),
                State::Firmware => "fw  ".to_owned(),
                s => format!("{:>3.0}% {}", cs.duty, s.name().to_uppercase()),
            };
            line.push_str(&format!(" │ {} {what} {rpm}", c.label));
        }
        eprintln!("{line}");
    }

    fn rpm(&self, key: &Key, readings: &Readings) -> Option<u32> {
        let tachs = self.tachs.get(key)?;
        tachs.iter().find_map(|t| match readings.rpms.get(t) {
            Some(Sample::Value(r)) => Some(*r),
            _ => None,
        })
    }

    fn stall(&mut self, now: f64, plan: &Plan, key: &Key, duty: f64, state: State, readings: &Readings) {
        let Some(c) = plan.controls.get(key) else { return };
        let (Some(stop), false) = (c.stop, c.is_nvml()) else { return };
        let Some(tachs) = self.tachs.get(key).cloned() else { return };
        let zero = tachs.iter().all(|t| matches!(readings.rpms.get(t), Some(Sample::Value(0))));
        let should_spin = duty >= stop + 10.0 && !matches!(state, State::Firmware | State::Absent | State::Contested | State::Fault);
        let n = self.stalls.entry(key.clone()).or_default();
        *n = if zero && should_spin { *n + 1 } else { 0 };
        if *n >= STALL_TICKS && self.stalled.insert(key.clone()) {
            let text = format!("{}: its tach reads 0 RPM at {duty:.0} % for {STALL_TICKS} s: a stalled fan or a broken tach", c.label);
            eprintln!("{}  ⚠ {text}", t(now));
            self.events.push((Level::Warn, Some(key.clone()), text));
        } else if !zero && self.stalled.remove(key) {
            let text = format!("{}: its tach reads again ({} RPM)", c.label, self.rpm(key, readings).unwrap_or(0));
            eprintln!("{}  {text}", t(now));
            self.events.push((Level::Info, Some(key.clone()), text));
        }
    }

    pub fn shutdown(&self, results: &[Result<String, String>]) {
        let done: Vec<&str> = results.iter().filter_map(|r| r.as_ref().ok()).map(String::as_str).collect();
        if !done.is_empty() {
            eprintln!("pennad: handed back: {}", done.join(", "));
        }
        for e in results.iter().filter_map(|r| r.as_ref().err()) {
            eprintln!("pennad: could not hand back: {e}; it stays in held.json for penna restore");
        }
    }
}

fn verb(cmd: &Command) -> &'static str {
    match cmd {
        Command::Take { .. } => "take",
        Command::Write { .. } => "write",
        Command::Reassert { .. } => "re-assert",
        Command::Handback { .. } => "hand-back",
        Command::Curve { .. } => "write the curve of",
        Command::Release { .. } => "release",
    }
}
