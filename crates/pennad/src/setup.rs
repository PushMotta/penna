//! Setup in the daemon (DESIGN.md §5): the run belongs to the connection that started it, steps once a tick before the control
//! loop, and its results go to `measured.toml` when measuring is over. [`penna_core::setup`] decides; this module feeds it what
//! the machine knows and carries out what it says.
use crate::Live;
use crate::trace::Trace;
use penna_core::boards::Board;
use penna_core::plan::Plan;
use penna_core::setup::{GUARD_CPU, GUARD_GPU, Guard, Input, Role, Setup, Step, TachHint, Target};
use penna_core::{Inventory, Key, Readings, Source};
use penna_proto::Level;
use std::collections::BTreeMap;

/// How long `identify` spins a fan (§5.3).
pub const IDENTIFY_SECONDS: f64 = 3.0;
pub const TAKEN: &str = "Setup is running in another window.";

/// A setup run and the connection it belongs to.
pub struct SetupRun {
    pub owner: u64,
    pub run: Setup,
    /// Its results are in `measured.toml`.
    pub saved: bool,
    last_step: Step,
}

/// What setup needs to know about this machine: every control Penna drives (or the ones named), every motherboard tach with
/// the board database's hint for its header, and the guards (§5).
pub fn input(inventory: &Inventory, board: Option<&Board>, only: Option<&[Key]>) -> Input {
    let targets = inventory
        .devices
        .iter()
        .flat_map(|d| &d.controls)
        .filter(|c| c.kind.drivable() && only.is_none_or(|o| o.contains(&c.key)))
        .map(|c| Target { key: c.key.clone(), label: c.label.clone(), kind: c.kind })
        .collect();
    let tachs = inventory
        .devices
        .iter()
        .filter(|d| d.source == Source::Hwmon)
        .flat_map(|d| d.tachs.iter().map(move |t| (d, t)))
        .map(|(d, t)| {
            let channel = t.key.channel().strip_prefix("fan").unwrap_or_default();
            let role = board.and_then(|b| b.chip(&d.driver)).and_then(|c| c.roles.get(channel)).cloned();
            TachHint { key: t.key.clone(), label: t.label.clone(), role }
        })
        .collect();
    let mut guards = Vec::new();
    if let Some(cpu) = penna_core::starter::cpu_control_temperature(inventory) {
        let label = inventory.devices.iter().flat_map(|d| &d.sensors).find(|s| s.key == cpu).map_or("Tctl", |s| s.label.as_str());
        guards.push(Guard { key: cpu.clone(), label: format!("CPU · {label}"), limit: GUARD_CPU });
    }
    let gpus: Vec<_> = inventory.devices.iter().filter(|d| d.source == Source::Nvml).collect();
    for (i, d) in gpus.iter().enumerate() {
        if let Some(s) = d.sensors.iter().find(|s| s.key.channel() == "temp") {
            let n = if gpus.len() > 1 { format!(" {}", i + 1) } else { String::new() };
            guards.push(Guard { key: s.key.clone(), label: format!("GPU{n}"), limit: GUARD_GPU });
        }
    }
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get() as u32);
    Input { targets, tachs, guards, threads }
}

/// `setup.start` from connection `conn`.
pub fn start(live: &mut Live, inventory: &Inventory, out: &mut Trace, conn: u64, only: Option<Vec<Key>>) -> Result<String, String> {
    if let Some(s) = &live.setup
        && !s.run.finished()
    {
        return Err(if s.owner == conn { "Setup is already running.".into() } else { TAKEN.into() });
    }
    let mut input = input(inventory, live.board.as_ref(), only.as_deref());
    // A fan the configuration names is called that ("GPU fan 1").
    for t in &mut input.targets {
        if let Some(l) = live.saved.control(&t.key).and_then(|c| c.label.clone()) {
            t.label = l;
        }
    }
    if input.targets.is_empty() {
        return Err("There is no fan here that Penna can drive: penna doctor says why.".into());
    }
    let keys: Vec<Key> = input.targets.iter().map(|t| t.key.clone()).collect();
    let n = keys.len();
    live.plan.carry(&keys, inventory, &live.measured);
    live.setup = Some(SetupRun { owner: conn, run: Setup::new(input, live.now), saved: false, last_step: Step::Idle });
    let text = format!("Setup started: {n} fan control{}. Every fan stays with its firmware for the 30 s idle sample", plural(n));
    out.note(live.now, Level::Info, None, text.clone());
    Ok(text)
}

/// Stops a running setup: every fan goes back as it was found.
pub fn stop(live: &mut Live, out: &mut Trace, reason: &str) -> bool {
    match live.setup.as_mut() {
        Some(s) if !s.run.finished() => {
            s.run.stop(reason);
            live.state.measuring.clear();
            live.plan.end_setup();
            out.note(live.now, Level::Info, None, format!("Setup stopped: {reason} Every fan is back as it was found"));
            true
        }
        _ => false,
    }
}

/// The connection that started setup closed: a running setup stops, a finished one is put away.
pub fn closed(live: &mut Live, out: &mut Trace, conn: u64) -> bool {
    if !live.setup.as_ref().is_some_and(|s| s.owner == conn) {
        return false;
    }
    stop(live, out, "The window that ran setup closed.");
    live.setup = None;
    live.plan.end_setup();
    true
}

/// One tick of setup, before the control loop runs: what it measures goes into `measuring`, together with any fan being
/// identified. When measuring is over, the results are saved, and this returns true: the caller names the controls for their
/// headers, then [`replan`]s, so the plan learns the new names and floors.
pub fn step(live: &mut Live, readings: &Readings, out: &mut Trace, now: f64, unix: f64) -> bool {
    let mut measuring = BTreeMap::new();
    let load1 = if live.real { load_average() } else { None };
    let mut results = None;
    if let Some(s) = live.setup.as_mut()
        && !s.run.finished()
    {
        measuring = s.run.step(readings, &live.state, load1, now);
        let step = s.run.step_now();
        if step != s.last_step {
            s.last_step = step;
            out.note(now, Level::Info, None, format!("Setup: {}", step.name().to_lowercase()));
        }
        if let Some(why) = s.run.stopped() {
            out.note(now, Level::Alert, None, format!("Setup stopped: {why} Every fan is back as it was found"));
        } else if s.run.finished() {
            results = Some(s.run.measured(unix));
            s.saved = true;
        }
    }
    let measured = results.is_some();
    if let Some(results) = results {
        for c in &results.controls {
            live.drift.forget(&c.key);
        }
        let n = results.controls.iter().filter(|c| !c.calibration.is_empty()).count();
        live.measured.merge(results);
        match save(live) {
            Ok(where_) => out.note(now, Level::Info, None, format!("Setup measured {n} fans; saved to {where_}")),
            Err(e) => out.note(now, Level::Alert, None, format!("Setup measured {n} fans, but saving failed: {e}")),
        }
    }
    live.identify.retain(|_, until| now < *until);
    for key in live.identify.keys() {
        measuring.insert(key.clone(), 100.0);
    }
    // Setup's controls stay in the plan while it runs or a fan is being identified; after, they go back and go.
    if live.identify.is_empty() && live.setup.as_ref().is_none_or(|s| s.run.finished()) {
        live.plan.end_setup();
    }
    live.state.measuring = measuring;
    measured
}

/// `setup.roles`: what the user confirmed each fan is (§5.3), kept with the measurements. A fan held at 50 % because it sits on
/// a pump header, now confirmed as anything but a pump, is measured below that at once (§11.3 call 7), on this connection.
pub fn roles(live: &mut Live, inventory: &Inventory, out: &mut Trace, conn: u64, roles: &BTreeMap<Key, Role>) -> Result<String, String> {
    for (key, role) in roles {
        let m = match live.measured.controls.iter_mut().find(|m| &m.key == key) {
            Some(m) => m,
            None => {
                live.measured.controls.push(penna_core::config::MeasuredControl::new(key.clone()));
                live.measured.controls.last_mut().expect("just pushed")
            }
        };
        m.role = Some(*role);
    }
    save(live).map_err(|e| format!("Couldn't save the roles: {e}."))?;
    let n = roles.len();
    let mut text = format!("{n} role{} confirmed", plural(n));
    let below: Vec<penna_core::config::MeasuredControl> = live
        .measured
        .controls
        .iter()
        .filter(|m| m.lowest == Some(penna_core::setup::PUMP_LOWEST) && m.role.is_some_and(|r| r != Role::Pump) && !m.tachs.is_empty())
        .filter(|m| roles.contains_key(&m.key))
        .cloned()
        .collect();
    if !below.is_empty() && live.setup.as_ref().is_none_or(|s| s.run.finished()) {
        let keys: Vec<Key> = below.iter().map(|m| m.key.clone()).collect();
        let input = input(inventory, live.board.as_ref(), Some(&keys));
        let names: Vec<String> = input.targets.iter().map(|t| t.label.clone()).collect();
        live.plan.carry(&keys, inventory, &live.measured);
        live.setup =
            Some(SetupRun { owner: conn, run: Setup::calibrate(input, &below, live.now), saved: false, last_step: Step::Calibration });
        text = format!("{text}; measuring {} below 50 %, about a minute", names.join(" and "));
        out.note(live.now, Level::Info, None, format!("Setup: measuring {} below 50 %", names.join(" and ")));
    }
    Ok(text)
}

/// `identify`: 100 % for 3 s, then back.
pub fn identify(live: &mut Live, inventory: &Inventory, key: Key) -> Result<(), String> {
    if live.setup.as_ref().is_some_and(|s| !s.run.finished()) {
        return Err("Setup is measuring every fan; identify one once it is done.".into());
    }
    live.plan.carry(std::slice::from_ref(&key), inventory, &live.measured);
    live.identify.insert(key, live.now + IDENTIFY_SECONDS * live.rate);
    Ok(())
}

/// The plan again, with what setup measured: names, floors and start duties change (§4.5, §5.2).
pub fn replan(live: &mut Live, inventory: &Inventory, out: &mut Trace, now: f64) {
    let cfg = live.draft.as_ref().map_or_else(|| live.saved.clone(), |d| d.config.clone());
    match Plan::new(cfg, inventory, &live.measured, live.cpu_model.as_deref()) {
        Ok(plan) => live.replace_plan(plan, inventory),
        Err(problems) => {
            let list: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
            out.note(now, Level::Warn, None, format!("The new measurements don't fit the configuration: {}", list.join("; ")));
        }
    }
}

/// Writes `measured.toml`, where this daemon keeps it, and says where.
fn save(live: &Live) -> Result<String, String> {
    let Some(path) = &live.measured_path else { return Ok("memory only".into()) };
    let text = format!(
        "# What Penna's setup measured on this machine (DESIGN.md §5, §7.5): facts about the hardware, written by pennad.\n{}",
        live.measured.to_toml()
    );
    penna_hw::held::write_atomically(path, text.as_bytes()).map_err(|e| format!("{e:#}"))?;
    Ok(path.display().to_string())
}

fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
