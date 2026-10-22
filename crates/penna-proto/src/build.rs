//! The UI-shaped [`Tick`] made from the control loop's state (DESIGN.md §7.3). Pure: the daemon calls it once a tick, and
//! again after a request changes something, so `state` never shows the past.
use crate::{
    ControlKindName, ControlTick, CurveKindName, CurveTick, DraftInfo, DraftTick, EmergencyTick, FirmwareTick, MixSpec, NvmlRange,
    OverrideTick, ProfileTick, SensorTick, Studio, StudioControl, StudioCurve, StudioProfile, StudioSensor, TachTick, Tick, TriggerSpec,
    VERSION,
};
use penna_core::config::{ControlConfig, CurveKind, DEFAULTS, Measured, words};
use penna_core::control::{LoopState, State};
use penna_core::drivers::{self, Tier};
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
    /// What a studio is previewing, if anything.
    pub draft: Option<DraftTick>,
    /// A setup run's progress, if one runs or waits in review.
    pub setup: Option<penna_core::setup::Progress>,
    /// Drifting controls (§5.6): how much slower than calibrated.
    pub drift: &'a BTreeMap<Key, f64>,
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

    let mut profiles: Vec<ProfileTick> = plan
        .config
        .profiles
        .iter()
        .map(|(id, assign)| ProfileTick {
            id: id.clone(),
            label: words(id),
            builtin: false,
            fans: assign.values().filter(|c| c.as_str() != penna_core::config::FIRMWARE).count() as u32,
        })
        .collect();
    profiles.sort_by_key(|p| profile_rank(&p.id));
    profiles.insert(0, ProfileTick { id: DEFAULTS.to_owned(), label: words(DEFAULTS), builtin: true, fans: 0 });

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
                kind: c.kind.map(ControlKindName::from),
                tier: c.kind.and(tier_of(cx.inventory, key)),
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
                drift: cx.drift.get(key).map(|d| r2(*d)),
            }
        })
        .collect();

    let firmware = cx
        .inventory
        .devices
        .iter()
        .flat_map(|d| &d.controls)
        .filter(|c| !plan.controls.contains_key(&c.key))
        .map(|c| FirmwareTick {
            key: c.key.clone(),
            label: plan.config.labels.get(&c.key).cloned().unwrap_or_else(|| c.label.clone()),
            rpm: cx.readings.rpms.get(&c.key).and_then(Sample::value),
        })
        .collect();

    let curves = plan
        .config
        .curves
        .iter()
        .map(|c| {
            let cs = st.curves.get(&c.id);
            let out = st.outputs.get(&c.id);
            let held = cs.is_some_and(|s| s.held);
            let hysteresis = plan.curves.iter().find(|p| p.id == c.id).map_or(0.0, |p| p.hysteresis);
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
                held,
                hold_until: cs.filter(|_| held).map(|s| r2(s.anchor - hysteresis)),
                error: out.and_then(|o| o.as_ref().err()).cloned(),
            }
        })
        .collect();

    let tachs = cx
        .inventory
        .devices
        .iter()
        .filter(|d| d.source == Source::Hwmon)
        .flat_map(|d| d.tachs.iter().map(move |t| (d, t)))
        .map(|(d, t)| TachTick {
            key: t.key.clone(),
            label: plan.config.labels.get(&t.key).cloned().unwrap_or_else(|| t.label.clone()),
            device: d.model.clone().unwrap_or_else(|| d.label.clone()),
            rpm: cx.readings.rpms.get(&t.key).and_then(Sample::value),
            control: cx.measured.controls.iter().find(|m| m.tachs.contains(&t.key)).map(|m| m.key.clone()),
        })
        .collect();

    Tick {
        v: VERSION,
        seq: cx.seq,
        t: r2(cx.t),
        profile: st.profile.clone(),
        profiles,
        full_speed: st.full_speed,
        draft: cx.draft.clone(),
        emergency,
        sensors,
        controls,
        firmware,
        tachs,
        curves,
        unreachable: cx.inventory.unreachable.clone(),
        notes: cx.inventory.notes.clone(),
        setup: cx.setup.clone().map(Box::new),
    }
}

/// How far the driver behind a control has been checked (§3.6).
fn tier_of(inventory: &Inventory, key: &Key) -> Option<Tier> {
    inventory.devices.iter().find(|d| d.controls.iter().any(|c| &c.key == key)).map(|d| drivers::tier(&d.driver))
}

fn profile_rank(id: &str) -> (usize, String) {
    (PROFILE_ORDER.iter().position(|o| *o == id).unwrap_or(PROFILE_ORDER.len()), id.to_owned())
}

/// Everything the studio body is made of.
pub struct StudioContext<'a> {
    pub plan: &'a Plan,
    pub inventory: &'a Inventory,
    pub measured: &'a Measured,
    /// The sensors as the latest tick shows them.
    pub sensors: &'a [SensorTick],
    pub draft: DraftInfo,
}

/// The studio body (§7.3, M3): the plan's configuration (the draft, while there is one) with the values in force, every
/// control on the machine including those no profile names yet, and what calibration measured for the RPM axis.
pub fn studio(cx: &StudioContext) -> Studio {
    let plan = cx.plan;
    let cfg = &plan.config;
    let active = cfg.profiles.get(&cfg.profile);
    let source_label = |k: &Key| {
        cx.sensors.iter().find(|s| &s.key == k).map(display).or_else(|| cfg.labels.get(k).cloned()).unwrap_or_else(|| k.to_string())
    };

    let mut profiles: Vec<StudioProfile> = cfg
        .profiles
        .iter()
        .map(|(id, assign)| StudioProfile { id: id.clone(), label: words(id), assign: assign.clone(), builtin: false })
        .collect();
    profiles.sort_by_key(|p| profile_rank(&p.id));
    profiles.insert(0, StudioProfile { id: DEFAULTS.to_owned(), label: words(DEFAULTS), assign: BTreeMap::new(), builtin: true });

    let curves = cfg
        .curves
        .iter()
        .map(|c| {
            let p = plan.curves.iter().find(|p| p.id == c.id);
            let mut out = StudioCurve {
                id: c.id.clone(),
                label: Some(c.label()),
                kind: CurveKindName::Flat,
                source: c.kind.source().cloned(),
                source_label: c.kind.source().map(source_label),
                points: None,
                response: None,
                hysteresis: None,
                mix: None,
                duty: None,
                trigger: None,
                used_by: active.map(|a| a.iter().filter(|(_, id)| **id == c.id).map(|(k, _)| k.clone()).collect()).unwrap_or_default(),
            };
            match &c.kind {
                CurveKind::Flat { percent } => out.duty = Some(*percent),
                CurveKind::Graph { points, .. } => {
                    out.kind = CurveKindName::Graph;
                    out.points = Some(points.clone());
                    out.response = p.map(|p| p.response);
                    out.hysteresis = p.map(|p| p.hysteresis);
                }
                CurveKind::Mix { op, curves } => {
                    out.kind = CurveKindName::Mix;
                    out.mix = Some(MixSpec { op: *op, of: curves.clone() });
                }
                CurveKind::Trigger { idle, load, on, off, .. } => {
                    out.kind = CurveKindName::Trigger;
                    out.trigger = Some(TriggerSpec { idle: *idle, load: *load, on: *on, off: *off });
                    out.response = p.map(|p| p.response);
                }
            }
            out
        })
        .collect();

    let mut controls: Vec<StudioControl> = plan.controls.values().map(|c| studio_control(c, cx.inventory, cx.measured)).collect();
    for c in cx.inventory.devices.iter().flat_map(|d| &d.controls).filter(|c| !plan.controls.contains_key(&c.key)) {
        let label = cfg.labels.get(&c.key).cloned().unwrap_or_else(|| c.label.clone());
        let (p, _) = penna_core::plan::control_plan(&ControlConfig::defaults(c.key.clone()), Some((c.kind, label.as_str())), cx.measured);
        controls.push(studio_control(&p, cx.inventory, cx.measured));
    }

    Studio {
        profile: cfg.profile.clone(),
        profiles,
        curves,
        controls,
        sensors: cx
            .sensors
            .iter()
            .map(|s| StudioSensor { key: s.key.clone(), label: s.label.clone(), device: s.device.clone(), is_virtual: s.is_virtual })
            .collect(),
        draft: cx.draft.clone(),
    }
}

fn studio_control(c: &penna_core::plan::ControlPlan, inventory: &Inventory, measured: &Measured) -> StudioControl {
    StudioControl {
        key: c.key.clone(),
        label: c.label.clone(),
        kind: c.kind.map(ControlKindName::from),
        tier: c.kind.and(tier_of(inventory, &c.key)),
        why: c.kind.and_then(ControlKind::undrivable_reason),
        min: c.min,
        max: c.max,
        offset: c.offset,
        step_up: c.step_up,
        step_down: c.step_down,
        may_stop: c.may_stop,
        stop: c.stop,
        start: r1(c.start),
        floor: r1(c.floor),
        failsafe: c.failsafe,
        nvml: match c.kind {
            Some(ControlKind::Nvml { min, max }) => Some(NvmlRange { min, max }),
            _ => None,
        },
        calibration: measured.get(&c.key).map(|m| m.calibration.clone()).filter(|t| !t.is_empty()),
    }
}

fn is_motherboard(driver: &str) -> bool {
    matches!(driver, "asus_ec_sensors" | "gigabyte_wmi") || drivers::lookup(driver).is_some_and(|e| e.class == drivers::Class::Motherboard)
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
        (_, d) if is_motherboard(d) => 2,
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
