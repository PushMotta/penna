//! The board database (DESIGN.md §3.6): what is known about particular motherboards, as data with sources. One TOML file per
//! board, matched by the DMI vendor and board name. It names headers and sensors, says which driver and options a board needs
//! and what that risks, and drops inputs the board leaves unconnected. Nothing in it is written from memory.
use crate::{Device, Inventory, Source};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    /// DMI `board_vendor`, as the firmware reports it.
    pub vendor: String,
    /// DMI `board_name`.
    pub board: String,
    /// Where each fact came from.
    pub sources: Vec<String>,
    #[serde(default)]
    pub driver: Option<DriverAdvice>,
    #[serde(default, rename = "chip")]
    pub chips: Vec<Chip>,
}

/// The driver a board needs, and the cost of loading it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverAdvice {
    pub module: String,
    /// The package that provides it, when the kernel's own is not enough.
    #[serde(default)]
    pub package: Option<String>,
    /// Module options, as `modprobe` takes them.
    #[serde(default)]
    pub options: Option<String>,
    pub why: String,
    #[serde(default)]
    pub risk: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chip {
    /// The hwmon `name` it registers. The it87 fork appends Gigabyte's SIV id to it on Gigabyte boards (`it8688_4008090b`,
    /// since c8d2038, Sep 2026); an entry names the chip without it and matches either.
    pub hwmon: String,
    /// What the chip is called: "IT8688E".
    #[serde(default)]
    pub chip: Option<String>,
    /// Tach channel → header name, as printed on the board.
    #[serde(default)]
    pub fans: BTreeMap<String, String>,
    /// Tach channel → what the header is for: `cpu`, `pump-header`, `chipset`. Setup suggests roles from it (§5.3).
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    /// Temperature channel → what it measures.
    #[serde(default)]
    pub temps: BTreeMap<String, String>,
    /// Inputs with nothing behind them, such as a thermistor header left empty: `temp2`.
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub quirks: Vec<String>,
}

impl Board {
    pub fn parse(text: &str) -> Result<Board, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn matches(&self, vendor: &str, board: &str) -> bool {
        self.vendor.trim() == vendor.trim() && self.board.trim() == board.trim()
    }

    pub fn chip(&self, hwmon: &str) -> Option<&Chip> {
        self.chips.iter().find(|c| c.names(hwmon))
    }

    /// Names, drops and describes what the board says about each device (§3.6). A person's own labels, kept in the
    /// configuration, still win over these.
    pub fn apply(&self, inventory: &mut Inventory) {
        for d in inventory.devices.iter_mut().filter(|d| d.source == Source::Hwmon) {
            if let Some(chip) = self.chip(&d.driver) {
                chip.apply(d);
            }
        }
        // A device left with nothing once its unconnected inputs are gone, such as this desk's acpitz, is not shown at all.
        inventory.devices.retain(|d| {
            self.chip(&d.driver).is_none_or(|c| c.ignore.is_empty())
                || !(d.sensors.is_empty() && d.tachs.is_empty() && d.controls.is_empty())
        });
    }
}

impl Chip {
    /// This chip's hwmon name, with or without the fork's `_` and eight hex digits of SIV id.
    fn names(&self, hwmon: &str) -> bool {
        hwmon == self.hwmon
            || hwmon
                .strip_prefix(self.hwmon.as_str())
                .and_then(|rest| rest.strip_prefix('_'))
                .is_some_and(|siv| siv.len() == 8 && siv.chars().all(|c| c.is_ascii_hexdigit()))
    }

    fn apply(&self, d: &mut Device) {
        if let Some(name) = &self.chip {
            d.model = Some(name.clone());
        }
        d.sensors.retain(|s| !self.ignore.iter().any(|i| i == s.key.channel()));
        d.tachs.retain(|t| !self.ignore.iter().any(|i| i == t.key.channel()));
        for s in &mut d.sensors {
            if let Some(name) = s.key.channel().strip_prefix("temp").and_then(|n| self.temps.get(n)) {
                s.label = name.clone();
            }
        }
        for t in &mut d.tachs {
            if let Some(name) = t.key.channel().strip_prefix("fan").and_then(|n| self.fans.get(n)) {
                t.label = name.clone();
            }
        }
    }
}

/// The entry for this machine, if the database has one.
pub fn find<'a>(boards: &'a [Board], vendor: &str, board: &str) -> Option<&'a Board> {
    boards.iter().find(|b| b.matches(vendor, board))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, SensorInfo, TachInfo};

    const DESK: &str = include_str!("../../../data/boards/gigabyte-x570-aorus-pro.toml");

    #[test]
    fn this_desk_parses_and_names_its_headers() {
        let b = Board::parse(DESK).unwrap();
        assert!(b.matches("Gigabyte Technology Co., Ltd.", "X570 AORUS PRO"));
        assert!(!b.matches("Gigabyte Technology Co., Ltd.", "X570 AORUS ELITE"));
        assert_eq!(b.driver.as_ref().unwrap().options.as_deref(), Some("ignore_resource_conflict=1"));
        let main = b.chip("it8688").unwrap();
        assert_eq!(main.fans["1"], "CPU_FAN");
        assert_eq!(main.fans["4"], "PCH_FAN");
        assert_eq!(b.chip("it8792").unwrap().fans["3"], "SYS_FAN4");
        assert_eq!(b.chip("it8688_4008090b"), Some(main), "the fork's name on this desk, with Gigabyte's SIV id");
        assert!(b.chip("it8688_40080").is_none() && b.chip("it86880").is_none());
        assert!(b.sources.iter().any(|s| s.contains("frankcrawford/it87")));
    }

    #[test]
    fn applying_names_tachs_and_sensors_and_drops_empty_inputs() {
        let b = Board::parse(DESK).unwrap();
        let k = |c: &str| Key::hwmon("it8688", "it87.2624", c);
        let sensor = |c: &str| SensorInfo { key: k(c), label: c.into(), limits: vec![], ignored_limits: vec![], below_ambient: false };
        let mut inv = Inventory {
            devices: vec![Device {
                id: "hwmon/it8688/it87.2624".into(),
                source: Source::Hwmon,
                driver: "it8688".into(),
                label: "Motherboard".into(),
                location: "it87.2624".into(),
                model: None,
                sensors: vec![sensor("temp1"), sensor("temp2"), sensor("temp5")],
                tachs: vec![TachInfo { key: k("fan1"), label: "fan1".into() }],
                controls: vec![],
            }],
            ..Inventory::default()
        };
        b.apply(&mut inv);
        let d = &inv.devices[0];
        assert_eq!(d.model.as_deref(), Some("IT8688E"));
        assert_eq!(d.sensors.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["System 1", "VRM MOS"], "EC_TEMP1 has no thermistor");
        assert_eq!(d.tachs[0].label, "CPU_FAN");
    }
}
