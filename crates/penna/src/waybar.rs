//! One JSON line for a Waybar custom module (DESIGN.md §8.1, §8.5): the same glyph and state the shell's bar widget shows, for
//! desktops without Omarchy's shell.
use penna_core::control::State;
use penna_proto::{ControlTick, Tick};
use serde::Serialize;

/// Material Design glyphs from the Nerd Font (§8.1).
pub const FAN: char = '\u{F0210}';
pub const FAN_UP: char = '\u{F146E}';
pub const FAN_CLOCK: char = '\u{F1A3A}';
pub const FAN_ALERT: char = '\u{F146C}';
pub const FAN_OFF: char = '\u{F081D}';
pub const FAN_AUTO: char = '\u{F171D}';

#[derive(Debug, PartialEq, Serialize)]
pub struct Line {
    pub text: String,
    /// The state, for Waybar's `format-icons` by `alt`.
    pub alt: &'static str,
    pub tooltip: String,
    pub class: &'static str,
}

/// What the bar says when no daemon answers.
pub fn offline() -> Line {
    Line {
        text: FAN_OFF.to_string(),
        alt: "offline",
        tooltip: "Penna isn't running. The fans are on firmware control.".into(),
        class: "offline",
    }
}

fn alarming(c: &ControlTick) -> bool {
    matches!(c.state, State::Failsafe | State::Emergency | State::Contested | State::Fault)
}

/// `sensor`: a sensor's key or label to show beside the glyph, as the widget setting does.
pub fn line(t: &Tick, sensor: Option<&str>) -> Line {
    let profile = t.profiles.iter().find(|p| p.id == t.profile).map_or(t.profile.as_str(), |p| p.label.as_str());
    let mut tips: Vec<String> = Vec::new();
    for e in &t.emergency {
        tips.push(format!("Emergency: {} at {} °C, limit {} °C", e.label, e.celsius.map_or("—".into(), |c| format!("{c:.0}")), e.limit));
    }
    for c in t.controls.iter().filter(|c| alarming(c)) {
        tips.push(format!("{}: {}", c.label, c.reason.as_deref().unwrap_or(c.state.name())));
    }
    let (glyph, alt, class) = if !t.emergency.is_empty() || t.controls.iter().any(alarming) {
        (FAN_ALERT, "alert", "alert")
    } else if t.full_speed {
        (FAN_UP, "full-speed", "override")
    } else if let Some(o) = t.controls.iter().filter_map(|c| c.override_).next() {
        if o.until.is_some() { (FAN_CLOCK, "override", "override") } else { (FAN_UP, "override", "override") }
    } else if !t.controls.iter().any(|c| matches!(c.state, State::Curve | State::Handback | State::Override)) {
        (FAN_AUTO, "firmware", "firmware")
    } else {
        (FAN, "normal", "normal")
    };
    tips.push(format!("Penna · {profile}{}", if t.full_speed { " · full speed" } else { "" }));
    for c in &t.controls {
        let rpm = c.rpm.map_or("—".to_owned(), |r| format!("{r} RPM"));
        tips.push(format!("{}  {rpm}", c.label));
    }
    let mut text = glyph.to_string();
    if let Some(name) = sensor {
        let lower = name.to_lowercase();
        let found = t.sensors.iter().find(|s| s.key.as_str() == name || s.label.to_lowercase() == lower);
        if let Some(c) = found.and_then(|s| s.celsius) {
            text.push_str(&format!(" {c:.0}°"));
        }
    }
    Line { text, alt, tooltip: tips.join("\n"), class }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden() -> Tick {
        match serde_json::from_str(include_str!("../../penna-proto/tests/fixtures/tick.json")).unwrap() {
            penna_proto::Event::Tick(t) => *t,
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_emergency_is_an_alert_glyph_with_words() {
        let l = line(&golden(), Some("Tctl"));
        assert_eq!((l.alt, l.class), ("alert", "alert"));
        assert_eq!(l.text, format!("{FAN_ALERT} 94°"));
        assert!(l.tooltip.starts_with("Emergency: AMD CPU · Tctl at 94 °C, limit 90 °C"), "{}", l.tooltip);
    }

    #[test]
    fn calm_full_speed_and_firmware() {
        let mut t = golden();
        t.emergency.clear();
        t.controls.retain(|c| !matches!(c.state, State::Emergency | State::Contested));
        t.controls.iter_mut().for_each(|c| c.override_ = None);
        assert_eq!(line(&t, None).text, FAN.to_string());
        t.full_speed = true;
        assert_eq!(line(&t, None).class, "override");
        t.full_speed = false;
        t.controls.iter_mut().for_each(|c| c.state = State::Firmware);
        assert_eq!(line(&t, None).alt, "firmware");
        assert!(serde_json::to_string(&offline()).unwrap().contains("\"class\":\"offline\""));
    }
}
