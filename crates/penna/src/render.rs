//! The `penna watch` tables: [`tick_frame`] for what the daemon says, [`frame`] for the hardware alone. Numbers sit in
//! fixed-width, right-aligned columns so a value never shifts sideways as it changes (DESIGN.md §8.6); styling is applied after
//! padding so escape codes never count as width.
use penna_core::control::{State, Trend};
use penna_core::{ControlKind, ControlMode, Limit, LimitKind, Source};
use penna_proto::hardware::{ControlSnapshot, DeviceSnapshot, FanSnapshot, SensorSnapshot, Snapshot, Status};
use penna_proto::{ControlTick, Tick};
use std::fmt::Write;

const WIDTH: usize = 80;
const LABEL: usize = 22;

#[derive(Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    fn wrap(self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_owned() }
    }
    fn bold(self, s: &str) -> String {
        self.wrap("1", s)
    }
    fn dim(self, s: &str) -> String {
        self.wrap("2", s)
    }
    fn urgent(self, s: &str) -> String {
        self.wrap("31", s)
    }
}

/// One frame of what the daemon says: emergencies first, then temperatures, the fans it drives, what firmware still runs, and
/// what it cannot reach. `via` names where the daemon is.
pub fn tick_frame(t: &Tick, style: Style, via: &str, mock: bool) -> String {
    let mut out = String::new();
    let profile = t.profiles.iter().find(|p| p.id == t.profile).map_or(t.profile.as_str(), |p| p.label.as_str());
    let fans: u32 = t.controls.iter().map(|c| c.fans.max(1)).sum();
    let summary = format!(
        " · {profile}{} · {}{}",
        if t.full_speed { " · full speed" } else { "" },
        count(fans as usize, "fan"),
        if mock { " · mock" } else { "" }
    );
    let left = format!("{}{}", style.bold("Penna"), style.dim(&summary));
    line_with_right(&mut out, &left, 5 + summary.chars().count(), &format!("via {via}"), style);

    for e in &t.emergency {
        let at = e.celsius.map_or("—".to_owned(), |c| format!("{c:.1} °C"));
        let text = format!(
            "Emergency: {} at {at}, limit {} °C. Every fan at 100 % until {} °C.",
            e.label,
            trim_number(e.limit, 1),
            trim_number(e.release, 1)
        );
        let _ = writeln!(out, "{}", style.urgent(&text));
    }

    out.push('\n');
    let _ = writeln!(out, "{}", style.bold("Temperatures"));
    let width = t.sensors.iter().map(|s| penna_proto::build::display(s).chars().count() + 2).max().unwrap_or(0).clamp(LABEL, 48);
    for s in &t.sensors {
        let name = penna_proto::build::display(s);
        let value = match s.celsius {
            Some(c) => format!("{c:>6.1} °C"),
            None => format!("{:>6} °C", "—"),
        };
        let note = match (&s.error, s.below_ambient) {
            (Some(e), _) => format!("   {}", style.urgent(&format!("read failed: {e}"))),
            (None, true) => format!("   {}", style.dim("below room temperature")),
            _ => String::new(),
        };
        let _ = writeln!(out, "  {}{value}{note}", pad(&name, width));
    }

    if !t.controls.is_empty() {
        out.push('\n');
        let _ = writeln!(out, "{}", style.bold("Fans"));
        for c in &t.controls {
            fan_row(&mut out, c, style, t.t);
        }
    }
    if !t.firmware.is_empty() {
        out.push('\n');
        let names: Vec<&str> = t.firmware.iter().map(|f| f.label.as_str()).collect();
        let _ = writeln!(out, "{} {}", style.bold("Firmware runs"), style.dim(&names.join(" · ")));
    }
    if !t.unreachable.is_empty() {
        out.push('\n');
        let _ = writeln!(out, "{}", style.bold("Not reachable"));
        for u in &t.unreachable {
            let _ = writeln!(out, "  {}  {}", pad(&u.label, LABEL + 16), style.dim(&u.reason));
        }
    }
    if !t.notes.is_empty() {
        out.push('\n');
        for n in &t.notes {
            let _ = writeln!(out, "{}", style.dim(n));
        }
    }
    out
}

/// `CPU cooler ×3     1,140 RPM    51 % ↑ 52 %   CPU quiet`, with what is wrong in words when something is.
fn fan_row(out: &mut String, c: &ControlTick, style: Style, now: f64) {
    let name = if c.fans > 1 { format!("{} ×{}", c.label, c.fans) } else { c.label.clone() };
    let rpm = match c.rpm {
        Some(r) => format!("{:>6} RPM", thousands(r)),
        None => format!("{:>6} RPM", "—"),
    };
    let held_by_driver = c.state == State::Handback;
    let duty = if held_by_driver || matches!(c.state, State::Firmware | State::Absent) {
        format!("{:>6}", "")
    } else {
        format!("{:>4.0} %", c.duty)
    };
    let trend = match c.trend {
        Trend::Rising => format!("↑ {:>3.0} %", c.target),
        Trend::Falling => format!("↓ {:>3.0} %", c.target),
        Trend::Steady => " ".repeat(7),
    };
    let curve = c.curve_label.as_deref().unwrap_or("");
    let what = match c.state {
        State::Curve => style.dim(curve),
        State::Handback => style.dim(&format!("{curve} · below 30 %: the driver's curve runs it")),
        State::Override => match c.override_ {
            Some(o) => {
                let left = o.until.map(|u| (u - now).max(0.0));
                let how_long = left.map_or(" until cleared".to_owned(), |l| format!(" · {l:.0} s left"));
                style.dim(&format!("held at {:.0} %{how_long}", o.duty))
            }
            None => style.dim("full speed"),
        },
        State::Measuring => style.dim("setup is measuring it"),
        State::Firmware => style.dim("firmware"),
        State::Absent => style.dim("not on this machine"),
        State::Failsafe | State::Emergency | State::Contested | State::Fault => {
            let why = c.reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default();
            style.urgent(&format!("{}{why}", c.state.name().to_uppercase()))
        }
    };
    let _ = writeln!(out, "  {}{rpm}  {duty} {trend}  {what}", pad(&name, LABEL));
}

fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One frame. `interval` is shown in the header when the frame is one of a live series.
pub fn frame(snap: &Snapshot, style: Style, interval: Option<f64>) -> String {
    let mut out = String::new();
    let fans: usize = snap.devices.iter().map(|d| d.fans.len()).sum();
    let summary = format!(
        " · {} · {} · {}",
        count(snap.devices.len(), "device"),
        count(snap.devices.iter().map(|d| d.sensors.len()).sum(), "sensor"),
        count(fans, "fan")
    );
    let left = format!("{}{}", style.bold("Penna"), style.dim(&summary));
    let right = interval.map(|s| format!("every {} s · Ctrl-C quits", trim_number(s, 1))).unwrap_or_default();
    line_with_right(&mut out, &left, 5 + summary.chars().count(), &right, style);

    let mut devices: Vec<&DeviceSnapshot> = snap.devices.iter().collect();
    devices.sort_by_key(|d| rank(d));
    for d in devices {
        out.push('\n');
        device(&mut out, d, style);
    }

    if !snap.unreachable.is_empty() {
        out.push('\n');
        let _ = writeln!(out, "{}", style.bold("Not reachable"));
        for u in &snap.unreachable {
            let _ = writeln!(out, "  {}  {}", pad(&u.label, LABEL + 16), style.dim(&u.reason));
        }
    }
    if !snap.notes.is_empty() {
        out.push('\n');
        for n in &snap.notes {
            let _ = writeln!(out, "{}", style.dim(n));
        }
    }
    out
}

fn device(out: &mut String, d: &DeviceSnapshot, style: Style) {
    let title = match &d.model {
        Some(m) => format!("{} · {m}", d.label),
        None => d.label.clone(),
    };
    let meta = format!("{} · {}", d.driver, d.location);
    line_with_right(out, &style.bold(&title), title.chars().count(), &meta, style);

    for s in &d.sensors {
        sensor(out, s, style);
    }
    // An NVML fan is one control and one tach under one key: one row.
    for c in &d.controls {
        let tach = d.fans.iter().find(|f| f.key == c.key);
        control(out, c, tach, d.source, style);
    }
    for f in d.fans.iter().filter(|f| !d.controls.iter().any(|c| c.key == f.key)) {
        let (rpm, problem) = rpm_cell(f);
        let _ = writeln!(
            out,
            "  {}{}{}{}",
            pad(&f.label, LABEL),
            " ".repeat(9),
            rpm,
            problem.map(|p| format!("   {}", style.urgent(&p))).unwrap_or_default()
        );
    }
}

fn sensor(out: &mut String, s: &SensorSnapshot, style: Style) {
    let value = match s.celsius {
        Some(c) => format!("{:>6.1} °C", c),
        None => format!("{:>6} °C", "—"),
    };
    let mut meta: Vec<String> = Vec::new();
    if let Some(p) = problem(&s.status, "°C") {
        meta.push(style.urgent(&p));
    }
    if !s.limits.is_empty() {
        meta.push(style.dim(&limits(&s.limits)));
    }
    for l in &s.ignored_limits {
        meta.push(style.dim(&format!("{} {} ignored", limit_name(l.kind), trim_number(l.celsius, 2))));
    }
    if s.below_ambient {
        meta.push(style.dim("below room temperature · never a default source"));
    }
    let sep = style.dim(" · ");
    let meta = if meta.is_empty() { String::new() } else { format!("   {}", meta.join(&sep)) };
    let _ = writeln!(out, "  {}{value}{meta}", pad(&s.label, LABEL));
}

fn control(out: &mut String, c: &ControlSnapshot, tach: Option<&FanSnapshot>, source: Source, style: Style) {
    let value = match c.percent {
        Some(p) => format!("{:>6.0} % ", p),
        None => format!("{:>6} % ", "—"),
    };
    let (rpm, rpm_problem) = tach.map(rpm_cell).unwrap_or_else(|| (" ".repeat(12), None));
    let mut meta: Vec<String> = Vec::new();
    if let Some(p) = problem(&c.status, "%").or(rpm_problem) {
        meta.push(style.urgent(&p));
    }
    if let Some(mode) = c.mode {
        meta.push(style.dim(&mode_name(mode, source)));
    }
    if let Some(raw) = c.raw {
        meta.push(style.dim(&format!("pwm {raw}")));
    }
    if let ControlKind::Nvml { min, max } = c.kind {
        meta.push(style.dim(&format!("settable {min}–{max} %")));
    }
    let sep = style.dim(" · ");
    let _ = writeln!(out, "  {}{value}{rpm}   {}", pad(&c.label, LABEL), meta.join(&sep));
}

/// `"  1000 RPM"` in a fixed 12-column cell, and what went wrong if there is no number.
fn rpm_cell(f: &FanSnapshot) -> (String, Option<String>) {
    match f.rpm {
        Some(r) => (format!("{:>8} RPM", r), None),
        None => (format!("{:>8} RPM", "—"), problem(&f.status, "RPM")),
    }
}

fn problem(status: &Status, unit: &str) -> Option<String> {
    if let Some(v) = status.implausible {
        return Some(format!("implausible reading: {} {unit}", trim_number(v, 2)));
    }
    status.error.as_ref().map(|e| format!("read failed: {e}"))
}

fn mode_name(mode: ControlMode, source: Source) -> String {
    match (mode, source) {
        (ControlMode::Driver, _) => "driver's curve".to_owned(),
        (ControlMode::Manual, Source::Nvml) => "manual".to_owned(),
        (ControlMode::Manual, Source::Hwmon) => "manual (pwm_enable 1)".to_owned(),
        (ControlMode::Full, _) => "full speed (pwm_enable 0)".to_owned(),
        (ControlMode::Automatic(v), _) => format!("firmware (pwm_enable {v})"),
        (ControlMode::Unknown, _) => "mode unknown".to_owned(),
    }
}

fn limits(limits: &[Limit]) -> String {
    limits.iter().map(|l| format!("{} {}", limit_name(l.kind), trim_number(l.celsius, 2))).collect::<Vec<_>>().join(" · ")
}

fn limit_name(kind: LimitKind) -> &'static str {
    match kind {
        LimitKind::Max | LimitKind::GpuMax => "max",
        LimitKind::Crit => "crit",
        LimitKind::Slowdown => "slowdown",
        LimitKind::Shutdown => "shutdown",
    }
}

/// CPUs first, then GPUs, motherboard, storage; sensors that only read below room temperature last.
fn rank(d: &DeviceSnapshot) -> u8 {
    if !d.sensors.is_empty() && d.sensors.iter().all(|s| s.below_ambient) && d.fans.is_empty() {
        return 5;
    }
    match (d.source, d.driver.as_str()) {
        (_, "k10temp" | "zenpower" | "coretemp") => 0,
        (Source::Nvml, _) | (_, "amdgpu") => 1,
        (_, d) if penna_hw::hwmon::is_motherboard(d) => 2,
        (_, "nvme" | "drivetemp") => 3,
        _ => 4,
    }
}

fn line_with_right(out: &mut String, left: &str, left_width: usize, right: &str, style: Style) {
    if right.is_empty() {
        let _ = writeln!(out, "{left}");
        return;
    }
    let gap = WIDTH.saturating_sub(left_width + right.chars().count()).max(2);
    let _ = writeln!(out, "{left}{}{}", " ".repeat(gap), style.dim(right));
}

fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width { format!("{s} ") } else { format!("{s}{}", " ".repeat(width - n)) }
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// `93`, `81.85`, `0.5`: up to `places` decimals, no trailing zeros.
fn trim_number(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penna_core::Key;
    use penna_proto::VERSION;

    fn snap() -> Snapshot {
        let uuid = "GPU-1";
        let fan = Key::nvml_fan(uuid, 0);
        Snapshot {
            v: VERSION,
            seq: 1,
            t: 0.0,
            devices: vec![
                DeviceSnapshot {
                    id: "hwmon/acpitz/thermal_zone0".into(),
                    source: Source::Hwmon,
                    driver: "acpitz".into(),
                    label: "ACPI thermal zone".into(),
                    location: "thermal_zone0".into(),
                    model: None,
                    sensors: vec![SensorSnapshot {
                        key: Key::hwmon("acpitz", "thermal_zone0", "temp1"),
                        label: "temp1".into(),
                        celsius: Some(16.8),
                        status: Status::default(),
                        limits: vec![],
                        ignored_limits: vec![],
                        below_ambient: true,
                    }],
                    fans: vec![],
                    controls: vec![],
                },
                DeviceSnapshot {
                    id: "hwmon/k10temp/0000:00:18.3".into(),
                    source: Source::Hwmon,
                    driver: "k10temp".into(),
                    label: "AMD CPU".into(),
                    location: "0000:00:18.3".into(),
                    model: None,
                    sensors: vec![SensorSnapshot {
                        key: Key::hwmon("k10temp", "0000:00:18.3", "temp1"),
                        label: "Tctl".into(),
                        celsius: Some(78.5),
                        status: Status::default(),
                        limits: vec![],
                        ignored_limits: vec![],
                        below_ambient: false,
                    }],
                    fans: vec![],
                    controls: vec![],
                },
                DeviceSnapshot {
                    id: format!("nvml/{uuid}"),
                    source: Source::Nvml,
                    driver: "nvml".into(),
                    label: "NVIDIA GeForce RTX 3090".into(),
                    location: "0000:09:00.0".into(),
                    model: None,
                    sensors: vec![],
                    fans: vec![FanSnapshot { key: fan.clone(), label: "Fan 1".into(), rpm: Some(998), status: Status::default() }],
                    controls: vec![ControlSnapshot {
                        key: fan,
                        label: "Fan 1".into(),
                        kind: ControlKind::Nvml { min: 30, max: 100 },
                        percent: Some(53.0),
                        raw: None,
                        mode: Some(ControlMode::Driver),
                        status: Status::default(),
                    }],
                },
            ],
            unreachable: vec![],
            notes: vec!["Motherboard fans: none visible.".into()],
        }
    }

    #[test]
    fn cpu_first_cold_sensors_last_and_columns_fixed() {
        let f = frame(&snap(), Style { color: false }, None);
        let lines: Vec<&str> = f.lines().collect();
        assert!(lines[0].starts_with("Penna · 3 devices · 2 sensors · 1 fan"), "{f}");
        let cpu = f.find("AMD CPU").unwrap();
        let gpu = f.find("NVIDIA GeForce RTX 3090").unwrap();
        let acpi = f.find("ACPI thermal zone").unwrap();
        assert!(cpu < gpu && gpu < acpi, "{f}");
        assert!(f.contains("  Tctl                    78.5 °C\n"), "{f}");
        assert!(f.contains("  Fan 1                     53 %      998 RPM   driver's curve · settable 30–100 %\n"), "{f}");
        assert!(f.contains("16.8 °C   below room temperature · never a default source"), "{f}");
        assert!(f.ends_with("Motherboard fans: none visible.\n"), "{f}");
        assert!(!f.contains('\x1b'));
    }

    #[test]
    fn a_tick_reads_like_a_person_would_say_it() {
        let t: Tick = match serde_json::from_str(include_str!("../../penna-proto/tests/fixtures/tick.json")).unwrap() {
            penna_proto::Event::Tick(t) => *t,
            _ => unreachable!(),
        };
        let f = tick_frame(&t, Style { color: false }, "/run/penna/penna.sock", false);
        assert!(f.starts_with("Penna · Balanced · 6 fans"), "{f}");
        assert!(f.contains("Emergency: AMD CPU · Tctl at 94.5 °C, limit 90 °C. Every fan at 100 % until 85 °C."), "{f}");
        assert!(f.contains("  CPU cooler             1,140 RPM   100 %          EMERGENCY: "), "{f}");
        assert!(f.contains("  Front intake ×3        1,057 RPM    35 % ↑  51 %  CONTESTED: wrote 35 %"), "{f}");
        assert!(f.contains("GPU fan 1                  0 RPM                  GPU · below 30 %: the driver's curve runs it"), "{f}");
        assert!(f.contains("NVMe · Composite"), "{f}");
        assert!(f.contains("Firmware runs pwm4"), "{f}");
    }

    #[test]
    fn numbers() {
        assert_eq!(thousands(1140), "1,140");
        assert_eq!(thousands(980), "980");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(trim_number(93.0, 2), "93");
        assert_eq!(trim_number(81.85, 2), "81.85");
        assert_eq!(trim_number(1.0, 1), "1");
    }
}
