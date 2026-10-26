//! Stable keys (DESIGN.md §3.2). The label a person sees is stored beside the key, never in it.
use serde::{Deserialize, Serialize};
use std::fmt;

/// `hwmon/<name>/<device>/<channel>`, `nvml/<uuid>/<fanN|temp>`, `virt/<id>`, `mock/<scenario>/<channel>`, or `platform/profile`, the
/// machine's power profile (§3.5).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Key(String);

/// The longest key: a PCI path, a driver's name and a channel come to well under this.
pub const KEY_MAX: usize = 256;

impl Key {
    pub fn hwmon(name: &str, device: &str, channel: &str) -> Key {
        Key(format!("hwmon/{}/{}/{}", part(name), part(device), part(channel)))
    }
    pub fn nvml_fan(uuid: &str, fan: u32) -> Key {
        Key(format!("nvml/{}/fan{fan}", part(uuid)))
    }
    pub fn nvml_temp(uuid: &str) -> Key {
        Key(format!("nvml/{}/temp", part(uuid)))
    }
    pub fn platform_profile() -> Key {
        Key("platform/profile".to_owned())
    }

    /// Checks the shape of a key read from a config file or the wire. Keys are printed to every user, so a key holds no control
    /// character, as no sysfs or NVML name does, and is at most [`KEY_MAX`] bytes. Nor a `.` or `..` part: no key becomes a
    /// path, and none could climb out of one if a later change made it one.
    pub fn parse(s: &str) -> Option<Key> {
        if s.len() > KEY_MAX || s.chars().any(crate::config::hidden) {
            return None;
        }
        let parts: Vec<&str> = s.split('/').collect();
        if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
            return None;
        }
        let ok = match parts[0] {
            "hwmon" => parts.len() == 4,
            "nvml" => parts.len() == 3 && (parts[2] == "temp" || fan_index(parts[2]).is_some()),
            "virt" => parts.len() == 2,
            "mock" => parts.len() == 3,
            "platform" => parts == ["platform", "profile"],
            _ => false,
        };
        ok.then(|| Key(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Everything before the channel: the id of the device the key belongs to.
    pub fn device(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(d, _)| d)
    }
    pub fn channel(&self) -> &str {
        self.0.rsplit_once('/').map_or("", |(_, c)| c)
    }
}

fn fan_index(channel: &str) -> Option<u32> {
    channel.strip_prefix("fan")?.parse().ok()
}

/// A path component: sysfs and NVML never put `/` in these, but a key must stay splittable if one ever does.
fn part(s: &str) -> String {
    s.trim().replace(['/', ' '], "_")
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Key {
    type Error = String;
    fn try_from(s: String) -> Result<Key, String> {
        Key::parse(&s).ok_or_else(|| format!("not a penna key: {s:?}"))
    }
}

impl From<Key> for String {
    fn from(k: Key) -> String {
        k.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwmon_keys_name_the_device_not_the_index() {
        let k = Key::hwmon("k10temp", "0000:00:18.3", "temp1");
        assert_eq!(k.as_str(), "hwmon/k10temp/0000:00:18.3/temp1");
        assert_eq!(k.device(), "hwmon/k10temp/0000:00:18.3");
        assert_eq!(k.channel(), "temp1");
    }

    #[test]
    fn nvml_keys() {
        let uuid = "GPU-168e2769-d673-f36a-ea7b-64df57f8d090";
        assert_eq!(Key::nvml_fan(uuid, 0).as_str(), "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0");
        assert_eq!(Key::nvml_temp(uuid).channel(), "temp");
    }

    #[test]
    fn parse_accepts_each_source_and_rejects_the_rest() {
        for ok in [
            "hwmon/nvme/0000:04:00.0/temp1",
            "nvml/GPU-1/fan1",
            "nvml/GPU-1/temp",
            "virt/case",
            "mock/render-burst/pwm2",
            "platform/profile",
        ] {
            assert_eq!(Key::parse(ok).map(|k| k.0), Some(ok.to_owned()), "{ok}");
        }
        for bad in [
            "hwmon3/temp1",
            "hwmon/k10temp/temp1",
            "nvml/GPU-1/pwm0",
            "virt/",
            "virt//x",
            "sysfs/a/b/c",
            "platform/fan",
            "",
            "hwmon/../etc/x",
            "virt/..",
            "mock/./pwm2",
        ] {
            assert!(Key::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn components_never_split_a_key() {
        assert_eq!(Key::hwmon("odd/name", "dev ice", "temp1").as_str(), "hwmon/odd_name/dev_ice/temp1");
    }

    #[test]
    fn string_round_trip_validates() {
        let k = Key::hwmon("k10temp", "0000:00:18.3", "temp3");
        assert_eq!(Key::try_from(String::from(k.clone())), Ok(k));
        assert!(Key::try_from("nonsense".to_owned()).is_err());
    }
}
