//! Requests carried out by the control loop between ticks (DESIGN.md §7.1, §7.3). Runtime requests (`full_speed`, `override`,
//! `retry`) change the loop's state only. Configuration requests (`profile.set`, `adopt`, `release`, `label`) are checked
//! against the machine as a whole new plan, then saved atomically, then used. Drafts (`draft.*`, M3) are a studio's unsaved
//! changes: checked the same way, run at once as a preview, and kept only by `draft.save`. A draft belongs to its connection.
use crate::trace::Trace;
use crate::{Draft, Live};
use penna_core::config::{self, Config, ControlConfig, Curve, CurveKind, DEFAULTS, FIRMWARE, LABEL_MAX, changes, explain_points, words};
use penna_core::control::{Request, Scope};
use penna_core::plan::{self, Plan};
use penna_core::{Inventory, Key};
use penna_proto::build::{self, StudioContext};
use penna_proto::{CurveKindName, DraftInfo, Level, Op, StudioCurve};
use serde_json::Value;
use std::path::Path;

/// The longest temporary override: a day.
const MAX_OVERRIDE: f64 = 86_400.0;
/// What a second studio is told while another holds a draft.
pub const DRAFT_TAKEN: &str = "Another studio has unsaved changes.";

/// Carries out one request from connection `conn`. A configuration or runtime request's result is `{"text": …}`: what changed, in
/// a sentence, for the CLI to print. `studio` and every draft request but `draft.save` answer with the studio body.
pub fn handle(live: &mut Live, inventory: &Inventory, out: &mut Trace, op: Op, uid: u32, conn: u64) -> Result<Option<Value>, String> {
    let now = live.now;
    let mut said = String::new();
    let mut note = |out: &mut Trace, control: Option<Key>, text: String| {
        out.note(now, Level::Info, control, format!("{text} (uid {uid})"));
        said = text;
    };
    match op {
        Op::ProfileSet { name } => {
            let id = profile(&live.saved, &name)?;
            change_saved(live, inventory, |c| {
                c.profile = id.clone();
                Ok(())
            })?;
            live.state.apply(&live.plan, Request::Profile(id.clone()), now)?;
            if id == DEFAULTS {
                note(out, None, "Profile: Defaults. Every fan goes back to its BIOS or driver".into());
            } else {
                note(out, None, format!("Profile: {}", words(&id)));
            }
        }
        Op::FullSpeed { on } => {
            live.state.apply(&live.plan, Request::FullSpeed(on), now)?;
            note(out, None, if on { "Full speed: every fan at 100 % until turned off".into() } else { "Full speed off".into() });
        }
        Op::Override { control, duty, seconds } => {
            if let Some(d) = duty
                && !(0.0..=100.0).contains(&d)
            {
                return Err(format!("A duty is between 0 and 100 %, not {d}."));
            }
            if let Some(s) = seconds
                && !(s > 0.0 && s <= MAX_OVERRIDE)
            {
                return Err(format!("An override lasts more than 0 and at most {MAX_OVERRIDE} seconds, not {s}."));
            }
            let (scope, who) = match control {
                None => (Scope::All, "Every fan".to_owned()),
                Some(name) => {
                    let key = control_key(&adopted(live), &name)?;
                    let label = live.plan.controls[&key].label.clone();
                    (Scope::Control(key), label)
                }
            };
            let key = match &scope {
                Scope::Control(k) => Some(k.clone()),
                Scope::All => None,
            };
            if key.is_none() && duty.is_none() {
                // "Every fan back on its curve" means every fan: per-fan overrides go too.
                let held: Vec<Key> = live.state.overrides.keys().cloned().collect();
                for k in held {
                    live.state.apply(&live.plan, Request::Override { scope: Scope::Control(k), duty: None, seconds: None }, now)?;
                }
            }
            live.state.apply(&live.plan, Request::Override { scope, duty, seconds: seconds.map(|s| s * live.rate) }, now)?;
            let text = match (duty, seconds) {
                (None, None) if key.is_none() => "Every fan: back on its curve".to_owned(),
                (None, _) => format!("{who}: back on its curve"),
                (Some(d), None) => format!("{who}: held at {d:.0} % until cleared"),
                (Some(d), Some(s)) => format!("{who}: held at {d:.0} % for {s:.0} s"),
            };
            note(out, key, text);
        }
        Op::Retry { control } => {
            let key = control_key(&adopted(live), &control)?;
            let label = live.plan.controls[&key].label.clone();
            live.state.apply(&live.plan, Request::Retry(key.clone()), now).map_err(|_| {
                let state = live.state.controls.get(&key).map_or("firmware", |c| c.state.name());
                format!("{label} is {state}; only a contested or faulted fan can be tried again.")
            })?;
            note(out, Some(key), format!("{label}: trying again"));
        }
        Op::Adopt { control, curve } => {
            let key = control_key(&every_control(live, inventory), &control)?;
            let id = curve_id(&live.saved, &curve)?;
            let active = editable(&live.saved)?;
            drivable(inventory, &key)?;
            change_saved(live, inventory, |c| {
                let active = c.profile.clone();
                c.profiles.entry(active).or_default().insert(key.clone(), id.clone());
                Ok(())
            })?;
            let label = live.plan.controls.get(&key).map_or_else(|| key.to_string(), |c| c.label.clone());
            note(out, Some(key), format!("{label} follows {} in {}", curve_label(&live.saved, &id), words(&active)));
        }
        Op::Release { control } => {
            let key = control_key(&every_control(live, inventory), &control)?;
            let active = editable(&live.saved)?;
            change_saved(live, inventory, |c| {
                let active = c.profile.clone();
                c.profiles.entry(active).or_default().insert(key.clone(), FIRMWARE.to_owned());
                Ok(())
            })?;
            let label = live.plan.controls.get(&key).map_or_else(|| key.to_string(), |c| c.label.clone());
            note(out, Some(key), format!("{label} goes back to its firmware in {}", words(&active)));
        }
        Op::Label { key, label } => {
            let label = label.trim().to_owned();
            if label.is_empty() || label.chars().count() > LABEL_MAX {
                return Err(format!("A label is 1 to {LABEL_MAX} characters."));
            }
            let key = labelled_key(live, inventory, &key)?;
            let was = relabel(&mut live.saved.clone(), inventory, &key, &label)?;
            change_saved(live, inventory, |c| relabel(c, inventory, &key, &label).map(|_| ()))?;
            note(out, Some(key), format!("{was} is now called {label}"));
        }
        Op::Studio => return Ok(Some(studio(live, inventory, conn))),
        Op::DraftPutCurve { curve } => {
            let mut cfg = draft_base(live, conn)?;
            let new = curve_from_studio(&cfg, inventory, curve)?;
            match cfg.curves.iter_mut().find(|c| c.id == new.id) {
                Some(c) => *c = new,
                None => cfg.curves.push(new),
            }
            preview(live, inventory, out, conn, cfg)?;
            return Ok(Some(studio(live, inventory, conn)));
        }
        Op::DraftDeleteCurve { curve } => {
            let mut cfg = draft_base(live, conn)?;
            let id = curve_id(&cfg, &curve)?;
            let label = curve_label(&cfg, &id);
            let mut users = Vec::new();
            for (profile, assign) in &cfg.profiles {
                for (key, curve) in assign {
                    if *curve == id {
                        users.push(format!("{} in {}", control_label(live, inventory, key), words(profile)));
                    }
                }
            }
            if !users.is_empty() {
                return Err(format!("{label} is used by {}; give it another curve first.", users.join(", ")));
            }
            let mixes: Vec<String> = cfg
                .curves
                .iter()
                .filter(|c| matches!(&c.kind, CurveKind::Mix { curves, .. } if curves.contains(&id)))
                .map(Curve::label)
                .collect();
            if !mixes.is_empty() {
                return Err(format!("{} mixes {label}; take it out of the mix first.", mixes.join(" and ")));
            }
            cfg.curves.retain(|c| c.id != id);
            preview(live, inventory, out, conn, cfg)?;
            return Ok(Some(studio(live, inventory, conn)));
        }
        Op::DraftAssign { control, curve } => {
            let key = control_key(&every_control(live, inventory), &control)?;
            let mut cfg = draft_base(live, conn)?;
            let id = if curve.trim().eq_ignore_ascii_case(FIRMWARE) { FIRMWARE.to_owned() } else { curve_id(&cfg, &curve)? };
            let active = editable(&cfg)?;
            if id != FIRMWARE {
                drivable(inventory, &key)?;
            }
            cfg.profiles.entry(active).or_default().insert(key, id);
            preview(live, inventory, out, conn, cfg)?;
            return Ok(Some(studio(live, inventory, conn)));
        }
        Op::DraftLimits { control, min, max, offset, step_up, step_down, may_stop, failsafe } => {
            let key = control_key(&every_control(live, inventory), &control)?;
            let mut cfg = draft_base(live, conn)?;
            if !cfg.controls.iter().any(|c| c.key == key) {
                cfg.controls.push(ControlConfig::defaults(key.clone()));
            }
            let c = cfg.controls.iter_mut().find(|c| c.key == key).expect("added above");
            let set = |field: &mut f64, v: Option<f64>| {
                if let Some(v) = v {
                    *field = v;
                }
            };
            set(&mut c.min, min);
            set(&mut c.max, max);
            set(&mut c.offset, offset);
            set(&mut c.step_up, step_up);
            set(&mut c.step_down, step_down);
            set(&mut c.failsafe, failsafe);
            if let Some(m) = may_stop {
                c.may_stop = m;
            }
            preview(live, inventory, out, conn, cfg)?;
            return Ok(Some(studio(live, inventory, conn)));
        }
        Op::DraftSave => {
            let draft = match &live.draft {
                None => return Err("There are no unsaved changes.".into()),
                Some(d) if d.owner != conn => return Err(DRAFT_TAKEN.into()),
                Some(d) => d.config.clone(),
            };
            let what = changed_names(live, inventory, &live.saved, &draft);
            persist(live, &draft)?;
            live.saved = draft;
            live.draft = None;
            note(out, None, format!("Saved: {what}"));
        }
        Op::DraftDiscard => {
            match &live.draft {
                Some(d) if d.owner != conn => return Err(DRAFT_TAKEN.into()),
                Some(_) => {
                    let what = changed_names(live, inventory, &live.saved, &live.draft.as_ref().expect("matched").config);
                    revert(live, inventory)?;
                    note(out, None, format!("Discarded unsaved changes to {what}"));
                }
                None => {}
            }
            return Ok(Some(studio(live, inventory, conn)));
        }
        Op::State | Op::Subscribe | Op::Unsubscribe | Op::Config => {
            return Err(format!("{} is answered by the socket, not the loop.", op.name()));
        }
    }
    Ok(Some(serde_json::json!({ "text": said })))
}

/// Connection `conn` closed. If it held a draft, the fans go back to the saved configuration, and everyone is told. Returns whether
/// anything changed.
pub fn closed(live: &mut Live, inventory: &Inventory, out: &mut Trace, conn: u64) -> bool {
    let Some(d) = live.draft.as_ref().filter(|d| d.owner == conn) else { return false };
    let what = changed_names(live, inventory, &live.saved, &d.config);
    if let Err(e) = revert(live, inventory) {
        out.note(live.now, Level::Alert, None, format!("A studio closed with unsaved changes, and going back failed: {e}"));
        return false;
    }
    out.note(live.now, Level::Info, None, format!("The studio closed without saving; back to the saved configuration ({what})"));
    true
}

/// The studio body for connection `conn`.
fn studio(live: &Live, inventory: &Inventory, conn: u64) -> Value {
    let body = live.tick_body(inventory);
    let draft = match &live.draft {
        Some(d) => {
            let (curves, controls) = changes(&live.saved, &d.config);
            DraftInfo { mine: d.owner == conn, dirty: true, curves, controls }
        }
        None => DraftInfo::default(),
    };
    let s = build::studio(&StudioContext { plan: &live.plan, inventory, measured: &live.measured, sensors: &body.sensors, draft });
    serde_json::to_value(s).expect("the studio body always serializes")
}

/// The configuration a draft request starts from: this connection's draft, or the saved one. Another connection's draft refuses.
fn draft_base(live: &Live, conn: u64) -> Result<Config, String> {
    match &live.draft {
        Some(d) if d.owner != conn => Err(DRAFT_TAKEN.into()),
        Some(d) => Ok(d.config.clone()),
        None => Ok(live.saved.clone()),
    }
}

/// Runs `cfg` as connection `conn`'s draft, at once: a refused one changes nothing and says why. A draft that matches what is
/// saved is no draft. A changed curve starts over from its new shape, so the preview is not held back by hysteresis.
fn preview(live: &mut Live, inventory: &Inventory, out: &mut Trace, conn: u64, cfg: Config) -> Result<(), String> {
    let plan = Plan::new(cfg.clone(), inventory, &live.measured, live.cpu_model.as_deref()).map_err(|p| config::sentence(&p))?;
    restart_changed_curves(live, &cfg);
    let started = live.draft.is_none();
    live.replace_plan(plan, inventory);
    if cfg == live.saved {
        live.draft = None;
    } else {
        live.draft = Some(Draft { owner: conn, config: cfg });
        if started {
            out.note(live.now, Level::Info, None, "A studio is previewing unsaved changes".into());
        }
    }
    Ok(())
}

/// Back to the saved configuration.
fn revert(live: &mut Live, inventory: &Inventory) -> Result<(), String> {
    let plan = plan_for(live, inventory, live.saved.clone())?;
    let saved = live.saved.clone();
    restart_changed_curves(live, &saved);
    live.replace_plan(plan, inventory);
    live.draft = None;
    Ok(())
}

fn restart_changed_curves(live: &mut Live, next: &Config) {
    let (curves, _) = changes(&live.plan.config, next);
    for id in curves {
        if let Some(c) = live.state.curves.get_mut(&id) {
            c.out = None;
            c.held = false;
        }
    }
}

/// The curves and controls a draft changes, as a person names them.
fn changed_names(live: &Live, inventory: &Inventory, saved: &Config, draft: &Config) -> String {
    let (curves, controls) = changes(saved, draft);
    let mut names: Vec<String> =
        curves.iter().map(|id| draft.curve(id).or(saved.curve(id)).map_or_else(|| id.clone(), Curve::label)).collect();
    names.extend(controls.iter().map(|k| control_label(live, inventory, k)));
    if names.is_empty() { "nothing".into() } else { names.join(", ") }
}

fn control_label(live: &Live, inventory: &Inventory, key: &Key) -> String {
    every_control(live, inventory).into_iter().find(|(k, _)| k == key).map_or_else(|| key.to_string(), |(_, l)| l)
}

/// A curve as the studio sent it, checked for what the configuration's own rules can't say well: its points, in values, and a
/// source that exists on this machine. Left-out label, response and hysteresis keep what the curve had; a value equal to the
/// default stays a default.
fn curve_from_studio(cfg: &Config, inventory: &Inventory, sc: StudioCurve) -> Result<Curve, String> {
    let id = sc.id.trim().to_owned();
    if id.is_empty() || id == FIRMWARE {
        return Err(format!("A curve needs an id other than {FIRMWARE:?}."));
    }
    let existing = cfg.curve(&id);
    let label = match sc.label {
        Some(l) => {
            let l = l.trim().to_owned();
            if l.is_empty() || l.chars().count() > LABEL_MAX {
                return Err(format!("A curve's name is 1 to {LABEL_MAX} characters."));
            }
            (l != words(&id)).then_some(l)
        }
        None => existing.and_then(|c| c.label.clone()),
    };
    let needs_source = |what: &str| format!("A {what} curve needs a source: the sensor it follows.");
    let kind = match sc.kind {
        CurveKindName::Graph => {
            let source = sc.source.ok_or_else(|| needs_source("graph"))?;
            let points = sc.points.ok_or("A graph curve needs its points.")?;
            if let Some(why) = explain_points(&points) {
                return Err(why);
            }
            CurveKind::Graph { source, points }
        }
        CurveKindName::Flat => {
            let duty = sc.duty.ok_or("A flat curve needs its duty.")?;
            if !(0.0..=100.0).contains(&duty) {
                return Err(format!("A flat curve runs between 0 and 100 %, not {}.", config::num(duty)));
            }
            CurveKind::Flat { percent: duty }
        }
        CurveKindName::Mix => {
            let m = sc.mix.ok_or("A mix needs the curves it mixes.")?;
            if let Some(missing) = m.of.iter().find(|c| cfg.curve(c).is_none()) {
                return Err(format!("A mix can only use curves that exist; there is no {missing:?}."));
            }
            if m.of.contains(&id) {
                return Err("A mix can't use itself.".into());
            }
            CurveKind::Mix { op: m.op, curves: m.of }
        }
        CurveKindName::Trigger => {
            let source = sc.source.ok_or_else(|| needs_source("trigger"))?;
            let t = sc.trigger.ok_or("A trigger needs its idle and load duties and its on and off temperatures.")?;
            if t.off >= t.on {
                return Err(format!(
                    "A trigger switches off below where it switches on: off at {} °C is not below on at {} °C.",
                    config::num(t.off),
                    config::num(t.on)
                ));
            }
            CurveKind::Trigger { source, idle: t.idle, load: t.load, on: t.on, off: t.off }
        }
    };
    if let Some(src) = kind.source() {
        let physical = inventory.devices.iter().flat_map(|d| &d.sensors).any(|s| &s.key == src);
        if !physical && cfg.virtual_sensor(src).is_none() {
            return Err(format!("There is no sensor {src} on this machine."));
        }
    }
    let had =
        |f: fn(&Curve) -> Option<f64>| existing.filter(|c| std::mem::discriminant(&c.kind) == std::mem::discriminant(&kind)).and_then(f);
    let keep = |given: Option<f64>, was: Option<f64>, default: f64| match given {
        Some(v) if was.is_none() && v == default => None,
        Some(v) => Some(v),
        None => was,
    };
    let (response, hysteresis) = match kind {
        CurveKind::Graph { .. } => (
            keep(sc.response, had(|c| c.response), plan::default_response(&kind)),
            keep(sc.hysteresis, had(|c| c.hysteresis), plan::default_hysteresis(&kind)),
        ),
        CurveKind::Trigger { .. } => (keep(sc.response, had(|c| c.response), plan::default_response(&kind)), None),
        _ => (None, None),
    };
    Ok(Curve { id, label, hysteresis_direction: existing.map(|c| c.hysteresis_direction).unwrap_or_default(), kind, response, hysteresis })
}

/// A change to the saved configuration: checked as a whole new plan, saved atomically, then used. A studio's draft gets the same
/// change, so its preview stays on top of what is saved; if the two can't both hold, nothing changes.
fn change_saved(live: &mut Live, inventory: &Inventory, change: impl Fn(&mut Config) -> Result<(), String>) -> Result<(), String> {
    let mut saved = live.saved.clone();
    change(&mut saved)?;
    let saved_plan = plan_for(live, inventory, saved.clone())?;
    let draft = match &live.draft {
        Some(d) => {
            let mut cfg = d.config.clone();
            change(&mut cfg)?;
            let plan =
                plan_for(live, inventory, cfg.clone()).map_err(|e| format!("That conflicts with a studio's unsaved changes: {e}"))?;
            Some((cfg, plan))
        }
        None => None,
    };
    persist(live, &saved)?;
    live.saved = saved;
    match (draft, live.draft.as_mut()) {
        (Some((cfg, plan)), Some(d)) => {
            d.config = cfg;
            live.replace_plan(plan, inventory);
        }
        _ => live.replace_plan(saved_plan, inventory),
    }
    Ok(())
}

/// The active profile, when it can be changed: Defaults can't (§4.7).
fn editable(cfg: &Config) -> Result<String, String> {
    if cfg.profile == DEFAULTS {
        let others: Vec<String> = cfg.profiles.keys().map(|p| words(p)).collect();
        return Err(format!(
            "Defaults leaves every fan with its BIOS or driver, and can't be changed. Choose {} first.",
            if others.is_empty() { "another profile".to_owned() } else { others.join(", ") }
        ));
    }
    Ok(cfg.profile.clone())
}

/// A control Penna can drive, or why not (§3.5).
fn drivable(inventory: &Inventory, key: &Key) -> Result<(), String> {
    let found = inventory.devices.iter().flat_map(|d| &d.controls).find(|c| &c.key == key);
    match found.and_then(|c| c.kind.undrivable_reason().map(|why| (c, why))) {
        Some((c, why)) => Err(format!("{} can't follow a curve. {why}", c.label)),
        None => Ok(()),
    }
}

/// A configuration as a plan for this machine, or why it can't be one.
fn plan_for(live: &Live, inventory: &Inventory, cfg: Config) -> Result<Plan, String> {
    Plan::new(cfg, inventory, &live.measured, live.cpu_model.as_deref()).map_err(|problems| {
        let list: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        format!("That would make the configuration invalid: {}.", list.join("; "))
    })
}

fn persist(live: &Live, cfg: &Config) -> Result<(), String> {
    match &live.config_path {
        Some(path) => save(path, cfg).map_err(|e| format!("Couldn't save the configuration to {}: {e:#}.", path.display())),
        None => Ok(()),
    }
}

fn save(path: &Path, cfg: &Config) -> anyhow::Result<()> {
    penna_hw::held::write_atomically(path, cfg.saved_text().as_bytes())
}

/// Controls the active configuration knows, with their labels.
fn adopted(live: &Live) -> Vec<(Key, String)> {
    live.plan.controls.iter().map(|(k, c)| (k.clone(), c.label.clone())).collect()
}

/// Every control: the configuration's, and every other one on the machine.
fn every_control(live: &Live, inventory: &Inventory) -> Vec<(Key, String)> {
    let mut all = adopted(live);
    for c in inventory.devices.iter().flat_map(|d| &d.controls) {
        if !all.iter().any(|(k, _)| k == &c.key) {
            all.push((c.key.clone(), live.plan.config.labels.get(&c.key).cloned().unwrap_or_else(|| c.label.clone())));
        }
    }
    all
}

/// A control named by its key, by its label (any case), or by the start of a label no other control shares.
pub fn control_key(candidates: &[(Key, String)], name: &str) -> Result<Key, String> {
    let name = name.trim();
    if let Some((k, _)) = candidates.iter().find(|(k, _)| k.as_str() == name) {
        return Ok(k.clone());
    }
    let lower = name.to_lowercase();
    let exact: Vec<&(Key, String)> = candidates.iter().filter(|(_, l)| l.to_lowercase() == lower).collect();
    let found = if exact.is_empty() { candidates.iter().filter(|(_, l)| l.to_lowercase().starts_with(&lower)).collect() } else { exact };
    match found.as_slice() {
        [(k, _)] => Ok(k.clone()),
        [] => {
            let names: Vec<&str> = candidates.iter().map(|(_, l)| l.as_str()).collect();
            Err(format!("No control is called {name:?}. Controls: {}.", if names.is_empty() { "none".into() } else { names.join(", ") }))
        }
        many => {
            let names: Vec<&str> = many.iter().map(|(_, l)| l.as_str()).collect();
            Err(format!("{name:?} could be {}; use more of the name, or the key.", names.join(" or ")))
        }
    }
}

fn profile(cfg: &Config, name: &str) -> Result<String, String> {
    let lower = name.trim().to_lowercase();
    if lower == DEFAULTS {
        return Ok(DEFAULTS.to_owned());
    }
    cfg.profiles
        .keys()
        .find(|id| id.as_str() == name || id.to_lowercase() == lower || words(id).to_lowercase() == lower)
        .cloned()
        .ok_or_else(|| {
            let ids: Vec<&str> = std::iter::once(DEFAULTS).chain(cfg.profiles.keys().map(String::as_str)).collect();
            format!("There is no profile {name:?}. Profiles: {}.", ids.join(", "))
        })
}

fn curve_id(cfg: &Config, name: &str) -> Result<String, String> {
    let lower = name.trim().to_lowercase();
    cfg.curves
        .iter()
        .find(|c| c.id == name || c.id.to_lowercase() == lower || c.label().to_lowercase() == lower)
        .map(|c| c.id.clone())
        .ok_or_else(|| {
            let names: Vec<String> = cfg.curves.iter().map(|c| format!("{} ({})", c.label(), c.id)).collect();
            format!("There is no curve {name:?}. Curves: {}.", names.join(", "))
        })
}

fn curve_label(cfg: &Config, id: &str) -> String {
    cfg.curve(id).map_or_else(|| id.to_owned(), config::Curve::label)
}

/// What a `label` request names: any sensor, tach or control, by key or by its current label.
fn labelled_key(live: &Live, inventory: &Inventory, name: &str) -> Result<Key, String> {
    if let Some(k) = Key::parse(name.trim()) {
        return Ok(k);
    }
    let mut all = every_control(live, inventory);
    for d in &inventory.devices {
        for s in &d.sensors {
            all.push((s.key.clone(), live.plan.config.labels.get(&s.key).cloned().unwrap_or_else(|| s.label.clone())));
        }
    }
    for v in &live.plan.config.sensors {
        all.push((v.key.clone(), v.label.clone().unwrap_or_else(|| words(v.key.channel()))));
    }
    control_key(&all, name).map_err(|e| e.replace("No control is called", "Nothing is called").replace("Controls:", "Known:"))
}

/// Puts the label where the configuration keeps it for this kind of key, and says what it was called before.
fn relabel(cfg: &mut Config, inventory: &Inventory, key: &Key, label: &str) -> Result<String, String> {
    let control = inventory.devices.iter().flat_map(|d| &d.controls).find(|c| &c.key == key);
    if control.is_some() || cfg.control(key).is_some() || cfg.profiles.values().any(|p| p.contains_key(key)) {
        let was =
            cfg.control(key).and_then(|c| c.label.clone()).or_else(|| control.map(|c| c.label.clone())).unwrap_or_else(|| key.to_string());
        match cfg.controls.iter_mut().find(|c| &c.key == key) {
            Some(c) => c.label = Some(label.to_owned()),
            None => cfg.controls.push(ControlConfig { label: Some(label.to_owned()), ..ControlConfig::defaults(key.clone()) }),
        }
        return Ok(was);
    }
    if let Some(v) = cfg.sensors.iter_mut().find(|v| &v.key == key) {
        let was = v.label.clone().unwrap_or_else(|| words(key.channel()));
        v.label = Some(label.to_owned());
        return Ok(was);
    }
    let hardware = inventory
        .devices
        .iter()
        .flat_map(|d| d.sensors.iter().map(|s| (&s.key, &s.label)).chain(d.tachs.iter().map(|t| (&t.key, &t.label))))
        .find(|(k, _)| *k == key);
    match hardware {
        Some((_, hw)) => {
            let was = cfg.labels.get(key).cloned().unwrap_or_else(|| hw.clone());
            cfg.labels.insert(key.clone(), label.to_owned());
            Ok(was)
        }
        None => Err(format!("Penna knows no sensor, tach or control {key}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls() -> Vec<(Key, String)> {
        ["CPU cooler", "Front intake", "GPU fan 1", "GPU fan 2"]
            .iter()
            .enumerate()
            .map(|(i, l)| (Key::parse(&format!("hwmon/it87/it87.2608/pwm{}", i + 1)).unwrap(), (*l).to_owned()))
            .collect()
    }

    #[test]
    fn controls_by_key_label_or_unique_start() {
        let c = controls();
        assert_eq!(control_key(&c, "hwmon/it87/it87.2608/pwm2").unwrap().as_str(), "hwmon/it87/it87.2608/pwm2");
        assert_eq!(control_key(&c, "cpu COOLER").unwrap().as_str(), "hwmon/it87/it87.2608/pwm1");
        assert_eq!(control_key(&c, "front").unwrap().as_str(), "hwmon/it87/it87.2608/pwm2");
        let e = control_key(&c, "gpu").unwrap_err();
        assert_eq!(e, "\"gpu\" could be GPU fan 1 or GPU fan 2; use more of the name, or the key.");
        let e = control_key(&c, "pump").unwrap_err();
        assert!(e.starts_with("No control is called \"pump\". Controls: CPU cooler, Front intake"), "{e}");
    }
}
