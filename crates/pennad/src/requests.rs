//! Requests that change fans or the configuration, carried out by the control loop between ticks (DESIGN.md §7.1, §7.3).
//! Runtime requests (`full_speed`, `override`, `retry`) change the loop's state only; configuration requests (`profile.set`,
//! `adopt`, `release`, `label`) are checked against the machine as a whole new plan, then saved atomically, then used.
use crate::Live;
use crate::trace::Trace;
use penna_core::config::{self, Config, ControlConfig, FIRMWARE, LABEL_MAX, words};
use penna_core::control::{Request, Scope};
use penna_core::plan::Plan;
use penna_core::{Inventory, Key};
use penna_proto::{Level, Op};
use serde_json::Value;
use std::path::Path;

/// The longest temporary override: a day.
const MAX_OVERRIDE: f64 = 86_400.0;

/// Carries out one request. The reply's result is `{"text": …}`: what changed, in a sentence, for the CLI to print.
pub fn handle(live: &mut Live, inventory: &Inventory, out: &mut Trace, op: Op, uid: u32) -> Result<Option<Value>, String> {
    let now = live.now;
    let mut said = String::new();
    let mut note = |out: &mut Trace, control: Option<Key>, text: String| {
        out.note(now, Level::Info, control, format!("{text} (uid {uid})"));
        said = text;
    };
    match op {
        Op::ProfileSet { name } => {
            let id = profile(&live.plan.config, &name)?;
            let mut cfg = live.plan.config.clone();
            cfg.profile = id.clone();
            commit(live, inventory, cfg)?;
            live.state.apply(&live.plan, Request::Profile(id.clone()), now)?;
            note(out, None, format!("Profile: {}", words(&id)));
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
            let id = curve_id(&live.plan.config, &curve)?;
            let mut cfg = live.plan.config.clone();
            let active = cfg.profile.clone();
            cfg.profiles.entry(active.clone()).or_default().insert(key.clone(), id.clone());
            commit(live, inventory, cfg)?;
            let label = live.plan.controls.get(&key).map_or_else(|| key.to_string(), |c| c.label.clone());
            note(out, Some(key), format!("{label} follows {} in {}", curve_label(&live.plan.config, &id), words(&active)));
        }
        Op::Release { control } => {
            let key = control_key(&every_control(live, inventory), &control)?;
            let mut cfg = live.plan.config.clone();
            let active = cfg.profile.clone();
            cfg.profiles.entry(active.clone()).or_default().insert(key.clone(), FIRMWARE.to_owned());
            commit(live, inventory, cfg)?;
            let label = live.plan.controls.get(&key).map_or_else(|| key.to_string(), |c| c.label.clone());
            note(out, Some(key), format!("{label} goes back to its firmware in {}", words(&active)));
        }
        Op::Label { key, label } => {
            let label = label.trim().to_owned();
            if label.is_empty() || label.chars().count() > LABEL_MAX {
                return Err(format!("A label is 1 to {LABEL_MAX} characters."));
            }
            let key = labelled_key(live, inventory, &key)?;
            let mut cfg = live.plan.config.clone();
            let was = relabel(&mut cfg, inventory, &key, &label)?;
            commit(live, inventory, cfg)?;
            note(out, Some(key), format!("{was} is now called {label}"));
        }
        Op::State | Op::Subscribe | Op::Unsubscribe | Op::Config => {
            return Err(format!("{} is answered by the socket, not the loop.", op.name()));
        }
    }
    Ok(Some(serde_json::json!({ "text": said })))
}

/// A new configuration becomes the plan only if it fits the machine, and only once it is saved.
fn commit(live: &mut Live, inventory: &Inventory, cfg: Config) -> Result<(), String> {
    let plan = Plan::new(cfg.clone(), inventory, &live.measured, live.cpu_model.as_deref()).map_err(|problems| {
        let list: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        format!("That would make the configuration invalid: {}.", list.join("; "))
    })?;
    if let Some(path) = &live.config_path {
        save(path, &cfg).map_err(|e| format!("Couldn't save the configuration to {}: {e:#}.", path.display()))?;
    }
    live.plan = plan;
    Ok(())
}

fn save(path: &Path, cfg: &Config) -> anyhow::Result<()> {
    let text = format!(
        "# Penna's configuration (DESIGN.md §7.5). pennad rewrites this file when a surface changes it: edit it while pennad is\n\
         # stopped, then run `penna check`.\n{}",
        cfg.to_toml()
    );
    penna_hw::held::write_atomically(path, text.as_bytes())
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
    cfg.profiles
        .keys()
        .find(|id| id.as_str() == name || id.to_lowercase() == lower || words(id).to_lowercase() == lower)
        .cloned()
        .ok_or_else(|| {
            let ids: Vec<&str> = cfg.profiles.keys().map(String::as_str).collect();
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
