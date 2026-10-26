//! `held.json`: the way back for every control Penna holds (DESIGN.md §6.1). An entry is written, synced and renamed into
//! place before its control leaves firmware, so whatever kills the daemon, `penna restore` can put every fan back.
use crate::Key;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeldFile {
    pub version: u32,
    /// The daemon that wrote it.
    pub pid: u32,
    pub controls: Vec<HeldEntry>,
}

/// Everything needed to hand one control back without discovering anything first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum HeldEntry {
    /// The exact `pwmN_enable` value found, never an assumed "automatic" (§6.1), and the duty it was running. Both go back, the
    /// duty first: on some chips it is also the automatic curve's start. A duty that couldn't be read, as `dell_smm`'s in
    /// automatic, is `None` and isn't written back; an enable that couldn't be read is the driver table's (§3.5). `also` are
    /// files Penna changed beside the control, put back after its mode: a ThinkPad's fan watchdog.
    Hwmon {
        key: Key,
        enable: PathBuf,
        original_enable: i64,
        pwm: PathBuf,
        original_pwm: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        also: Vec<Also>,
    },
    /// The simulator's file-backed NVML (§9.1).
    MockNvml { key: Key, policy: PathBuf, original_policy: String },
    /// A target speed (`lenovo_wmi_other`, applesmc): the target found, and the switch to manual with the value it held when the
    /// driver has one. The target goes back first, then the switch.
    Target {
        key: Key,
        target: PathBuf,
        original: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        switch: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original_switch: Option<i64>,
    },
    /// amdgpu's OverDrive fan curve: the points found, °C and %, written back and committed. It resets at boot.
    OdCurve { key: Key, path: PathBuf, original: Vec<[i64; 2]> },
    /// asus-wmi's custom fan curve: every point's file as found, then its enable. Nobody has checked whether the firmware keeps
    /// it across a reboot, so it goes to `firmware.json` too (§6.7).
    PointCurve { key: Key, enable: PathBuf, original_enable: i64, points: Vec<Also> },
    /// The machine's power profile (§3.5, §4.7), set through power-profiles-daemon or the firmware's file, and the one that was
    /// set before Penna changed it. It outlives a reboot, so the entry goes to `firmware.json` as well (§6.7).
    Platform { key: Key, via: PlatformVia, original: String },
    /// A real NVIDIA fan (M4): the policy it was on, `driver` (restored with `nvmlDeviceSetDefaultFanSpeed_v2`) or `manual`
    /// (restored with that policy and `original_speed`, the target speed it was holding).
    Nvml {
        key: Key,
        uuid: String,
        fan: u32,
        #[serde(default = "driver_policy")]
        original_policy: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original_speed: Option<u32>,
    },
}

/// A file Penna changed beside a control, and the value it held.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Also {
    pub path: PathBuf,
    pub value: String,
}

/// How a platform profile is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "kebab-case")]
pub enum PlatformVia {
    /// power-profiles-daemon's `ActiveProfile`, on the system bus.
    Daemon,
    /// `/sys/firmware/acpi/platform_profile`, when no daemon runs.
    Sysfs { path: PathBuf },
}

fn driver_policy() -> String {
    "driver".to_owned()
}

impl HeldEntry {
    pub fn key(&self) -> &Key {
        match self {
            HeldEntry::Hwmon { key, .. }
            | HeldEntry::MockNvml { key, .. }
            | HeldEntry::Nvml { key, .. }
            | HeldEntry::Target { key, .. }
            | HeldEntry::OdCurve { key, .. }
            | HeldEntry::PointCurve { key, .. }
            | HeldEntry::Platform { key, .. } => key,
        }
    }
}

impl HeldEntry {
    /// What it changed survives a reboot: it goes to `firmware.json` too, not only `held.json` in `/run` (§6.7).
    pub fn outlives_reboot(&self) -> bool {
        matches!(self, HeldEntry::Platform { .. } | HeldEntry::PointCurve { .. })
    }

    /// Every file putting it back writes.
    pub fn paths(&self) -> Vec<&std::path::Path> {
        match self {
            HeldEntry::Hwmon { enable, pwm, also, .. } => {
                [enable.as_path(), pwm.as_path()].into_iter().chain(also.iter().map(|a| a.path.as_path())).collect()
            }
            HeldEntry::MockNvml { policy, .. } => vec![policy.as_path()],
            HeldEntry::Target { target, switch, .. } => std::iter::once(target.as_path()).chain(switch.as_deref()).collect(),
            HeldEntry::OdCurve { path, .. } => vec![path.as_path()],
            HeldEntry::PointCurve { enable, points, .. } => {
                std::iter::once(enable.as_path()).chain(points.iter().map(|p| p.path.as_path())).collect()
            }
            HeldEntry::Platform { via: PlatformVia::Sysfs { path }, .. } => vec![path.as_path()],
            HeldEntry::Platform { via: PlatformVia::Daemon, .. } | HeldEntry::Nvml { .. } => Vec::new(),
        }
    }
}

impl HeldFile {
    pub fn parse(text: &str) -> Result<HeldFile, String> {
        let f: HeldFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if f.version != VERSION {
            return Err(format!("held.json version {} is not {VERSION}", f.version));
        }
        Ok(f)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("held entries always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let f = HeldFile {
            version: VERSION,
            pid: 42,
            controls: vec![
                HeldEntry::Hwmon {
                    key: Key::parse("hwmon/it87/it87.2608/pwm3").unwrap(),
                    enable: "/tmp/x/pwm3_enable".into(),
                    original_enable: 5,
                    pwm: "/tmp/x/pwm3".into(),
                    original_pwm: Some(102),
                    also: vec![],
                },
                HeldEntry::MockNvml {
                    key: Key::parse("nvml/GPU-1/fan0").unwrap(),
                    policy: "/tmp/x/fan0_policy".into(),
                    original_policy: "driver".into(),
                },
            ],
        };
        assert_eq!(HeldFile::parse(&f.to_json()).unwrap(), f);
        assert!(f.to_json().contains("\"kind\": \"hwmon\""));
        assert!(!f.to_json().contains("also"), "nothing beside the control: nothing written");
    }

    #[test]
    fn a_duty_that_could_not_be_read_and_files_beside_the_control() {
        let entry = HeldEntry::Hwmon {
            key: Key::parse("hwmon/dell_smm/dell_smm-hwmon/pwm1").unwrap(),
            enable: "/tmp/x/pwm1_enable".into(),
            original_enable: 2,
            pwm: "/tmp/x/pwm1".into(),
            original_pwm: None,
            also: vec![Also { path: "/tmp/x/fan_watchdog".into(), value: "0".into() }],
        };
        let f = HeldFile { version: VERSION, pid: 3, controls: vec![entry] };
        assert_eq!(HeldFile::parse(&f.to_json()).unwrap(), f);
        // A file an earlier Penna wrote: the duty as a plain number, no `also`.
        let old = r#"{"version":1,"pid":1,"controls":[{"kind":"hwmon","key":"hwmon/it87/it87.2608/pwm3","enable":"/e","original_enable":5,"pwm":"/p","original_pwm":102}]}"#;
        match &HeldFile::parse(old).unwrap().controls[0] {
            HeldEntry::Hwmon { original_pwm, also, .. } => assert_eq!((*original_pwm, also.len()), (Some(102), 0)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nvml_entries_round_trip_and_default_to_the_driver() {
        let manual = HeldEntry::Nvml {
            key: Key::parse("nvml/GPU-1/fan1").unwrap(),
            uuid: "GPU-1".into(),
            fan: 1,
            original_policy: "manual".into(),
            original_speed: Some(55),
        };
        let f = HeldFile { version: VERSION, pid: 7, controls: vec![manual] };
        assert_eq!(HeldFile::parse(&f.to_json()).unwrap(), f);
        let old = r#"{"version":1,"pid":1,"controls":[{"kind":"nvml","key":"nvml/GPU-1/fan0","uuid":"GPU-1","fan":0}]}"#;
        match &HeldFile::parse(old).unwrap().controls[0] {
            HeldEntry::Nvml { original_policy, original_speed, .. } => {
                assert_eq!(original_policy, "driver");
                assert_eq!(*original_speed, None);
            }
            other => panic!("{other:?}"),
        }
    }
}
