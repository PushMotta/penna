//! Drift (DESIGN.md §5.6): a calibrated fan that turns well below what its table says for the duty it runs, steadily, gets a
//! line on its card: dust, or a tired bearing. Pure, like the loop: one tick's state and readings in, what changed out.
use crate::config::Measured;
use crate::control::{LoopState, State, Trend};
use crate::{Key, Readings, Sample};
use std::collections::BTreeMap;

/// Slower than calibrated by this share, steadily for [`DRIFT_SECONDS`], is drift.
pub const DRIFT_SLOWER: f64 = 0.25;
pub const DRIFT_SECONDS: f64 = 600.0;
/// A drifting fan is well again once it is back within this share.
pub const DRIFT_RECOVERED: f64 = 0.15;
/// Below this the table's RPM is too small to compare against.
const MIN_EXPECTED_RPM: f64 = 300.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DriftWatch {
    /// Since when each control has turned too slowly, without a break.
    since: BTreeMap<Key, f64>,
    /// Drifting controls, and how much slower than calibrated, as a share.
    pub drifting: BTreeMap<Key, f64>,
}

/// What a tick changed: a control that started drifting, or one that is well again.
#[derive(Clone, Debug, PartialEq)]
pub enum DriftChange {
    Started { key: Key, slower: f64 },
    Recovered { key: Key },
}

impl DriftWatch {
    /// Compares every calibrated fan the loop runs on its curve, at a steady duty, with its table.
    pub fn update(&mut self, state: &LoopState, readings: &Readings, measured: &Measured, now: f64) -> Vec<DriftChange> {
        let mut out = Vec::new();
        for m in &measured.controls {
            if m.calibration.len() < 2 || m.tachs.is_empty() {
                continue;
            }
            let Some(cs) = state.controls.get(&m.key) else { continue };
            // Only a fan on its curve at a steady duty says anything: a moving duty leaves the fan behind it.
            if cs.state != State::Curve || cs.handback || cs.trend != Trend::Steady {
                continue;
            }
            let expected = table_rpm(&m.calibration, cs.duty);
            let rpms: Vec<f64> = m.tachs.iter().filter_map(|t| readings.rpms.get(t).and_then(Sample::value)).map(f64::from).collect();
            if expected < MIN_EXPECTED_RPM || rpms.is_empty() {
                continue;
            }
            let actual = rpms.iter().sum::<f64>() / rpms.len() as f64;
            let slower = 1.0 - actual / expected;
            if slower >= DRIFT_SLOWER {
                let since = *self.since.entry(m.key.clone()).or_insert(now);
                if now - since >= DRIFT_SECONDS && !self.drifting.contains_key(&m.key) {
                    self.drifting.insert(m.key.clone(), slower);
                    out.push(DriftChange::Started { key: m.key.clone(), slower });
                } else if let Some(s) = self.drifting.get_mut(&m.key) {
                    *s = slower;
                }
            } else {
                self.since.remove(&m.key);
                if slower < DRIFT_RECOVERED && self.drifting.remove(&m.key).is_some() {
                    out.push(DriftChange::Recovered { key: m.key.clone() });
                }
            }
        }
        out
    }

    /// Calibrated again: whatever it said about this control no longer holds.
    pub fn forget(&mut self, key: &Key) {
        self.since.remove(key);
        self.drifting.remove(key);
    }
}

/// RPM at a duty, from a calibration table (ascending), linear between its points.
pub fn table_rpm(table: &[[f64; 2]], duty: f64) -> f64 {
    let Some(first) = table.first() else { return 0.0 };
    if duty < first[0] {
        return 0.0;
    }
    for w in table.windows(2) {
        let ([d0, r0], [d1, r1]) = (w[0], w[1]);
        if duty <= d1 {
            return r0 + (r1 - r0) * (duty - d0) / (d1 - d0).max(1e-9);
        }
    }
    table[table.len() - 1][1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeasuredControl;
    use crate::control::ControlState;

    fn key() -> Key {
        Key::parse("hwmon/it87/it87.2608/pwm2").unwrap()
    }
    fn tach() -> Key {
        Key::parse("hwmon/it87/it87.2608/fan2").unwrap()
    }

    fn setup(duty: f64, state: State, trend: Trend) -> (LoopState, Measured) {
        let mut st = LoopState::default();
        let mut cs = ControlState::default();
        (cs.state, cs.duty, cs.trend, cs.held) = (state, duty, trend, true);
        st.controls.insert(key(), cs);
        let m = Measured {
            controls: vec![MeasuredControl {
                tachs: vec![tach()],
                calibration: vec![[20.0, 480.0], [60.0, 1560.0], [100.0, 2190.0]],
                ..MeasuredControl::new(key())
            }],
            ..Measured::default()
        };
        (st, m)
    }

    fn reading(rpm: u32) -> Readings {
        let mut r = Readings::default();
        r.rpms.insert(tach(), Sample::Value(rpm));
        r
    }

    #[test]
    fn the_table_interpolates_and_stops_below_its_first_duty() {
        let t = [[20.0, 480.0], [60.0, 1560.0], [100.0, 2190.0]];
        assert_eq!(table_rpm(&t, 60.0), 1560.0);
        assert_eq!(table_rpm(&t, 40.0), 1020.0);
        assert_eq!(table_rpm(&t, 10.0), 0.0);
        assert_eq!(table_rpm(&t, 100.0), 2190.0);
    }

    #[test]
    fn ten_steady_minutes_well_below_the_table_is_drift_and_recovery_ends_it() {
        let (st, m) = setup(60.0, State::Curve, Trend::Steady);
        let mut w = DriftWatch::default();
        // 30 % slower than calibrated (1,560 RPM at 60 %).
        for t in 0..600 {
            assert!(w.update(&st, &reading(1090), &m, t as f64).is_empty(), "not before 10 minutes, at {t} s");
        }
        let c = w.update(&st, &reading(1090), &m, 600.0);
        assert!(
            matches!(&c[..], [DriftChange::Started { key, slower }] if key == &super::tests::key() && (slower - 0.30).abs() < 0.01),
            "{c:?}"
        );
        assert!(w.update(&st, &reading(1090), &m, 601.0).is_empty(), "said once");
        // 20 % slower: not yet well again (within 15 %), and no new drift either.
        assert!(w.update(&st, &reading(1250), &m, 602.0).is_empty());
        assert!(w.drifting.contains_key(&key()));
        assert_eq!(w.update(&st, &reading(1500), &m, 603.0), vec![DriftChange::Recovered { key: key() }]);
    }

    #[test]
    fn a_break_starts_the_ten_minutes_again_and_only_a_steady_curve_counts() {
        let (st, m) = setup(60.0, State::Curve, Trend::Steady);
        let mut w = DriftWatch::default();
        for t in 0..400 {
            w.update(&st, &reading(1090), &m, t as f64);
        }
        w.update(&st, &reading(1550), &m, 400.0);
        for t in 401..1000 {
            assert!(w.update(&st, &reading(1090), &m, t as f64).is_empty(), "the break restarted it, at {t} s");
        }
        assert!(!w.update(&st, &reading(1090), &m, 1001.0).is_empty());
        for (state, trend) in [(State::Override, Trend::Steady), (State::Curve, Trend::Rising), (State::Measuring, Trend::Steady)] {
            let (st, m) = setup(60.0, state, trend);
            let mut w = DriftWatch::default();
            for t in 0..700 {
                assert!(w.update(&st, &reading(500), &m, t as f64).is_empty(), "{state:?} {trend:?}");
            }
        }
    }
}
