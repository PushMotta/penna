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
    /// The exact `pwmN_enable` value found, never an assumed "automatic" (§6.1), and the duty it was running.
    Hwmon { key: Key, enable: PathBuf, original_enable: i64, pwm: PathBuf, original_pwm: u32 },
    /// The simulator's file-backed NVML (§9.1).
    MockNvml { key: Key, policy: PathBuf, original_policy: String },
    /// A real NVIDIA fan: restored with `nvmlDeviceSetDefaultFanSpeed_v2` (M4).
    Nvml { key: Key, uuid: String, fan: u32 },
}

impl HeldEntry {
    pub fn key(&self) -> &Key {
        match self {
            HeldEntry::Hwmon { key, .. } | HeldEntry::MockNvml { key, .. } | HeldEntry::Nvml { key, .. } => key,
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
                    original_pwm: 102,
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
    }
}
