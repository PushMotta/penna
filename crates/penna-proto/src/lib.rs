//! Penna's wire format, version 2 (DESIGN.md §7.3): newline-delimited JSON over a Unix socket. The daemon speaks first with
//! [`Hello`]; a client sends [`Request`]s and gets a [`Reply`] for each; a subscriber also gets [`History`] once, then a
//! [`Tick`] every tick and a [`Transition`] for everything that changes. Events are tagged by `ev`, requests by `op`.
//!
//! [`build::tick`] makes the UI-shaped [`Tick`] from the control loop's state: everything a surface draws, with no second
//! request. [`hardware`] is the machine alone, for `penna watch` when no daemon answers.
pub mod build;
pub mod hardware;

use penna_core::control::{State, Trend};
use penna_core::{Key, Limit, Unreachable};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const VERSION: u32 = 2;
/// The longest line either side sends.
pub const MAX_LINE: usize = 1 << 20;
/// Ticks of history a subscriber gets first (§8.6: full sparklines on the first frame).
pub const HISTORY: usize = 300;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
    Hello(Hello),
    Tick(Tick),
    History(History),
    Event(Transition),
    /// `penna watch --json` without the daemon: the hardware alone.
    Hardware(hardware::Snapshot),
}

impl Event {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("events always serialize")
    }
}

/// The daemon's first line on every connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub v: u32,
    pub daemon: String,
    pub mock: bool,
    /// Seconds between ticks.
    pub tick: f64,
    /// The peer-credential decision (§7.4): surfaces disable what changes fans when it is false.
    pub can_write: bool,
    /// When `can_write` is false: why, and what to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One line from a client. `id` is the client's own and comes back on the reply, whatever JSON it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Op {
    #[serde(rename = "state")]
    State,
    #[serde(rename = "subscribe")]
    Subscribe,
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
    #[serde(rename = "config")]
    Config,
    #[serde(rename = "profile.set")]
    ProfileSet { name: String },
    #[serde(rename = "full_speed")]
    FullSpeed { on: bool },
    /// `control` omitted: every control. `duty` null or omitted: clear. `seconds` omitted: until cleared.
    #[serde(rename = "override")]
    Override {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        control: Option<String>,
        #[serde(default)]
        duty: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<f64>,
    },
    #[serde(rename = "retry")]
    Retry { control: String },
    #[serde(rename = "adopt")]
    Adopt { control: String, curve: String },
    #[serde(rename = "release")]
    Release { control: String },
    #[serde(rename = "label")]
    Label { key: String, label: String },
    /// Everything static the curve editor needs, with this connection's draft applied (M3).
    #[serde(rename = "studio")]
    Studio,
    #[serde(rename = "draft.put_curve")]
    DraftPutCurve { curve: StudioCurve },
    /// The curve's id is `curve`: `id` is the request's own (§7.3).
    #[serde(rename = "draft.delete_curve")]
    DraftDeleteCurve { curve: String },
    /// `curve` is a curve id or `"firmware"`; applies to the active profile.
    #[serde(rename = "draft.assign")]
    DraftAssign { control: String, curve: String },
    #[serde(rename = "draft.limits")]
    DraftLimits {
        control: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_down: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        may_stop: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failsafe: Option<f64>,
    },
    #[serde(rename = "draft.save")]
    DraftSave,
    #[serde(rename = "draft.discard")]
    DraftDiscard,
}

impl Op {
    /// Every operation this version knows, in the order the protocol lists them.
    pub const NAMES: &[&str] = &[
        "state",
        "subscribe",
        "unsubscribe",
        "config",
        "profile.set",
        "full_speed",
        "override",
        "retry",
        "adopt",
        "release",
        "label",
        "studio",
        "draft.put_curve",
        "draft.delete_curve",
        "draft.assign",
        "draft.limits",
        "draft.save",
        "draft.discard",
    ];

    /// Changes fans or the configuration, so it needs `can_write`.
    pub fn writes(&self) -> bool {
        !matches!(self, Op::State | Op::Subscribe | Op::Unsubscribe | Op::Config | Op::Studio)
    }

    /// Answered by the control loop, which knows the plan and the drafts, rather than by the socket from what was published.
    pub fn needs_loop(&self) -> bool {
        !matches!(self, Op::State | Op::Subscribe | Op::Unsubscribe | Op::Config)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Op::State => "state",
            Op::Subscribe => "subscribe",
            Op::Unsubscribe => "unsubscribe",
            Op::Config => "config",
            Op::ProfileSet { .. } => "profile.set",
            Op::FullSpeed { .. } => "full_speed",
            Op::Override { .. } => "override",
            Op::Retry { .. } => "retry",
            Op::Adopt { .. } => "adopt",
            Op::Release { .. } => "release",
            Op::Label { .. } => "label",
            Op::Studio => "studio",
            Op::DraftPutCurve { .. } => "draft.put_curve",
            Op::DraftDeleteCurve { .. } => "draft.delete_curve",
            Op::DraftAssign { .. } => "draft.assign",
            Op::DraftLimits { .. } => "draft.limits",
            Op::DraftSave => "draft.save",
            Op::DraftDiscard => "draft.discard",
        }
    }
}

impl Request {
    /// A client's line, or the reply that explains what is wrong with it. The id is recovered whenever the line is JSON.
    #[allow(clippy::result_large_err)]
    pub fn parse(line: &str) -> Result<Request, Reply> {
        let value: Value = serde_json::from_str(line).map_err(|e| Reply::error(None, format!("That line isn't JSON: {e}.")))?;
        let id = value.get("id").cloned();
        let Some(obj) = value.as_object() else {
            return Err(Reply::error(id, "A request is a JSON object with an \"op\", like {\"id\":1,\"op\":\"state\"}."));
        };
        let op = match obj.get("op") {
            Some(Value::String(op)) => op.clone(),
            _ => return Err(Reply::error(id, "A request needs an \"op\", like {\"id\":1,\"op\":\"state\"}.")),
        };
        if !Op::NAMES.contains(&op.as_str()) {
            return Err(Reply::error(id, format!("Penna doesn't know the operation {op:?}. It knows: {}.", Op::NAMES.join(", "))));
        }
        serde_json::from_value::<Request>(value).map_err(|e| Reply::error(id, explain(&op, &e.to_string())))
    }
}

/// serde's words for a bad argument, as a sentence about this operation.
fn explain(op: &str, e: &str) -> String {
    let e = e.split(" at line").next().unwrap_or(e);
    if let Some(field) = e.strip_prefix("missing field `").and_then(|r| r.strip_suffix('`')) {
        return format!("{op} needs \"{field}\".");
    }
    if let Some(rest) = e.strip_prefix("invalid type: ") {
        return format!("{op} got the wrong kind of value: {rest}.");
    }
    format!("{op}: {e}.")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: Option<Value>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn ok(id: Option<Value>, result: Option<Value>) -> Reply {
        Reply { id, ok: true, result, error: None }
    }
    pub fn error(id: Option<Value>, error: impl Into<String>) -> Reply {
        Reply { id, ok: false, result: None, error: Some(error.into()) }
    }
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("replies always serialize")
    }
}

/// Everything a surface draws, once a tick (§7.3). Every field is always present; a value that is not known is `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tick {
    pub v: u32,
    pub seq: u64,
    /// Unix time in seconds.
    pub t: f64,
    pub profile: String,
    pub profiles: Vec<ProfileTick>,
    pub full_speed: bool,
    /// What a studio is previewing, or `null` (M3).
    pub draft: Option<DraftTick>,
    pub emergency: Vec<EmergencyTick>,
    pub sensors: Vec<SensorTick>,
    /// The controls the configuration knows.
    pub controls: Vec<ControlTick>,
    /// Every other control on the machine: its firmware runs it.
    pub firmware: Vec<FirmwareTick>,
    pub curves: Vec<CurveTick>,
    pub unreachable: Vec<Unreachable>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileTick {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmergencyTick {
    pub sensor: Key,
    pub label: String,
    pub celsius: Option<f64>,
    pub limit: f64,
    /// The emergency ends below this.
    pub release: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorTick {
    pub key: Key,
    pub label: String,
    /// The device it belongs to, as a person calls it ("AMD CPU", "NVIDIA GeForce RTX 3090"); "Virtual" for a virtual sensor.
    pub device: String,
    pub celsius: Option<f64>,
    /// The sparkline's fixed range, lowest and highest °C (§11.1, call 5).
    pub scale: [f64; 2],
    pub limits: Vec<Limit>,
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
    pub below_ambient: bool,
    /// Why there is no value.
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlKindName {
    Pwm,
    Nvml,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlTick {
    pub key: Key,
    pub label: String,
    /// `None`: named by the configuration but not on this machine.
    pub kind: Option<ControlKindName>,
    /// Paired tachs: the fans this control drives that Penna can hear.
    pub fans: u32,
    /// The mean of the paired tachs that read, or `null`.
    pub rpm: Option<u32>,
    pub rpms: Vec<Option<u32>>,
    pub state: State,
    pub reason: Option<String>,
    /// PWM duty for hwmon, NVML percent for NVML.
    pub duty: f64,
    pub target: f64,
    pub trend: Trend,
    /// The curve the active profile gives it, or `null` for firmware.
    pub curve: Option<String>,
    pub curve_label: Option<String>,
    pub floor: f64,
    pub may_stop: bool,
    #[serde(rename = "override")]
    pub override_: Option<OverrideTick>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverrideTick {
    pub duty: f64,
    /// Unix time it ends, or `null`: until cleared.
    pub until: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareTick {
    pub key: Key,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CurveKindName {
    Flat,
    Graph,
    Mix,
    Trigger,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurveTick {
    pub id: String,
    pub label: String,
    pub kind: CurveKindName,
    pub source: Option<Key>,
    /// The source after response time (§4.4).
    pub smoothed: Option<f64>,
    pub out: Option<f64>,
    /// Hysteresis is holding the output.
    pub held: bool,
    /// While held: the temperature below which the output may fall (§4.4); otherwise `null`.
    pub hold_until: Option<f64>,
    pub error: Option<String>,
}

/// The changes some connection is previewing (§7.4): the curves and controls a draft touches.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DraftTick {
    pub curves: Vec<String>,
    pub controls: Vec<Key>,
}

/// Everything static the curve editor needs, with the connection's draft applied (§7.3, M3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Studio {
    pub profile: String,
    pub profiles: Vec<StudioProfile>,
    pub curves: Vec<StudioCurve>,
    pub controls: Vec<StudioControl>,
    pub sensors: Vec<StudioSensor>,
    pub draft: DraftInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StudioProfile {
    pub id: String,
    pub label: String,
    /// Control key → curve id, or `"firmware"`.
    pub assign: BTreeMap<Key, String>,
}

/// A curve as the studio shows it and sends it back. Only the fields of its kind are present. On the way in, `label`,
/// `response` and `hysteresis` may be left out to keep what the curve had, and `source_label` and `used_by` are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StudioCurve {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub kind: CurveKindName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 2]>>,
    /// Seconds of smoothing on the source; in the studio body, the value in force (default included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<f64>,
    /// °C; in the studio body, the value in force (default included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hysteresis: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<MixSpec>,
    /// A flat curve's duty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerSpec>,
    /// The controls the active profile gives this curve.
    #[serde(default)]
    pub used_by: Vec<Key>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MixSpec {
    pub op: penna_core::config::MixOp,
    pub of: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerSpec {
    pub idle: f64,
    pub load: f64,
    pub on: f64,
    pub off: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StudioControl {
    pub key: Key,
    pub label: String,
    pub kind: Option<ControlKindName>,
    pub min: f64,
    pub max: f64,
    pub offset: f64,
    pub step_up: f64,
    pub step_down: f64,
    pub may_stop: bool,
    /// Calibrated: the lowest duty that still turns the fan; `null` before calibration.
    pub stop: Option<f64>,
    pub start: f64,
    pub floor: f64,
    pub failsafe: f64,
    /// An NVIDIA fan's settable range, else `null`.
    pub nvml: Option<NvmlRange>,
    /// Measured duty → RPM, ascending (§5.2), or `null` before calibration.
    pub calibration: Option<Vec<[f64; 2]>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvmlRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StudioSensor {
    pub key: Key,
    pub label: String,
    pub device: String,
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DraftInfo {
    /// This connection holds the draft.
    pub mine: bool,
    /// The draft differs from the saved configuration.
    pub dirty: bool,
    pub curves: Vec<String>,
    pub controls: Vec<Key>,
}

/// The last ticks, oldest first, sent once after `subscribe`. `null` is a failed read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    pub seq: u64,
    pub tick: f64,
    pub sensors: BTreeMap<Key, Vec<Option<f64>>>,
    pub controls: BTreeMap<Key, ControlHistory>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ControlHistory {
    pub duty: Vec<Option<f64>>,
    pub rpm: Vec<Option<u32>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Alert,
}

/// Something that changed: what the mock trace prints, for the CLI and notifications.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub seq: u64,
    pub t: f64,
    pub level: Level,
    pub control: Option<Key>,
    pub text: String,
}

/// Where a client finds the daemon (§7.3): `$PENNA_SOCKET`, then production, then where `pennad --mock` listens.
pub fn socket_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("PENNA_SOCKET").filter(|p| !p.is_empty()) {
        out.push(p.into());
    }
    out.push(PRODUCTION_SOCKET.into());
    if let Some(mock) = mock_socket() {
        out.push(mock);
    }
    out
}

pub const PRODUCTION_SOCKET: &str = "/run/penna/penna.sock";

/// `$XDG_RUNTIME_DIR/penna/penna.sock`, where `pennad --mock` listens unless told otherwise.
pub fn mock_socket() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty()).map(|d| std::path::PathBuf::from(d).join("penna/penna.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_with_their_arguments() {
        let r = Request::parse(r#"{"id":7,"op":"profile.set","name":"render"}"#).unwrap();
        assert_eq!(r.id, Some(Value::from(7)));
        assert_eq!(r.op, Op::ProfileSet { name: "render".into() });
        let r = Request::parse(r#"{"id":"a","op":"override","duty":null}"#).unwrap();
        assert_eq!(r.op, Op::Override { control: None, duty: None, seconds: None });
        assert!(Request::parse(r#"{"op":"state"}"#).unwrap().id.is_none());
        assert!(r.op.writes() && !Op::State.writes());
    }

    #[test]
    fn studio_requests_parse_and_read_or_write() {
        let r = Request::parse(r#"{"id":1,"op":"studio"}"#).unwrap();
        assert!(r.op == Op::Studio && !r.op.writes() && r.op.needs_loop());
        // A curve straight from the studio body, with what the daemon ignores on the way in.
        let line = r#"{"id":2,"op":"draft.put_curve","curve":{"id":"cpu-quiet","label":"CPU quiet","kind":"graph",
            "source":"hwmon/k10temp/0000:00:18.3/temp1","source_label":"AMD CPU · Tctl","points":[[45,30],[88,100]],
            "response":5.0,"hysteresis":3.0,"used_by":["hwmon/it87/it87.2608/pwm1"]}}"#;
        let Op::DraftPutCurve { curve } = Request::parse(line).unwrap().op else { panic!("not a put_curve") };
        assert_eq!((curve.kind, curve.points.as_deref()), (CurveKindName::Graph, Some(&[[45.0, 30.0], [88.0, 100.0]][..])));
        let r = Request::parse(r#"{"id":3,"op":"draft.limits","control":"CPU cooler","min":35,"may_stop":false}"#).unwrap();
        let Op::DraftLimits { min, max, may_stop, .. } = r.op else { panic!("not limits") };
        assert_eq!((min, max, may_stop), (Some(35.0), None, Some(false)));
        for op in ["draft.save", "draft.discard"] {
            assert!(Request::parse(&format!(r#"{{"op":"{op}"}}"#)).unwrap().op.writes(), "{op}");
        }
        let e = Request::parse(r#"{"id":4,"op":"draft.assign","control":"CPU cooler"}"#).unwrap_err();
        assert_eq!(e.error.as_deref(), Some("draft.assign needs \"curve\"."));
    }

    #[test]
    fn bad_lines_get_sentences_and_keep_their_id() {
        let e = Request::parse(r#"{"id":3,"op":"turbo"}"#).unwrap_err();
        assert_eq!(e.id, Some(Value::from(3)));
        assert!(e.error.as_deref().unwrap().starts_with("Penna doesn't know the operation \"turbo\""), "{e:?}");
        let e = Request::parse(r#"{"id":4,"op":"profile.set"}"#).unwrap_err();
        assert_eq!(e.error.as_deref(), Some("profile.set needs \"name\"."));
        let e = Request::parse("nope").unwrap_err();
        assert!(e.id.is_none() && e.error.as_deref().unwrap().starts_with("That line isn't JSON"));
        let e = Request::parse(r#"{"id":5,"op":"full_speed","on":"yes"}"#).unwrap_err();
        assert!(e.error.as_deref().unwrap().starts_with("full_speed got the wrong kind of value"), "{e:?}");
    }

    #[test]
    fn replies_are_one_line_and_echo_the_id() {
        assert_eq!(Reply::ok(Some(Value::from(7)), None).line(), r#"{"id":7,"ok":true}"#);
        assert_eq!(Reply::error(None, "No.").line(), r#"{"id":null,"ok":false,"error":"No."}"#);
    }

    #[test]
    fn hello_matches_the_protocol() {
        let h = Event::Hello(Hello { v: 2, daemon: "0.2.0".into(), mock: true, tick: 1.0, can_write: true, why: None });
        assert_eq!(h.line(), r#"{"ev":"hello","v":2,"daemon":"0.2.0","mock":true,"tick":1.0,"can_write":true}"#);
    }

    /// The tick's shape is the contract with the shell plugin: a golden fixture pins it, field for field.
    #[test]
    fn the_golden_tick_round_trips() {
        let text = include_str!("../tests/fixtures/tick.json");
        let golden: Value = serde_json::from_str(text).unwrap();
        let ev: Event = serde_json::from_value(golden.clone()).expect("the golden tick parses");
        assert!(matches!(ev, Event::Tick(_)));
        assert_eq!(serde_json::to_value(&ev).unwrap(), golden, "serializing changed the shape");
    }

    #[test]
    fn history_and_events_are_tagged() {
        let e = Event::Event(Transition { seq: 1, t: 2.0, level: Level::Alert, control: None, text: "EMERGENCY".into() });
        assert_eq!(e.line(), r#"{"ev":"event","seq":1,"t":2.0,"level":"alert","control":null,"text":"EMERGENCY"}"#);
        let h = Event::History(History::default());
        assert!(h.line().starts_with(r#"{"ev":"history","seq":0,"tick":0.0,"sensors":{}"#), "{}", h.line());
    }
}
