//! The machine alone, without the daemon: what `penna watch` reads directly when no `pennad` answers, and prints as
//! `{"ev":"hardware",…}` with `--json`. Version 1 of the wire format, kept for that one use.
use penna_core::{ControlKind, ControlMode, Inventory, Key, Limit, Readings, Sample, Source, Unreachable};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

/// Everything the hardware said at one pass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub v: u32,
    pub seq: u64,
    /// Unix time in seconds.
    pub t: f64,
    pub devices: Vec<DeviceSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreachable: Vec<Unreachable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub id: String,
    pub source: Source,
    pub driver: String,
    pub label: String,
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub sensors: Vec<SensorSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fans: Vec<FanSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlSnapshot>,
}

/// One value or why there is none. At most one field is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// A number the hardware reported that cannot be what it claims (a temperature of 4000 °C).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implausible: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Its device is asleep, which is not a failed read (DESIGN.md §4.2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asleep: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorSnapshot {
    pub key: Key,
    pub label: String,
    pub celsius: Option<f64>,
    #[serde(flatten)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<Limit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_limits: Vec<Limit>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub below_ambient: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FanSnapshot {
    pub key: Key,
    pub label: String,
    pub rpm: Option<u32>,
    #[serde(flatten)]
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlSnapshot {
    pub key: Key,
    pub label: String,
    pub kind: ControlKind,
    /// PWM duty for hwmon, NVML percent for NVML.
    pub percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<u32>,
    pub mode: Option<ControlMode>,
    #[serde(flatten)]
    pub status: Status,
}

impl Snapshot {
    pub fn new(seq: u64, t: f64, inventory: &Inventory, readings: &Readings) -> Snapshot {
        let devices = inventory
            .devices
            .iter()
            .map(|d| DeviceSnapshot {
                id: d.id.clone(),
                source: d.source,
                driver: d.driver.clone(),
                label: d.label.clone(),
                location: d.location.clone(),
                model: d.model.clone(),
                sensors: d
                    .sensors
                    .iter()
                    .map(|s| {
                        let (celsius, status) = split(readings.temps.get(&s.key));
                        SensorSnapshot {
                            key: s.key.clone(),
                            label: s.label.clone(),
                            celsius,
                            status,
                            limits: s.limits.clone(),
                            ignored_limits: s.ignored_limits.clone(),
                            below_ambient: s.below_ambient,
                        }
                    })
                    .collect(),
                fans: d
                    .tachs
                    .iter()
                    .map(|f| {
                        let (rpm, status) = split(readings.rpms.get(&f.key));
                        FanSnapshot { key: f.key.clone(), label: f.label.clone(), rpm, status }
                    })
                    .collect(),
                controls: d
                    .controls
                    .iter()
                    .map(|c| {
                        let (reading, status) = split(readings.controls.get(&c.key));
                        ControlSnapshot {
                            key: c.key.clone(),
                            label: c.label.clone(),
                            kind: c.kind,
                            percent: reading.map(|r| r.percent),
                            raw: reading.and_then(|r| r.raw),
                            mode: reading.map(|r| r.mode),
                            status,
                        }
                    })
                    .collect(),
            })
            .collect();
        Snapshot { v: VERSION, seq, t, devices, unreachable: inventory.unreachable.clone(), notes: inventory.notes.clone() }
    }
}

fn split<T: Copy>(sample: Option<&Sample<T>>) -> (Option<T>, Status) {
    match sample {
        Some(Sample::Value(v)) => (Some(*v), Status::default()),
        Some(Sample::Implausible(v)) => (None, Status { implausible: Some(*v), ..Status::default() }),
        Some(Sample::Error(e)) => (None, Status { error: Some(e.clone()), ..Status::default() }),
        Some(Sample::Asleep) => (None, Status { asleep: true, ..Status::default() }),
        None => (None, Status { error: Some("not read".to_owned()), ..Status::default() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penna_core::{ControlInfo, ControlReading, Device, LimitKind, SensorInfo, TachInfo};

    fn desk() -> (Inventory, Readings) {
        let uuid = "GPU-168e2769-d673-f36a-ea7b-64df57f8d090";
        let cpu = Key::hwmon("k10temp", "0000:00:18.3", "temp1");
        let nvme = Key::hwmon("nvme", "0000:04:00.0", "temp2");
        let fan = Key::nvml_fan(uuid, 0);
        let inventory = Inventory {
            devices: vec![
                Device {
                    id: "hwmon/k10temp/0000:00:18.3".into(),
                    source: Source::Hwmon,
                    driver: "k10temp".into(),
                    label: "AMD CPU".into(),
                    location: "0000:00:18.3".into(),
                    model: None,
                    sensors: vec![SensorInfo {
                        key: cpu.clone(),
                        label: "Tctl".into(),
                        limits: vec![],
                        ignored_limits: vec![],
                        below_ambient: false,
                    }],
                    tachs: vec![],
                    controls: vec![],
                },
                Device {
                    id: "hwmon/nvme/0000:04:00.0".into(),
                    source: Source::Hwmon,
                    driver: "nvme".into(),
                    label: "NVMe".into(),
                    location: "0000:04:00.0".into(),
                    model: Some("Samsung SSD 980 PRO 1TB".into()),
                    sensors: vec![SensorInfo {
                        key: nvme.clone(),
                        label: "Sensor 1".into(),
                        limits: vec![],
                        ignored_limits: vec![Limit { kind: LimitKind::Max, celsius: 65261.85 }],
                        below_ambient: false,
                    }],
                    tachs: vec![],
                    controls: vec![],
                },
                Device {
                    id: format!("nvml/{uuid}"),
                    source: Source::Nvml,
                    driver: "nvml".into(),
                    label: "NVIDIA GeForce RTX 3090".into(),
                    location: "0000:09:00.0".into(),
                    model: None,
                    sensors: vec![],
                    tachs: vec![TachInfo { key: fan.clone(), label: "Fan 1".into() }],
                    controls: vec![ControlInfo { key: fan.clone(), label: "Fan 1".into(), kind: ControlKind::Nvml { min: 30, max: 100 } }],
                },
            ],
            unreachable: vec![Unreachable {
                id: "pci/0000:0a:00.0".into(),
                label: "NVIDIA TU106 [GeForce RTX 2060 SUPER]".into(),
                reason: "passed through to a VM (vfio-pci)".into(),
            }],
            notes: vec![],
        };
        let mut readings = Readings::default();
        readings.temps.insert(cpu, Sample::Value(78.5));
        readings.temps.insert(nvme, Sample::Error("Input/output error".into()));
        readings.rpms.insert(fan.clone(), Sample::Value(998));
        readings.controls.insert(fan, Sample::Value(ControlReading { percent: 53.0, raw: None, mode: ControlMode::Driver }));
        (inventory, readings)
    }

    #[test]
    fn snapshot_carries_values_and_reasons() {
        let (inventory, readings) = desk();
        let s = Snapshot::new(7, 1_760_000_000.5, &inventory, &readings);
        assert_eq!(s.devices[0].sensors[0].celsius, Some(78.5));
        assert_eq!(s.devices[1].sensors[0].status.error.as_deref(), Some("Input/output error"));
        assert_eq!(s.devices[2].fans[0].rpm, Some(998));
        assert_eq!(s.devices[2].controls[0].mode, Some(ControlMode::Driver));
    }

    #[test]
    fn hardware_is_one_json_object_and_round_trips() {
        let (inventory, readings) = desk();
        let ev = crate::Event::Hardware(Snapshot::new(1, 0.0, &inventory, &readings));
        let line = serde_json::to_string(&ev).unwrap();
        assert!(line.starts_with(r#"{"ev":"hardware","v":1,"seq":1"#), "{line}");
        assert!(!line.contains('\n'));
        assert!(line.contains(r#""ignored_limits":[{"kind":"max","celsius":65261.85}]"#), "{line}");
        assert!(line.contains(r#""kind":{"type":"nvml","min":30,"max":100}"#), "{line}");
        assert!(line.contains(r#""mode":"driver""#), "{line}");
        assert_eq!(serde_json::from_str::<crate::Event>(&line).unwrap(), ev);
    }
}
