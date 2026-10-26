//! The configuration a person chose (DESIGN.md §7.5), as read from `/etc/penna/config.toml`, and the checks `penna check` runs.
//! Measurements (pairing, calibration) are facts about the hardware and live apart, in [`Measured`].
use crate::Key;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const VERSION: u32 = 1;
/// The curve name a profile uses to leave a control with its firmware.
pub const FIRMWARE: &str = "firmware";
/// The built-in profile that leaves every control with its firmware (DESIGN.md §4.7). It is never stored in `profiles`, so it
/// can't be edited, renamed or deleted.
pub const DEFAULTS: &str = "defaults";
/// The longest label a person can give a sensor, tach or control, and the longest curve id or profile name.
pub const LABEL_MAX: usize = 48;
/// How much one configuration may hold. Any member of the penna group can add to it, and every client is sent all of it each
/// tick (§7.4), so these are far above what any machine needs and keep the largest tick well under a protocol line.
pub const CURVES_MAX: usize = 128;
pub const POINTS_MAX: usize = 32;
pub const MIX_MAX: usize = 16;
pub const VIRTUAL_MAX: usize = 64;
pub const PROFILES_MAX: usize = 32;
pub const LABELS_MAX: usize = 512;

/// What is wrong with a name, a curve id or a profile name (`what`), or `None`. Each reaches every user's terminal and shell,
/// and the journal, so it holds no control characters (escape sequences, newlines, NUL) and nothing that reorders the text
/// around it.
pub fn name_problem(what: &str, name: &str) -> Option<String> {
    if name.trim().is_empty() || name.chars().count() > LABEL_MAX {
        return Some(format!("{what} is 1 to {LABEL_MAX} characters"));
    }
    name.chars().find(|&c| hidden(c)).map(|c| format!("{what} can't hold a control character ({c:?})"))
}

/// Characters that do something other than show: C0, DEL and C1 controls, the bidirectional marks, overrides and isolates,
/// the line and paragraph separators, and what draws nothing at all (zero-width space, word joiner and the invisible
/// operators, the byte-order mark, the Hangul fillers, the soft hyphen, the Mongolian vowel separator, the deprecated format
/// characters and the tags), which would let a name look like another or like none. The zero-width joiner and non-joiner
/// stay, since Persian, the Indic scripts and emoji need them, and so do the variation selectors.
pub fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180E}'
                | '\u{200B}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    /// The active profile, or [`DEFAULTS`].
    pub profile: String,
    #[serde(default, rename = "sensor", skip_serializing_if = "Vec::is_empty")]
    pub sensors: Vec<VirtualSensor>,
    #[serde(default, rename = "curve", skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<Curve>,
    #[serde(default, rename = "control", skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlConfig>,
    /// Profile name → control key → curve id, or [`FIRMWARE`].
    #[serde(default)]
    pub profiles: BTreeMap<String, BTreeMap<Key, String>>,
    /// Names a person gave physical sensors and tachs (§3.2). Controls keep theirs in `[[control]]`, virtual sensors in `[[sensor]]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<Key, String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "VirtualRaw", into = "VirtualRaw")]
pub struct VirtualSensor {
    pub key: Key,
    pub label: Option<String>,
    pub kind: VirtualKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VirtualKind {
    Max(Vec<Key>),
    Min(Vec<Key>),
    Average(Vec<Key>),
    TimeAverage { source: Key, seconds: f64 },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VirtualRaw {
    key: Key,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max: Option<Vec<Key>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min: Option<Vec<Key>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    average: Option<Vec<Key>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time_average: Option<TimeAverageRaw>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TimeAverageRaw {
    source: Key,
    seconds: f64,
}

impl TryFrom<VirtualRaw> for VirtualSensor {
    type Error = String;
    fn try_from(r: VirtualRaw) -> Result<VirtualSensor, String> {
        let mut kinds = Vec::new();
        if let Some(k) = r.max {
            kinds.push(VirtualKind::Max(k));
        }
        if let Some(k) = r.min {
            kinds.push(VirtualKind::Min(k));
        }
        if let Some(k) = r.average {
            kinds.push(VirtualKind::Average(k));
        }
        if let Some(t) = r.time_average {
            kinds.push(VirtualKind::TimeAverage { source: t.source, seconds: t.seconds });
        }
        match kinds.len() {
            1 => Ok(VirtualSensor { key: r.key, label: r.label, kind: kinds.pop().expect("one kind") }),
            0 => Err(format!("sensor {} needs one of max, min, average or time_average", r.key)),
            _ => Err(format!("sensor {} has more than one of max, min, average and time_average", r.key)),
        }
    }
}

impl From<VirtualSensor> for VirtualRaw {
    fn from(v: VirtualSensor) -> VirtualRaw {
        let mut r = VirtualRaw { key: v.key, label: v.label, max: None, min: None, average: None, time_average: None };
        match v.kind {
            VirtualKind::Max(k) => r.max = Some(k),
            VirtualKind::Min(k) => r.min = Some(k),
            VirtualKind::Average(k) => r.average = Some(k),
            VirtualKind::TimeAverage { source, seconds } => r.time_average = Some(TimeAverageRaw { source, seconds }),
        }
        r
    }
}

impl VirtualKind {
    pub fn inputs(&self) -> Vec<&Key> {
        match self {
            VirtualKind::Max(k) | VirtualKind::Min(k) | VirtualKind::Average(k) => k.iter().collect(),
            VirtualKind::TimeAverage { source, .. } => vec![source],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "CurveRaw", into = "CurveRaw")]
pub struct Curve {
    pub id: String,
    /// What a person calls it. Default: from the id, see [`Curve::label`].
    pub label: Option<String>,
    pub kind: CurveKind,
    /// Exponential smoothing of the source, in seconds (§4.4). Default: 5 s for CPU sources, none otherwise.
    pub response: Option<f64>,
    /// °C the temperature must fall below where the output last rose before the output falls (§4.4). Default 3 °C on a graph.
    pub hysteresis: Option<f64>,
    pub hysteresis_direction: HysteresisDirection,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CurveRaw {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flat: Option<FlatRaw>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    graph: Option<GraphRaw>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mix: Option<MixRaw>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trigger: Option<TriggerRaw>,
    #[serde(default, deserialize_with = "seconds_opt", serialize_with = "ser_seconds_opt", skip_serializing_if = "Option::is_none")]
    response: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hysteresis: Option<f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    hysteresis_direction: HysteresisDirection,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FlatRaw {
    percent: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphRaw {
    source: Key,
    points: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MixRaw {
    op: MixOp,
    curves: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TriggerRaw {
    source: Key,
    idle: f64,
    load: f64,
    on: f64,
    off: f64,
}

impl TryFrom<CurveRaw> for Curve {
    type Error = String;
    fn try_from(r: CurveRaw) -> Result<Curve, String> {
        let mut kinds = Vec::new();
        if let Some(f) = r.flat {
            kinds.push(CurveKind::Flat { percent: f.percent });
        }
        if let Some(g) = r.graph {
            kinds.push(CurveKind::Graph { source: g.source, points: g.points });
        }
        if let Some(m) = r.mix {
            kinds.push(CurveKind::Mix { op: m.op, curves: m.curves });
        }
        if let Some(t) = r.trigger {
            kinds.push(CurveKind::Trigger { source: t.source, idle: t.idle, load: t.load, on: t.on, off: t.off });
        }
        let kind = match kinds.len() {
            1 => kinds.pop().expect("one kind"),
            0 => return Err(format!("curve {:?} needs one of flat, graph, mix or trigger", r.id)),
            _ => return Err(format!("curve {:?} has more than one of flat, graph, mix and trigger", r.id)),
        };
        Ok(Curve {
            id: r.id,
            label: r.label,
            kind,
            response: r.response,
            hysteresis: r.hysteresis,
            hysteresis_direction: r.hysteresis_direction,
        })
    }
}

impl From<Curve> for CurveRaw {
    fn from(c: Curve) -> CurveRaw {
        let mut r = CurveRaw {
            id: c.id,
            label: c.label,
            flat: None,
            graph: None,
            mix: None,
            trigger: None,
            response: c.response,
            hysteresis: c.hysteresis,
            hysteresis_direction: c.hysteresis_direction,
        };
        match c.kind {
            CurveKind::Flat { percent } => r.flat = Some(FlatRaw { percent }),
            CurveKind::Graph { source, points } => r.graph = Some(GraphRaw { source, points }),
            CurveKind::Mix { op, curves } => r.mix = Some(MixRaw { op, curves }),
            CurveKind::Trigger { source, idle, load, on, off } => r.trigger = Some(TriggerRaw { source, idle, load, on, off }),
        }
        r
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HysteresisDirection {
    /// Rise at once; fall only after the temperature drops by the hysteresis.
    #[default]
    Falling,
    /// Change in either direction only after the temperature moves by the hysteresis.
    Both,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Clone, Debug, PartialEq)]
pub enum CurveKind {
    Flat { percent: f64 },
    Graph { source: Key, points: Vec<[f64; 2]> },
    Mix { op: MixOp, curves: Vec<String> },
    Trigger { source: Key, idle: f64, load: f64, on: f64, off: f64 },
}

impl Curve {
    /// Its label, or one made from the id: `cpu-quiet` is "CPU quiet".
    pub fn label(&self) -> String {
        self.label.clone().unwrap_or_else(|| words(&self.id))
    }
}

/// An id as words for a person: separators become spaces, the first letter is a capital, and acronyms are upper case.
pub fn words(id: &str) -> String {
    const ACRONYMS: &[&str] = &["cpu", "gpu", "nvme", "ssd", "hdd", "vrm", "aio", "pch"];
    let parts: Vec<String> = id
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, w)| {
            if ACRONYMS.contains(&w.to_ascii_lowercase().as_str()) {
                w.to_ascii_uppercase()
            } else if i == 0 {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            } else {
                w.to_owned()
            }
        })
        .collect();
    if parts.is_empty() { id.to_owned() } else { parts.join(" ") }
}

impl CurveKind {
    pub fn source(&self) -> Option<&Key> {
        match self {
            CurveKind::Graph { source, .. } | CurveKind::Trigger { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MixOp {
    Max,
    Min,
    Average,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlConfig {
    pub key: Key,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub min: f64,
    #[serde(default = "hundred")]
    pub max: f64,
    #[serde(default)]
    pub offset: f64,
    /// %/s
    #[serde(default = "step_up")]
    pub step_up: f64,
    /// %/s
    #[serde(default = "step_down")]
    pub step_down: f64,
    #[serde(default)]
    pub may_stop: bool,
    /// The kick that spins a stopped fan up, held for [`crate::control::KICK_SECONDS`]. Default: the calibrated start duty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    #[serde(default = "hundred")]
    pub failsafe: f64,
}

fn hundred() -> f64 {
    100.0
}
fn step_up() -> f64 {
    20.0
}
fn step_down() -> f64 {
    5.0
}

impl ControlConfig {
    pub fn defaults(key: Key) -> ControlConfig {
        ControlConfig {
            key,
            label: None,
            min: 0.0,
            max: 100.0,
            offset: 0.0,
            step_up: step_up(),
            step_down: step_down(),
            may_stop: false,
            start: None,
            failsafe: 100.0,
        }
    }
}

/// What setup measured (DESIGN.md §5, `/var/lib/penna/measured.toml`): facts about the hardware, kept whether or not any curve
/// is saved. Everything but `key` may be left out, so a file from before setup grew a field still reads.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    /// Unix time of the last setup run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// Each sensor's mean over the idle sample (§5.5's anchors).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub idle: BTreeMap<Key, f64>,
    #[serde(default, rename = "control")]
    pub controls: Vec<MeasuredControl>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredControl {
    pub key: Key,
    /// The write test's result (§5.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<crate::setup::WriteResult>,
    /// Pairing found the fan on it: `tachs`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paired: bool,
    /// Pairing found no fan of its own on this header: nothing is plugged in, or what is turns with another control too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub empty: bool,
    /// The lowest duty that still turns the fan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<f64>,
    /// The first duty that spins a stopped fan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    /// Seconds from the start duty's write to a steady speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spin_up: Option<f64>,
    /// Calibration went no lower than this, and Penna never runs the fan below it: a pump, a chipset fan, a fan on a pump header
    /// not yet confirmed, or a header that turns its fan faster below it (§5.3, §5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lowest: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tachs: Vec<Key>,
    /// Duty → RPM, ascending (§5.4): what calibration measured, for the editor's RPM axis. NVML fans have one too, over their
    /// settable range.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calibration: Vec<[f64; 2]>,
    /// The idle sample saw its firmware stop it (§5.3: a chipset fan may then stop).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub firmware_stops: bool,
    /// What setup suggested it is, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested: Option<crate::setup::Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the user confirmed it is (§5.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<crate::setup::Role>,
}

impl MeasuredControl {
    pub fn new(key: Key) -> MeasuredControl {
        MeasuredControl {
            key,
            write: None,
            paired: false,
            empty: false,
            stop: None,
            start: None,
            spin_up: None,
            lowest: None,
            tachs: Vec::new(),
            calibration: Vec::new(),
            firmware_stops: false,
            suggested: None,
            reason: None,
            role: None,
        }
    }
}

impl Measured {
    pub fn parse(text: &str) -> Result<Measured, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn get(&self, key: &Key) -> Option<&MeasuredControl> {
        self.controls.iter().find(|m| &m.key == key)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("measurements always serialize")
    }

    /// Takes in a newer run's results: a control measured again replaces what was known about it, keeping a role the user
    /// confirmed while the hardware stays the same; the others stay.
    pub fn merge(&mut self, newer: Measured) {
        self.at = newer.at.or(self.at);
        self.idle.extend(newer.idle);
        for mut m in newer.controls {
            match self.controls.iter_mut().find(|c| c.key == m.key) {
                Some(old) => {
                    if m.role.is_none() && old.tachs == m.tachs {
                        m.role = old.role;
                    }
                    *old = m;
                }
                None => self.controls.push(m),
            }
        }
    }
}

/// One thing wrong with a configuration, with where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub at: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

fn problem(at: impl Into<String>, message: impl Into<String>) -> Problem {
    Problem { at: at.into(), message: message.into() }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("a config always serializes")
    }

    /// The file as `pennad` and `penna reset` write it: a header saying who rewrites it, then the TOML.
    pub fn saved_text(&self) -> String {
        format!(
            "# Penna's configuration (DESIGN.md §7.5). pennad rewrites this file when a surface changes it: edit it while pennad is\n\
             # stopped, then run `penna check`.\n{}",
            self.to_toml()
        )
    }

    pub fn curve(&self, id: &str) -> Option<&Curve> {
        self.curves.iter().find(|c| c.id == id)
    }

    pub fn control(&self, key: &Key) -> Option<&ControlConfig> {
        self.controls.iter().find(|c| &c.key == key)
    }

    pub fn virtual_sensor(&self, key: &Key) -> Option<&VirtualSensor> {
        self.sensors.iter().find(|s| &s.key == key)
    }

    /// Everything that is wrong with this configuration on its own, without looking at a machine.
    pub fn validate(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        if self.version != VERSION {
            out.push(problem("version", format!("is {}, this Penna reads version {VERSION}", self.version)));
        }
        if self.profile != DEFAULTS && !self.profiles.contains_key(&self.profile) {
            out.push(problem(
                "profile",
                format!("{:?} is not one of the profiles ({}), nor the built-in {DEFAULTS:?}", self.profile, names(self.profiles.keys())),
            ));
        }
        if self.profiles.contains_key(DEFAULTS) {
            out.push(problem(format!("profile {DEFAULTS:?}"), "is Penna's built-in profile; give yours another name"));
        }
        if self.profiles.len() > PROFILES_MAX {
            out.push(problem("profiles", format!("there are {}; a configuration holds at most {PROFILES_MAX}", self.profiles.len())));
        }
        for name in self.profiles.keys() {
            if let Some(why) = name_problem("a profile's name", name) {
                out.push(problem(format!("profile {name:?}"), why));
            }
        }
        if self.sensors.len() > VIRTUAL_MAX {
            out.push(problem("sensors", format!("there are {}; a configuration holds at most {VIRTUAL_MAX}", self.sensors.len())));
        }
        if self.curves.len() > CURVES_MAX {
            out.push(problem("curves", format!("there are {}; a configuration holds at most {CURVES_MAX}", self.curves.len())));
        }
        if self.labels.len() > LABELS_MAX {
            out.push(problem("labels", format!("there are {}; a configuration holds at most {LABELS_MAX}", self.labels.len())));
        }

        let mut seen = BTreeSet::new();
        for s in &self.sensors {
            let at = format!("sensor {}", s.key);
            if !s.key.as_str().starts_with("virt/") {
                out.push(problem(&at, "a virtual sensor's key starts with virt/"));
            }
            if !seen.insert(s.key.clone()) {
                out.push(problem(&at, "is defined twice"));
            }
            if let Some(why) = s.label.as_deref().and_then(|l| name_problem("a sensor's name", l)) {
                out.push(problem(&at, why));
            }
            if s.kind.inputs().len() > MIX_MAX {
                out.push(problem(&at, format!("uses {} sensors; a virtual sensor uses at most {MIX_MAX}", s.kind.inputs().len())));
            }
            match &s.kind {
                VirtualKind::Max(k) | VirtualKind::Min(k) | VirtualKind::Average(k) if k.is_empty() => {
                    out.push(problem(&at, "needs at least one input"))
                }
                VirtualKind::TimeAverage { seconds, .. } if !(1.0..=3600.0).contains(seconds) => {
                    out.push(problem(&at, "time_average seconds must be between 1 and 3600"))
                }
                _ => {}
            }
            for input in s.kind.inputs() {
                if input == &s.key {
                    out.push(problem(&at, "uses itself"));
                } else if input.as_str().starts_with("virt/") && self.virtual_sensor(input).is_none() {
                    out.push(problem(&at, format!("uses {input}, which is not defined")));
                }
            }
        }
        if let Err(cycle) = virtual_order(self) {
            out.push(problem(format!("sensor {cycle}"), "is part of a loop of virtual sensors"));
        }

        let mut ids = BTreeSet::new();
        for c in &self.curves {
            let at = format!("curve {:?}", c.id);
            if c.id.is_empty() || c.id == FIRMWARE {
                out.push(problem(&at, format!("a curve needs an id other than {FIRMWARE:?}")));
            } else if let Some(why) = name_problem("a curve's id", &c.id) {
                out.push(problem(&at, why));
            }
            if !ids.insert(c.id.as_str()) {
                out.push(problem(&at, "is defined twice"));
            }
            if let Some(why) = c.label.as_deref().and_then(|l| name_problem("a curve's name", l)) {
                out.push(problem(&at, why));
            }
            if let Some(r) = c.response
                && !(0.0..=600.0).contains(&r)
            {
                out.push(problem(&at, "response must be between 0 and 600 s"));
            }
            if let Some(h) = c.hysteresis
                && !(0.0..=20.0).contains(&h)
            {
                out.push(problem(&at, "hysteresis must be between 0 and 20 °C"));
            }
            if let Some(src) = c.kind.source() {
                self.check_source(&at, src, &mut out);
            }
            match &c.kind {
                CurveKind::Flat { percent } => {
                    percent_in_range(&at, "percent", *percent, &mut out);
                    if c.response.is_some() || c.hysteresis.is_some() {
                        out.push(problem(&at, "a flat curve has no response or hysteresis"));
                    }
                }
                CurveKind::Graph { points, .. } => check_points(&at, points, &mut out),
                CurveKind::Mix { curves, .. } => {
                    if curves.is_empty() {
                        out.push(problem(&at, "a mix needs at least one curve"));
                    }
                    if curves.len() > MIX_MAX {
                        out.push(problem(&at, format!("mixes {} curves; a mix uses at most {MIX_MAX}", curves.len())));
                    }
                    if curves.iter().collect::<BTreeSet<_>>().len() != curves.len() {
                        out.push(problem(&at, "mixes the same curve twice"));
                    }
                    for id in curves {
                        if self.curve(id).is_none() {
                            out.push(problem(&at, format!("mixes {id:?}, which is not defined")));
                        }
                    }
                    if c.response.is_some() || c.hysteresis.is_some() {
                        out.push(problem(&at, "a mix has no response or hysteresis of its own; set them on the curves it mixes"));
                    }
                }
                CurveKind::Trigger { idle, load, on, off, .. } => {
                    percent_in_range(&at, "idle", *idle, &mut out);
                    percent_in_range(&at, "load", *load, &mut out);
                    if off >= on {
                        out.push(problem(&at, format!("off ({off} °C) must be below on ({on} °C)")));
                    }
                    if c.hysteresis.is_some() {
                        out.push(problem(&at, "a trigger's hysteresis is the gap between off and on"));
                    }
                }
            }
        }
        if let Err(cycle) = curve_order(self) {
            out.push(problem(format!("curve {cycle:?}"), "is part of a loop of mixes"));
        }

        let mut keys = BTreeSet::new();
        for c in &self.controls {
            let at = format!("control {}", c.key);
            if !(c.key.as_str().starts_with("hwmon/") || c.key.as_str().starts_with("nvml/") || c.key.as_str().starts_with("mock/")) {
                out.push(problem(&at, "is not a control key (hwmon/…/pwmN or nvml/…/fanN)"));
            }
            if !keys.insert(&c.key) {
                out.push(problem(&at, "is defined twice"));
            }
            if let Some(why) = c.label.as_deref().and_then(|l| name_problem("a label", l)) {
                out.push(problem(&at, why));
            }
            for (name, v) in [("min", c.min), ("max", c.max), ("failsafe", c.failsafe)] {
                percent_in_range(&at, name, v, &mut out);
            }
            if c.min > c.max {
                out.push(problem(&at, format!("min ({} %) is above max ({} %)", c.min, c.max)));
            }
            if !(-50.0..=50.0).contains(&c.offset) {
                out.push(problem(&at, "offset must be between -50 and 50 %"));
            }
            for (name, v) in [("step_up", c.step_up), ("step_down", c.step_down)] {
                if !(v > 0.0 && v <= 100.0) {
                    out.push(problem(&at, format!("{name} must be more than 0 and at most 100 %/s")));
                }
            }
            if let Some(s) = c.start {
                percent_in_range(&at, "start", s, &mut out);
                if s > c.max {
                    out.push(problem(&at, format!("start ({s} %) is above max ({} %)", c.max)));
                }
            }
            if c.failsafe < c.min {
                out.push(problem(&at, format!("failsafe ({} %) is below min ({} %)", c.failsafe, c.min)));
            }
        }

        for (key, label) in &self.labels {
            if let Some(why) = name_problem("a label", label) {
                out.push(problem(format!("labels, {key}"), why));
            }
        }

        for (name, assignments) in &self.profiles {
            for (key, curve) in assignments {
                let at = format!("profile {name:?}, {key}");
                if curve != FIRMWARE && self.curve(curve).is_none() {
                    out.push(problem(&at, format!("uses curve {curve:?}, which is not defined")));
                }
                if key.as_str().starts_with("virt/") {
                    out.push(problem(&at, "a profile assigns curves to controls, not sensors"));
                }
            }
        }
        out
    }

    fn check_source(&self, at: &str, src: &Key, out: &mut Vec<Problem>) {
        if src.as_str().starts_with("virt/") && self.virtual_sensor(src).is_none() {
            out.push(problem(at, format!("reads {src}, which is not defined")));
        }
        if src.channel().starts_with("pwm") || (src.as_str().starts_with("nvml/") && src.channel() != "temp") {
            out.push(problem(at, format!("reads {src}, which is not a temperature")));
        }
    }

    /// Every physical sensor any curve reads, through virtual sensors.
    pub fn physical_sources(&self) -> BTreeSet<Key> {
        let mut out = BTreeSet::new();
        let mut stack: Vec<Key> = self.curves.iter().filter_map(|c| c.kind.source().cloned()).collect();
        let mut seen = BTreeSet::new();
        while let Some(k) = stack.pop() {
            if !seen.insert(k.clone()) {
                continue;
            }
            match self.virtual_sensor(&k) {
                Some(v) => stack.extend(v.kind.inputs().into_iter().cloned()),
                None => {
                    out.insert(k);
                }
            }
        }
        out
    }
}

fn names<'a>(it: impl Iterator<Item = &'a String>) -> String {
    let v: Vec<String> = it.map(|s| format!("{s:?}")).collect();
    if v.is_empty() { "there are none".to_owned() } else { v.join(", ") }
}

fn percent_in_range(at: &str, name: &str, v: f64, out: &mut Vec<Problem>) {
    if !(0.0..=100.0).contains(&v) {
        out.push(problem(at, format!("{name} ({v}) must be between 0 and 100 %")));
    }
}

/// Decision 3 of the design review: a graph never falls as the temperature rises.
fn check_points(at: &str, points: &[[f64; 2]], out: &mut Vec<Problem>) {
    if points.len() < 2 {
        out.push(problem(at, "a graph needs at least two points"));
        return;
    }
    if points.len() > POINTS_MAX {
        out.push(problem(at, format!("a graph has at most {POINTS_MAX} points, not {}", points.len())));
        return;
    }
    for (i, [t, p]) in points.iter().enumerate() {
        if !(-40.0..=150.0).contains(t) {
            out.push(problem(at, format!("point {} is at {t} °C, outside −40…150 °C", i + 1)));
        }
        percent_in_range(at, &format!("point {}", i + 1), *p, out);
    }
    for (i, w) in points.windows(2).enumerate() {
        let ([t0, p0], [t1, p1]) = (w[0], w[1]);
        if t1 <= t0 {
            out.push(problem(at, format!("point {} ({t1} °C) must be hotter than point {} ({t0} °C)", i + 2, i + 1)));
        }
        if p1 < p0 {
            out.push(problem(
                at,
                format!("point {} ({p1} %) is below point {} ({p0} %): a fan curve never falls as it gets hotter", i + 2, i + 1),
            ));
        }
    }
}

/// A number as a person writes it: `72`, `72.5`.
pub fn num(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r.fract() == 0.0 { format!("{r:.0}") } else { format!("{r}") }
}

/// What is wrong with a graph's points, in one sentence that names the values, or `None` when nothing is (§7.3, decision 3).
/// The studio shows it as it is, so it speaks of temperatures and duties, not point numbers.
pub fn explain_points(points: &[[f64; 2]]) -> Option<String> {
    if points.len() < 2 {
        return Some(format!("A graph needs at least two points; this one has {}.", points.len()));
    }
    if points.len() > POINTS_MAX {
        return Some(format!("A graph has at most {POINTS_MAX} points; this one has {}.", points.len()));
    }
    for [t, p] in points {
        if !(-40.0..=150.0).contains(t) {
            return Some(format!("A point at {} °C is outside the −40 to 150 °C a sensor can read.", num(*t)));
        }
        if !(0.0..=100.0).contains(p) {
            return Some(format!("A point at {} °C → {} % is outside 0 to 100 %.", num(*t), num(*p)));
        }
    }
    for w in points.windows(2) {
        let ([t0, p0], [t1, p1]) = (w[0], w[1]);
        if t1 <= t0 {
            return Some(format!(
                "Each point must be hotter than the one before it: {} °C → {} % comes after {} °C → {} %.",
                num(t1),
                num(p1),
                num(t0),
                num(p0)
            ));
        }
        if p1 < p0 {
            return Some(format!(
                "A curve can't fall as the temperature rises: {} °C → {} % is below {} °C → {} %.",
                num(t1),
                num(p1),
                num(t0),
                num(p0)
            ));
        }
    }
    None
}

/// Problems as one sentence for a person: the first capitalised, the rest after semicolons.
pub fn sentence(problems: &[Problem]) -> String {
    let mut parts: Vec<String> = problems.iter().map(|p| p.message.clone()).collect();
    parts.dedup();
    let text = parts.join("; ");
    let mut c = text.chars();
    let first: String = c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default();
    if first.ends_with('.') { first } else { format!("{first}.") }
}

/// What a draft changes against the saved configuration: the curves added, changed or removed, and the controls whose limits or
/// assignment in any profile differ. Both sorted.
pub fn changes(saved: &Config, draft: &Config) -> (Vec<String>, Vec<Key>) {
    let mut curves = BTreeSet::new();
    for c in saved.curves.iter().chain(&draft.curves) {
        if saved.curve(&c.id) != draft.curve(&c.id) {
            curves.insert(c.id.clone());
        }
    }
    let mut controls = BTreeSet::new();
    for c in saved.controls.iter().chain(&draft.controls) {
        if saved.control(&c.key) != draft.control(&c.key) {
            controls.insert(c.key.clone());
        }
    }
    let empty = BTreeMap::new();
    for name in saved.profiles.keys().chain(draft.profiles.keys()) {
        let (a, b) = (saved.profiles.get(name).unwrap_or(&empty), draft.profiles.get(name).unwrap_or(&empty));
        for key in a.keys().chain(b.keys()) {
            if a.get(key) != b.get(key) {
                controls.insert(key.clone());
            }
        }
    }
    (curves.into_iter().collect(), controls.into_iter().collect())
}

/// Curves in an order where every mix comes after what it mixes. `Err` names a curve in a loop.
pub fn curve_order(cfg: &Config) -> Result<Vec<String>, String> {
    let deps = |c: &Curve| match &c.kind {
        CurveKind::Mix { curves, .. } => curves.clone(),
        _ => Vec::new(),
    };
    topo(cfg.curves.iter().map(|c| (c.id.clone(), deps(c))).collect())
}

/// Virtual sensors in an order where every one comes after the virtual sensors it reads.
pub fn virtual_order(cfg: &Config) -> Result<Vec<Key>, String> {
    let nodes: BTreeMap<String, Vec<String>> = cfg
        .sensors
        .iter()
        .map(|s| {
            let deps = s.kind.inputs().into_iter().filter(|k| cfg.virtual_sensor(k).is_some()).map(|k| k.to_string()).collect();
            (s.key.to_string(), deps)
        })
        .collect();
    Ok(topo(nodes)?.into_iter().filter_map(|s| Key::parse(&s)).collect())
}

fn topo(nodes: BTreeMap<String, Vec<String>>) -> Result<Vec<String>, String> {
    fn visit(n: &str, nodes: &BTreeMap<String, Vec<String>>, mark: &mut BTreeMap<String, u8>, out: &mut Vec<String>) -> Result<(), String> {
        match mark.get(n) {
            Some(2) => return Ok(()),
            Some(1) => return Err(n.to_owned()),
            _ => {}
        }
        mark.insert(n.to_owned(), 1);
        for d in nodes.get(n).into_iter().flatten() {
            if nodes.contains_key(d) {
                visit(d, nodes, mark, out)?;
            }
        }
        mark.insert(n.to_owned(), 2);
        out.push(n.to_owned());
        Ok(())
    }
    let mut mark = BTreeMap::new();
    let mut out = Vec::new();
    for n in nodes.keys() {
        visit(n, &nodes, &mut mark, &mut out)?;
    }
    Ok(out)
}

/// `"5s"`, `"500ms"`, `"2m"`, `"1.5s"` or a plain number of seconds.
pub fn parse_seconds(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else {
        (s, 1.0)
    };
    let v: f64 = num.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then_some(v * mult)
}

fn seconds_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Num(f64),
        Text(String),
    }
    match Option::<Raw>::deserialize(d)? {
        None => Ok(None),
        Some(Raw::Num(v)) => Ok(Some(v)),
        Some(Raw::Text(s)) => parse_seconds(&s).map(Some).ok_or_else(|| de::Error::custom(format!("not a duration: {s:?} (try \"5s\")"))),
    }
}

fn ser_seconds_opt<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => s.serialize_str(&format!("{v}s")),
        None => s.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const EXAMPLE: &str = r#"
version = 1
profile = "balanced"

[[sensor]]
key = "virt/case"
label = "Case"
max = ["hwmon/k10temp/0000:00:18.3/temp1", "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/temp"]

[[curve]]
id = "cpu-quiet"
graph = { source = "hwmon/k10temp/0000:00:18.3/temp1", points = [[45, 30], [65, 40], [78, 65], [88, 100]] }
response = "5s"
hysteresis = 3.0

[[curve]]
id = "gpu-balanced"
graph = { source = "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/temp", points = [[45, 0], [55, 35], [70, 60], [83, 100]] }

[[curve]]
id = "case"
mix = { op = "max", curves = ["cpu-quiet", "gpu-balanced"] }

[[curve]]
id = "boost"
trigger = { source = "virt/case", idle = 35, load = 70, on = 60, off = 50 }

[[control]]
key = "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0"
label = "GPU fan 1"
may_stop = true
failsafe = 100

[profiles.balanced]
"nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0" = "gpu-balanced"
"hwmon/it87/it87.2608/pwm1" = "cpu-quiet"
"hwmon/it87/it87.2608/pwm2" = "firmware"
"#;

    fn example() -> Config {
        Config::parse(EXAMPLE).expect("the example parses")
    }

    #[test]
    fn the_design_example_parses_and_is_valid() {
        let c = example();
        assert_eq!(c.validate(), []);
        assert_eq!(c.curves.len(), 4);
        assert_eq!(c.curve("cpu-quiet").unwrap().response, Some(5.0));
        assert!(matches!(c.sensors[0].kind, VirtualKind::Max(ref k) if k.len() == 2));
        assert!(matches!(c.curve("boost").unwrap().kind, CurveKind::Trigger { on, off, .. } if on == 60.0 && off == 50.0));
        let ctl = c.control(&Key::parse("nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0").unwrap()).unwrap();
        assert_eq!((ctl.step_up, ctl.step_down, ctl.max, ctl.may_stop), (20.0, 5.0, 100.0, true));
    }

    #[test]
    fn it_round_trips() {
        let c = example();
        assert_eq!(Config::parse(&c.to_toml()).unwrap(), c);
    }

    #[test]
    fn a_dipping_graph_is_refused() {
        let text = EXAMPLE.replace("[[45, 30], [65, 40], [78, 65], [88, 100]]", "[[45, 30], [65, 50], [78, 40], [88, 100]]");
        let problems = Config::parse(&text).unwrap().validate();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("never falls"), "{}", problems[0]);
    }

    #[test]
    fn points_must_get_hotter() {
        let text = EXAMPLE.replace("[[45, 30], [65, 40], [78, 65], [88, 100]]", "[[45, 30], [45, 40]]");
        assert!(Config::parse(&text).unwrap().validate().iter().any(|p| p.message.contains("hotter")));
    }

    #[test]
    fn references_must_exist() {
        let text = EXAMPLE
            .replace("curves = [\"cpu-quiet\", \"gpu-balanced\"]", "curves = [\"cpu-quiet\", \"nope\"]")
            .replace("\"hwmon/it87/it87.2608/pwm1\" = \"cpu-quiet\"", "\"hwmon/it87/it87.2608/pwm1\" = \"missing\"");
        let p = Config::parse(&text).unwrap().validate();
        assert!(p.iter().any(|p| p.message.contains("\"nope\"")), "{p:?}");
        assert!(p.iter().any(|p| p.message.contains("\"missing\"")), "{p:?}");
    }

    #[test]
    fn mix_loops_are_found() {
        let (head, profile) = EXAMPLE.split_once("[profiles.balanced]").unwrap();
        let text = format!(
            "{head}[[curve]]\nid = \"a\"\nmix = {{ op = \"max\", curves = [\"b\"] }}\n[[curve]]\nid = \"b\"\nmix = {{ op = \"min\", curves = [\"a\"] }}\n[profiles.balanced]{profile}"
        );
        let p = Config::parse(&text).unwrap().validate();
        assert!(p.iter().any(|p| p.message.contains("loop of mixes")), "{p:?}");
    }

    #[test]
    fn a_trigger_needs_off_below_on() {
        let text = EXAMPLE.replace("on = 60, off = 50", "on = 50, off = 60");
        assert!(Config::parse(&text).unwrap().validate().iter().any(|p| p.message.contains("must be below on")));
    }

    #[test]
    fn the_active_profile_must_exist() {
        let text = EXAMPLE.replace("profile = \"balanced\"", "profile = \"quiet\"");
        assert!(Config::parse(&text).unwrap().validate().iter().any(|p| p.at == "profile"));
    }

    #[test]
    fn defaults_is_built_in() {
        let text = EXAMPLE.replace("profile = \"balanced\"", "profile = \"defaults\"");
        assert_eq!(Config::parse(&text).unwrap().validate(), [], "Defaults needs no [profiles.defaults]");
        let text = format!("{EXAMPLE}\n[profiles.defaults]\n");
        let p = Config::parse(&text).unwrap().validate();
        assert!(p.iter().any(|p| p.message.contains("built-in")), "{p:?}");
    }

    #[test]
    fn unknown_fields_and_two_kinds_are_parse_errors() {
        assert!(Config::parse(&EXAMPLE.replace("may_stop = true", "may_stopp = true")).is_err());
        let both = EXAMPLE.replace("id = \"case\"\nmix", "id = \"case\"\nflat = { percent = 40 }\nmix");
        assert!(Config::parse(&both).is_err());
    }

    #[test]
    fn words_from_ids() {
        assert_eq!(words("cpu-quiet"), "CPU quiet");
        assert_eq!(words("gpu"), "GPU");
        assert_eq!(words("case"), "Case");
        assert_eq!(words("rear_exhaust"), "Rear exhaust");
        assert_eq!(example().curve("gpu-balanced").unwrap().label(), "GPU balanced");
    }

    #[test]
    fn labels_round_trip() {
        let text = format!("{EXAMPLE}\n[labels]\n\"hwmon/k10temp/0000:00:18.3/temp1\" = \"CPU\"\n");
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.labels.len(), 1);
        assert_eq!(Config::parse(&c.to_toml()).unwrap(), c);
    }

    #[test]
    fn a_name_holds_nothing_a_terminal_or_the_journal_reads_as_an_instruction() {
        for bad in
            ["\u{1b}[2J", "OSC \u{1b}]52;c;aGk=\u{7}", "two\nlines", "NUL\u{0}", "C1 \u{9b}31m", "\u{202e}desrever", "a\u{2028}b", "  "]
        {
            assert!(name_problem("a label", bad).is_some(), "{bad:?}");
        }
        for bad in ["\u{200B}", "CPU\u{200B}cooler", "\u{FEFF}Rear", "Rear\u{2060}", "\u{3164}", "soft\u{00AD}hyphen", "tag\u{E0041}"] {
            assert!(name_problem("a label", bad).is_some(), "{bad:?} draws nothing, or looks like another name");
        }
        for good in
            ["CPU cooler", "Rear exhaust · 140 mm", "Ventilador traseiro", "GPU 2 — top", "温度", "می\u{200C}خواهم", "🌀 Case ❤\u{FE0F}"]
        {
            assert_eq!(name_problem("a label", good), None, "{good:?}");
        }
        assert!(name_problem("a label", &"é".repeat(49)).is_some(), "48 characters, not bytes");
        assert_eq!(name_problem("a label", &"é".repeat(48)), None);
        assert!(Key::parse("hwmon/it87/it87.2608/pwm1\u{1b}[2J").is_none());
        assert!(Key::parse(&format!("hwmon/it87/{}/pwm1", "a".repeat(300))).is_none());

        let with = |extra: &str| Config::parse(&format!("{EXAMPLE}{extra}")).unwrap().validate();
        assert!(with("\n[labels]\n\"hwmon/k10temp/0000:00:18.3/temp1\" = \"CPU\\u001b[31m\"\n").iter().any(|p| p.at.starts_with("labels")));
        // In TOML's own escapes, as a hand-written file would hold them.
        let curve = |id: &str, label: &str| format!("\n[[curve]]\nid = \"{id}\"\nlabel = \"{label}\"\nflat = {{ percent = 40 }}\n");
        assert!(with(&curve("quiet\\u0007", "Quiet")).iter().any(|p| p.message.contains("a curve's id")));
        assert!(with(&curve("quiet", "Quiet\\u001b")).iter().any(|p| p.message.contains("a curve's name")));
        assert!(with("\n[profiles.\"night\\nmode\"]\n").iter().any(|p| p.message.contains("a profile's name")));
    }

    /// Every client is sent the whole configuration each tick, so the most a configuration may hold is still a small line.
    #[test]
    fn a_configuration_at_every_limit_stays_far_under_a_protocol_line() {
        let name = "温".repeat(LABEL_MAX);
        let mut c = Config::parse(EXAMPLE).unwrap();
        c.curves = (0..CURVES_MAX)
            .map(|i| Curve {
                id: format!("{i:03}{}", "c".repeat(LABEL_MAX - 3)),
                label: Some(name.clone()),
                kind: CurveKind::Graph {
                    source: Key::parse("hwmon/k10temp/0000:00:18.3/temp1").unwrap(),
                    points: (0..POINTS_MAX).map(|p| [20.0 + p as f64 * 3.333_333_3, p as f64 * 3.125]).collect(),
                },
                response: Some(5.0),
                hysteresis: Some(3.0),
                hysteresis_direction: HysteresisDirection::default(),
            })
            .collect();
        let line = serde_json::to_string(&c).unwrap();
        assert!(line.len() < (1 << 20) / 2, "{} bytes", line.len());

        let too_many = |n: usize| {
            let mut c = c.clone();
            c.curves.truncate(n);
            c
        };
        assert!(!too_many(CURVES_MAX).validate().iter().any(|p| p.at == "curves"));
        let mut over = too_many(CURVES_MAX);
        over.curves.push(Curve { id: "one-more".into(), ..over.curves[0].clone() });
        assert!(over.validate().iter().any(|p| p.at == "curves"));
        let points: Vec<[f64; 2]> = (0..=POINTS_MAX).map(|p| [20.0 + p as f64, 10.0 + p as f64]).collect();
        assert!(explain_points(&points).is_some_and(|s| s.contains("at most")));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_seconds("5s"), Some(5.0));
        assert_eq!(parse_seconds("500ms"), Some(0.5));
        assert_eq!(parse_seconds("2m"), Some(120.0));
        assert_eq!(parse_seconds("1.5"), Some(1.5));
        assert_eq!(parse_seconds("-1s"), None);
        assert_eq!(parse_seconds("fast"), None);
        let numeric = EXAMPLE.replace("response = \"5s\"", "response = 2.5");
        assert_eq!(Config::parse(&numeric).unwrap().curve("cpu-quiet").unwrap().response, Some(2.5));
    }

    #[test]
    fn refused_points_are_explained_with_their_values() {
        assert_eq!(explain_points(&[[45.0, 30.0], [65.0, 40.0], [78.0, 65.0], [88.0, 100.0]]), None);
        assert_eq!(
            explain_points(&[[45.0, 30.0], [65.0, 60.0], [72.0, 58.0], [88.0, 100.0]]).as_deref(),
            Some("A curve can't fall as the temperature rises: 72 °C → 58 % is below 65 °C → 60 %.")
        );
        assert_eq!(
            explain_points(&[[45.0, 30.0], [70.0, 50.0], [65.0, 55.0]]).as_deref(),
            Some("Each point must be hotter than the one before it: 65 °C → 55 % comes after 70 °C → 50 %.")
        );
        assert_eq!(explain_points(&[[45.0, 30.0]]).as_deref(), Some("A graph needs at least two points; this one has 1."));
        assert_eq!(explain_points(&[[45.0, 30.0], [72.5, 120.0]]).as_deref(), Some("A point at 72.5 °C → 120 % is outside 0 to 100 %."));
        assert_eq!(
            explain_points(&[[45.0, 30.0], [160.0, 100.0]]).as_deref(),
            Some("A point at 160 °C is outside the −40 to 150 °C a sensor can read.")
        );
    }

    #[test]
    fn problems_read_as_one_sentence() {
        let p = |m: &str| Problem { at: "control x".into(), message: m.into() };
        assert_eq!(sentence(&[p("min (60 %) is above max (50 %)")]), "Min (60 %) is above max (50 %).");
        assert_eq!(sentence(&[p("a"), p("b"), p("b")]), "A; b.");
    }

    #[test]
    fn changes_name_curves_and_controls() {
        let saved = example();
        let mut draft = saved.clone();
        assert_eq!(changes(&saved, &draft), (vec![], vec![]));
        if let CurveKind::Graph { points, .. } = &mut draft.curves[0].kind {
            points[1] = [65.0, 45.0];
        }
        draft.curves.retain(|c| c.id != "boost");
        let pwm2 = Key::parse("hwmon/it87/it87.2608/pwm2").unwrap();
        draft.profiles.get_mut("balanced").unwrap().insert(pwm2.clone(), "case".into());
        let (curves, controls) = changes(&saved, &draft);
        assert_eq!(curves, ["boost", "cpu-quiet"]);
        assert_eq!(controls, [pwm2]);
    }

    #[test]
    fn numbers_as_people_write_them() {
        assert_eq!(num(72.0), "72");
        assert_eq!(num(72.5), "72.5");
        assert_eq!(num(0.333333), "0.33");
    }

    #[test]
    fn physical_sources_see_through_virtual_sensors() {
        let s = example().physical_sources();
        let names: Vec<&str> = s.iter().map(Key::as_str).collect();
        assert_eq!(names, ["hwmon/k10temp/0000:00:18.3/temp1", "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/temp"]);
    }
}
