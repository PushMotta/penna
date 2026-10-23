//! The platform profile as a fan lever (DESIGN.md §3.5, §4.7). On a machine where it is the only lever, with no fan Penna can give
//! a curve, each Penna profile sets a power profile, and Defaults puts back the one that was set before Penna changed it. The way
//! back is written ahead like any held control's, and to `firmware.json` too, since a power profile outlives a reboot (§6.7).
//! Penna sets it only on a switch: a power profile chosen elsewhere in between is left alone until the next one.
use crate::trace::Trace;
use penna_core::{ControlKind, Inventory, Key};
use penna_hw::held::{self, Held};
use penna_hw::platform::{Platform, for_profile};
use penna_proto::Level;
use std::collections::BTreeMap;

pub struct Lever {
    platform: Option<Platform>,
    /// The power profile Penna last set.
    applied: Option<String>,
    /// The last failure said, so a failure that repeats every tick is said once.
    said: Option<String>,
}

impl Lever {
    pub fn new(platform: Option<Platform>) -> Lever {
        Lever { platform, applied: None, said: None }
    }

    /// Whether the power profile is this machine's only fan lever: a power profile to set, and no fan Penna can give a curve.
    /// A fan with modes doesn't count: it has only automatic and full speed.
    pub fn engaged(&self, inventory: &Inventory) -> bool {
        self.platform.is_some()
            && !inventory.devices.iter().flat_map(|d| &d.controls).any(|c| c.kind.drivable() && c.kind != ControlKind::Modes)
    }

    /// The power profile each of `profiles` sets, where the power profile is the only lever (§4.7): for the protocol.
    pub fn profiles<'a>(&self, inventory: &Inventory, profiles: impl Iterator<Item = &'a String>) -> BTreeMap<String, String> {
        match (&self.platform, self.engaged(inventory)) {
            (Some(p), true) => profiles.filter_map(|id| Some((id.clone(), for_profile(id, p.choices(), p.daemon())?))).collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Sets the power profile `profile` asks for when it changed, or puts back the one Penna found.
    pub fn step(&mut self, inventory: &Inventory, profile: &str, held: &mut Held, out: &mut Trace, now: f64) {
        let engaged = self.engaged(inventory);
        let Some(p) = &self.platform else { return };
        let key = Key::platform_profile();
        let want = if engaged { for_profile(profile, p.choices(), p.daemon()) } else { None };
        let result = match want {
            Some(w) if self.applied.as_deref() == Some(w.as_str()) => Ok(None),
            Some(w) => (|| {
                if held.get(&key).is_none() {
                    held.add(p.original()?)?;
                }
                p.set(&w)?;
                Ok::<_, anyhow::Error>(Some(format!("Power profile: {w}, for {profile}")))
            })()
            .inspect(|_| self.applied = Some(w.clone())),
            None => match held.get(&key).cloned() {
                Some(entry) => held::restore(&entry).and_then(|s| held.remove(&key).map(|()| Some(format!("Power profile back: {s}")))),
                None => Ok(None),
            }
            .inspect(|_| self.applied = None),
        };
        match result {
            Ok(Some(text)) => {
                self.said = None;
                out.note(now, Level::Info, None, text);
            }
            Ok(None) => {}
            Err(e) => {
                let text = format!("Couldn't set the power profile: {e:#}");
                if self.said.as_deref() != Some(text.as_str()) {
                    out.note(now, Level::Alert, None, text.clone());
                    self.said = Some(text);
                }
            }
        }
    }
}
