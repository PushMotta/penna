//! The UI-shaped [`Tick`] made from the control loop's state (DESIGN.md §7.3). Pure: the daemon calls it once a tick, and
//! again after a request changes something, so `state` never shows the past.
use crate::{
    ControlKindName, ControlTick, CurveKindName, CurveTick, EmergencyTick, FirmwareTick, OverrideTick, ProfileTick, SensorTick, Tick,
    VERSION,
};
use penna_core::config::{CurveKind, Measured, words};
use penna_core::control::{LoopState, State};
use penna_core::plan::Plan;
use penna_core::{ControlKind, Inventory, Key, LimitKind, Readings, Sample, Source};
use std::collections::BTreeMap;

/// Everything one tick is made of.
pub struct Context<'a> {
    pub plan: &'a Plan,
    pub state: &'a LoopState,
    pub readings: &'a Readings,
    pub inventory: &'a Inventory,
    pub measured: &'a Measured,
    pub seq: u64,
    /// Unix time, seconds.
    pub t: f64,
    /// The loop's own clock, which override deadlines use.
    pub now: f64,
    /// Loop seconds per wall-clock second: 1, except on a mock desk running faster than real time.
    pub rate: f64,
}

/// A sparkline starts here (§11.1, call 5).
pub const SCALE_FLOOR: f64 = 20.0;
const SCALE_TOP: f64 = 100.0;

/// The order profiles are offered in when their names say how hard they push; any other name follows, alphabetically.
const PROFILE_ORDER: &[&str] = &["silent", "quiet", "balanced", "normal", "performance", "render", "full"];

pub fn tick(cx: &Context) -> Tick {
    let plan = cx.plan;
    let st = cx.state;
    let sensors = sensors(cx);
    let label_of = |k: &Key| sensors.iter().find(|s| &s.key == k).map(display).unwrap_or_else(|| k.to_string());

    let mut profiles: Vec<ProfileTick> = plan.config.profiles.keys().map(|id| ProfileTick { id: id.clone(), label: words(id) }).collect();
    profiles.sort_by_key(|p| (PROFILE_ORDER.iter().position(|o| *o == p.id).unwrap_or(PROFILE_ORDER.len()), p.id.clone()));

    let emergency = st
        .emergency
        .iter()
        .map(|(k, limit)| EmergencyTick {
            sensor: k.clone(),
            label: label_of(k),
            celsius: st.sources.get(k).and_then(|r| r.as_ref().ok()).copied().map(r2),
            limit: *limit,
            release: limit - penna_core::control::EMERGENCY_RELEASE,
        })
        .collect();

    let controls = plan
        .controls
        .iter()
        .map(|(key, c)| {
            let cs = st.controls.get(key).cloned().unwrap_or_default();
            let tachs = tachs(cx, key, c.kind);
            let rpms: Vec<Option<u32>> = tachs.iter().map(|t| cx.readings.rpms.get(t).and_then(Sample::value)).collect();
            let read: Vec<u32> = rpms.iter().flatten().copied().collect();
            let rpm = (!read.is_empty()).then(|| (read.iter().map(|&r| r as f64).sum::<f64>() / read.len() as f64).round() as u32);
            let curve = plan.assignment(&st.profile, key).map(str::to_owned);
            let ov = st.overrides.get(key).or(st.override_all.as_ref());
            ControlTick {
                key: key.clone(),
                label: c.label.clone(),
                kind: c.kind.map(|k| match k {
                    ControlKind::Pwm => ControlKindName::Pwm,
                    ControlKind::Nvml { .. } => ControlKindName::Nvml,
                }),
                fans: tachs.len() as u32,
                rpm,
                rpms,
                state: if c.kind.is_none() { State::Absent } else { cs.state },
                reason: cs.reason.clone(),
                duty: r1(cs.duty),
                target: r1(cs.target),
                trend: cs.trend,
                curve_label: curve.as_deref().and_then(|id| plan.config.curve(id)).map(|c| c.label()),
                curve,
                floor: r1(c.floor),
                may_stop: c.may_stop,
                override_: ov.map(|o| OverrideTick { duty: r1(o.duty), until: o.until.map(|u| r2(cx.t + (u - cx.now) / cx.rate)) }),
            }
        })
        .collect();

    let firmware = cx
        .inventory
        .devices
        .iter()
        .flat_map(|d| &d.controls)
        .filter(|c| !plan.controls.contains_key(&c.key))
        .map(|c| FirmwareTick { key: c.key.clone(), label: plan.config.labels.get(&c.key).cloned().unwrap_or_else(|| c.label.clone()) })
        .collect();

    let curves = plan
        .config
        .curves
        .iter()
        .map(|c| {
            let cs = st.curves.get(&c.id);
            let out = st.outputs.get(&c.id);
            CurveTick {
                id: c.id.clone(),
                label: c.label(),
                kind: match c.kind {
                    CurveKind::Flat { .. } => CurveKindName::Flat,
                    CurveKind::Graph { .. } => CurveKindName::Graph,
                    CurveKind::Mix { .. } => CurveKindName::Mix,
                    CurveKind::Trigger { .. } => CurveKindName::Trigger,
                },
                source: c.kind.source().cloned(),
                smoothed: cs.and_then(|s| s.smoothed).map(r2),
                out: out.and_then(|o| o.as_ref().ok()).copied().map(r1),
                held: cs.is_some_and(|s| s.held),
                error: out.and_then(|o| o.as_ref().err()).cloned(),
            }
        })
        .collect();

    Tick {
        v: VERSION,
        seq: cx.seq,
        t: r2(cx.t),
        profile: st.profile.clone(),
        profiles,
        full_speed: st.full_speed,
        emergency,
        sensors,
        controls,
        firmware,
        curves,
        unreachable: cx.inventory.unreachable.clone(),
        notes: cx.inventory.notes.clone(),
    }
}

/// "AMD CPU · Tctl", or a virtual sensor's own label.
pub fn display(s: &SensorTick) -> String {
    if s.is_virtual { s.label.clone() } else { format!("{} · {}", s.device, s.label) }
}

/// Where a device's sensors go in the list: CPUs first, then GPUs, motherboard, storage, anything else, virtual sensors, and
/// sensors that only read below room temperature last.
fn rank(source: Source, driver: &str) -> u8 {
    match (source, driver) {
        (_, "k10temp" | "zenpower" | "coretemp") => 0,
        (Source::Nvml, _) | (_, "amdgpu") => 1,
        (_, "it87" | "nct6775" | "nct6683" | "w83627ehf" | "f71882fg") => 2,
        (_, "nvme" | "drivetemp") => 3,
        _ => 4,
    }
}

fn sensors(cx: &Context) -> Vec<SensorTick> {
    let plan = cx.plan;
    let mut out = Vec::new();
    let mut tops: BTreeMap<Key, f64> = BTreeMap::new();
    let mut devices: Vec<&penna_core::Device> = cx.inventory.devices.iter().collect();
    devices.sort_by_key(|d| (d.sensors.iter().all(|s| s.below_ambient), rank(d.source, &d.driver)));
    for d in devices {
        for s in &d.sensors {
            let (celsius, error) = match cx.readings.temps.get(&s.key) {
                Some(Sample::Value(v)) => (Some(r2(*v)), None),
                Some(Sample::Implausible(v)) => (None, Some(format!("read {v}, which is not a temperature"))),
                Some(Sample::Error(e)) => (None, Some(e.clone())),
                None => (None, Some("not read".to_owned())),
            };
            let own = [LimitKind::Crit, LimitKind::GpuMax, LimitKind::Max]
                .iter()
                .find_map(|kind| s.limits.iter().find(|l| l.kind == *kind).map(|l| l.celsius));
            let top = plan.emergency.get(&s.key).copied().or(own).map_or(SCALE_TOP, scale_top);
            tops.insert(s.key.clone(), top);
            out.push(SensorTick {
                key: s.key.clone(),
                label: plan.config.labels.get(&s.key).cloned().unwrap_or_else(|| s.label.clone()),
                device: d.label.clone(),
                celsius,
                scale: [SCALE_FLOOR, top],
                limits: s.limits.clone(),
                is_virtual: false,
                below_ambient: s.below_ambient,
                error,
            });
        }
    }
    for (key, kind) in &plan.virtuals {
        let top =
            kind.inputs().iter().filter_map(|k| tops.get(*k)).copied().fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))));
        let top = top.unwrap_or(SCALE_TOP);
        tops.insert(key.clone(), top);
        let (celsius, error) = match cx.state.sources.get(key) {
            Some(Ok(v)) => (Some(r2(*v)), None),
            Some(Err(e)) => (None, Some(e.clone())),
            None => (None, Some("not read yet".to_owned())),
        };
        let v = plan.config.virtual_sensor(key);
        out.push(SensorTick {
            key: key.clone(),
            label: v.and_then(|v| v.label.clone()).unwrap_or_else(|| words(key.channel())),
            device: "Virtual".to_owned(),
            celsius,
            scale: [SCALE_FLOOR, top],
            limits: Vec::new(),
            is_virtual: true,
            below_ambient: false,
            error,
        });
    }
    out
}

/// The tachs a control drives: pairing for hwmon, the fan itself for NVML.
fn tachs(cx: &Context, key: &Key, kind: Option<ControlKind>) -> Vec<Key> {
    match kind {
        Some(ControlKind::Nvml { .. }) => vec![key.clone()],
        _ => cx.measured.get(key).map(|m| m.tachs.clone()).unwrap_or_default(),
    }
}

/// A limit plus 5 °C, up to the next multiple of 5.
fn scale_top(limit: f64) -> f64 {
    ((limit + 5.0) / 5.0).ceil() * 5.0
}

fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_end_five_degrees_above_the_limit_on_a_multiple_of_five() {
        assert_eq!(scale_top(90.0), 95.0);
        assert_eq!(scale_top(93.0), 100.0);
        assert_eq!(scale_top(84.85), 90.0);
    }
}
