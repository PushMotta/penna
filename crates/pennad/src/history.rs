//! The last [`HISTORY`] ticks of every sensor and control, for the `history` event a subscriber gets first (DESIGN.md §7.3):
//! a shell that has just restarted draws full sparklines on its first frame.
use penna_core::Key;
use penna_proto::{ControlHistory, HISTORY, History, Tick};
use std::collections::{BTreeMap, VecDeque};

#[derive(Default)]
pub struct Ring {
    len: usize,
    sensors: BTreeMap<Key, VecDeque<Option<f64>>>,
    duty: BTreeMap<Key, VecDeque<Option<f64>>>,
    rpm: BTreeMap<Key, VecDeque<Option<u32>>>,
}

impl Ring {
    /// One tick. Every series stays the same length: a key that appears late starts with `null`s, and one that is gone gets
    /// `null` until it falls out of the window.
    pub fn push(&mut self, t: &Tick) {
        let len = self.len;
        for s in &t.sensors {
            self.sensors.entry(s.key.clone()).or_insert_with(|| fresh(len)).push_back(s.celsius);
        }
        for c in &t.controls {
            self.duty.entry(c.key.clone()).or_insert_with(|| fresh(len)).push_back(Some(c.duty));
            self.rpm.entry(c.key.clone()).or_insert_with(|| fresh(len)).push_back(c.rpm);
        }
        self.len += 1;
        for series in self.sensors.values_mut().chain(self.duty.values_mut()) {
            if series.len() < self.len {
                series.push_back(None);
            }
        }
        for series in self.rpm.values_mut() {
            if series.len() < self.len {
                series.push_back(None);
            }
        }
        if self.len > HISTORY {
            self.len = HISTORY;
            for s in self.sensors.values_mut().chain(self.duty.values_mut()) {
                s.pop_front();
            }
            for s in self.rpm.values_mut() {
                s.pop_front();
            }
        }
        self.sensors.retain(|_, s| s.iter().any(Option::is_some));
        self.duty.retain(|k, s| s.iter().any(Option::is_some) || self.rpm.get(k).is_some_and(|r| r.iter().any(Option::is_some)));
        let duty = &self.duty;
        self.rpm.retain(|k, _| duty.contains_key(k));
    }

    pub fn snapshot(&self, seq: u64, tick: f64) -> History {
        History {
            seq,
            tick,
            sensors: self.sensors.iter().map(|(k, v)| (k.clone(), v.iter().copied().collect())).collect(),
            controls: self
                .duty
                .iter()
                .map(|(k, d)| {
                    let rpm = self.rpm.get(k).map(|r| r.iter().copied().collect()).unwrap_or_default();
                    (k.clone(), ControlHistory { duty: d.iter().copied().collect(), rpm })
                })
                .collect(),
        }
    }
}

fn fresh<T: Clone>(n: usize) -> VecDeque<Option<T>> {
    VecDeque::from(vec![None; n])
}

#[cfg(test)]
mod tests {
    use super::*;
    use penna_proto::SensorTick;

    fn tick(sensors: &[(&str, Option<f64>)]) -> Tick {
        Tick {
            v: 2,
            seq: 0,
            t: 0.0,
            profile: "p".into(),
            profiles: vec![],
            full_speed: false,
            draft: None,
            emergency: vec![],
            sensors: sensors
                .iter()
                .map(|(k, c)| SensorTick {
                    key: Key::parse(k).unwrap(),
                    label: String::new(),
                    device: String::new(),
                    celsius: *c,
                    scale: [20.0, 100.0],
                    limits: vec![],
                    is_virtual: false,
                    below_ambient: false,
                    error: None,
                })
                .collect(),
            controls: vec![],
            firmware: vec![],
            curves: vec![],
            unreachable: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn series_stay_aligned_and_capped() {
        let a = "hwmon/k10temp/0000:00:18.3/temp1";
        let b = "hwmon/nvme/0000:04:00.0/temp1";
        let mut r = Ring::default();
        r.push(&tick(&[(a, Some(40.0))]));
        r.push(&tick(&[(a, Some(41.0)), (b, Some(30.0))]));
        r.push(&tick(&[(b, None)]));
        let h = r.snapshot(3, 1.0);
        assert_eq!(h.sensors[&Key::parse(a).unwrap()], [Some(40.0), Some(41.0), None]);
        assert_eq!(h.sensors[&Key::parse(b).unwrap()], [None, Some(30.0), None]);
        for i in 0..HISTORY + 20 {
            r.push(&tick(&[(a, Some(i as f64))]));
        }
        let h = r.snapshot(0, 1.0);
        assert_eq!(h.sensors[&Key::parse(a).unwrap()].len(), HISTORY);
        assert_eq!(h.sensors[&Key::parse(a).unwrap()].last(), Some(&Some((HISTORY + 19) as f64)));
        assert!(!h.sensors.contains_key(&Key::parse(b).unwrap()), "a series with nothing left in the window goes");
    }
}
