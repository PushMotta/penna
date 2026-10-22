//! What Penna knows about each fan driver (DESIGN.md §3.5, §3.6): the kind of control it offers, what a write needs, how far that
//! knowledge has been checked, and where it came from. Data, not code paths: a driver missing from here gets the generic hwmon ABI.
//!
//! Checked against the kernel source and documentation as of Linux 7.3, and the out-of-tree drivers' READMEs, on 21 Oct 2026.
use serde::{Deserialize, Serialize};

/// How far Penna's knowledge of a driver has been checked (§3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// From the driver's documentation only.
    Documented,
    /// A real machine's capture with this driver runs in the simulator.
    Captured,
    /// The exit-path hardware test (§9.4) passed on real hardware with this driver.
    Verified,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Documented => "documented",
            Tier::Captured => "captured",
            Tier::Verified => "verified",
        }
    }
}

/// What a driver's `pwmN` files accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offers {
    /// `pwmN` from 0 to `scale`, manual when `pwmN_enable` is 1.
    Duty {
        scale: u32,
    },
    Levels {
        steps: u32,
    },
    FirmwareCurve {
        points: u32,
    },
    Modes,
    /// Nothing Penna writes.
    ReadOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Motherboard,
    Laptop,
    Gpu,
    Other,
}

#[derive(Debug)]
pub struct Driver {
    /// What people call it: the kernel module, or the out-of-tree project.
    pub name: &'static str,
    /// The hwmon `name` values it registers. A trailing `*` matches a prefix: it87 names itself after the chip (`it8688`).
    hwmon: &'static [&'static str],
    pub class: Class,
    /// What the hardware is, for a person: "Motherboard", "ThinkPad".
    pub label: &'static str,
    pub offers: Offers,
    pub tier: Tier,
    /// What a write needs that a default install may not have.
    pub needs: Option<&'static str>,
    /// Where this entry was written from.
    pub source: &'static str,
}

const LINUX: &str = "Linux 7.3 source and documentation";

pub const DRIVERS: &[Driver] = &[
    Driver {
        name: "it87",
        hwmon: &["it87", "it8*"],
        class: Class::Motherboard,
        label: "Motherboard",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Verified,
        needs: None,
        source: "drivers/hwmon/it87.c, Documentation/hwmon/it87.rst; github.com/frankcrawford/it87 at bd7a566; the §9.4 test on \
                 this desk's IT8688E and IT8792E with that fork, 22 Oct 2026",
    },
    Driver {
        name: "nct6775",
        hwmon: &["nct61*", "nct67*"],
        class: Class::Motherboard,
        label: "Motherboard",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/hwmon/nct6775-core.c, Documentation/hwmon/nct6775.rst",
    },
    // nct6683 reads only (no pwm_enable); the out-of-tree nct6687d registers the same name with writable controls, so the
    // channel decides (§3.5).
    Driver {
        name: "nct6683",
        hwmon: &["nct6683", "nct6686", "nct6687"],
        class: Class::Motherboard,
        label: "Motherboard",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/hwmon/nct6683.c; github.com/Fred78290/nct6687d",
    },
    Driver {
        name: "f71882fg",
        hwmon: &["f718*", "f8000", "f8186*"],
        class: Class::Motherboard,
        label: "Motherboard",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/hwmon/f71882fg.c, Documentation/hwmon/f71882fg.rst",
    },
    Driver {
        name: "w83627ehf",
        hwmon: &["w83*"],
        class: Class::Motherboard,
        label: "Motherboard",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "Documentation/hwmon/w83627ehf.rst",
    },
    Driver {
        name: "amdgpu",
        hwmon: &["amdgpu"],
        class: Class::Gpu,
        label: "AMD GPU",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/gpu/drm/amd/pm/amdgpu_pm.c, Documentation/gpu/amdgpu/thermal.rst",
    },
    Driver {
        name: "nouveau",
        hwmon: &["nouveau"],
        class: Class::Gpu,
        label: "NVIDIA GPU (nouveau)",
        offers: Offers::Duty { scale: 100 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/gpu/drm/nouveau/nouveau_hwmon.c",
    },
    Driver {
        name: "gpd-fan",
        hwmon: &["gpdfan"],
        class: Class::Laptop,
        label: "GPD handheld",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "Documentation/hwmon/gpd-fan.rst",
    },
    Driver {
        name: "cros_ec_hwmon",
        hwmon: &["cros_ec"],
        class: Class::Laptop,
        label: "Embedded controller",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "Documentation/hwmon/cros_ec_hwmon.rst",
    },
    Driver {
        name: "acer-wmi",
        hwmon: &["acer"],
        class: Class::Laptop,
        label: "Acer laptop",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/platform/x86/acer-wmi.c",
    },
    Driver {
        name: "thinkpad_acpi",
        hwmon: &["thinkpad"],
        class: Class::Laptop,
        label: "ThinkPad",
        offers: Offers::Levels { steps: 8 },
        tier: Tier::Documented,
        needs: Some("thinkpad_acpi fan_control=1"),
        source: "Documentation/admin-guide/laptops/thinkpad-acpi.rst",
    },
    Driver {
        name: "dell-smm-hwmon",
        hwmon: &["dell_smm"],
        class: Class::Laptop,
        label: "Dell laptop",
        offers: Offers::Levels { steps: 3 },
        tier: Tier::Documented,
        needs: None,
        source: "Documentation/hwmon/dell-smm-hwmon.rst",
    },
    Driver {
        name: "asus-wmi",
        hwmon: &["asus"],
        class: Class::Laptop,
        label: "ASUS laptop",
        offers: Offers::Modes,
        tier: Tier::Documented,
        needs: None,
        source: "drivers/platform/x86/asus-wmi.c",
    },
    Driver {
        name: "asus-wmi custom fan curves",
        hwmon: &["asus_custom_fan_curve"],
        class: Class::Laptop,
        label: "ASUS fan curves",
        offers: Offers::FirmwareCurve { points: 8 },
        tier: Tier::Documented,
        needs: None,
        source: "drivers/platform/x86/asus-wmi.c",
    },
    Driver {
        name: "hp-wmi",
        hwmon: &["hp"],
        class: Class::Laptop,
        label: "HP laptop",
        offers: Offers::Modes,
        tier: Tier::Documented,
        needs: None,
        source: "drivers/platform/x86/hp/hp-wmi.c",
    },
    Driver {
        name: "alienware-wmi",
        hwmon: &["alienware_wmi"],
        class: Class::Laptop,
        label: "Alienware",
        offers: Offers::Modes,
        tier: Tier::Documented,
        needs: None,
        source: "Documentation/admin-guide/laptops/alienware-wmi.rst",
    },
    // Penna never writes its curve files: one written in the wrong unit hard-powered-off a Legion Pro 5 (§3.5).
    Driver {
        name: "legion-laptop",
        hwmon: &["legion_hwmon"],
        class: Class::Laptop,
        label: "Lenovo Legion",
        offers: Offers::ReadOnly,
        tier: Tier::Documented,
        needs: None,
        source: "github.com/johnfanv2/LenovoLegionLinux README",
    },
    Driver {
        name: "read-only",
        hwmon: &["system76_acpi", "surface_fan", "i915", "xe", "msi_wmi_platform"],
        class: Class::Other,
        label: "",
        offers: Offers::ReadOnly,
        tier: Tier::Documented,
        needs: None,
        source: LINUX,
    },
];

/// The entry for an hwmon `name`, if Penna knows the driver.
pub fn lookup(hwmon_name: &str) -> Option<&'static Driver> {
    DRIVERS.iter().find(|d| {
        d.hwmon.iter().any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => hwmon_name.starts_with(prefix),
            None => hwmon_name == *pattern,
        })
    })
}

/// How far a device's driver has been checked. NVIDIA's NVML was verified on the 3090 in M4 (§9.4), and it87 on this desk's
/// IT8688E and IT8792E in M5. A driver Penna doesn't know is at best Documented: the generic hwmon ABI is all it has to go on.
pub fn tier(driver: &str) -> Tier {
    if driver == "nvml" {
        return Tier::Verified;
    }
    lookup(driver).map_or(Tier::Documented, |d| d.tier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_prefixes() {
        assert_eq!(lookup("it87").unwrap().name, "it87");
        assert_eq!(lookup("it8688").unwrap().name, "it87", "it87 names itself after the chip");
        assert_eq!(lookup("it8792").unwrap().name, "it87");
        assert_eq!(lookup("nct6798").unwrap().name, "nct6775");
        assert_eq!(lookup("nct6687").unwrap().name, "nct6683");
        assert_eq!(lookup("thinkpad").unwrap().offers, Offers::Levels { steps: 8 });
        assert_eq!(lookup("nouveau").unwrap().offers, Offers::Duty { scale: 100 });
        assert_eq!(lookup("legion_hwmon").unwrap().offers, Offers::ReadOnly);
        assert!(lookup("k10temp").is_none());
        assert!(lookup("asus_custom_fan_curve").is_some_and(|d| d.offers == Offers::FirmwareCurve { points: 8 }));
        assert_eq!(lookup("asus").unwrap().name, "asus-wmi", "asus is not a prefix of asus_custom_fan_curve's entry");
    }

    #[test]
    fn tiers() {
        assert_eq!(tier("nvml"), Tier::Verified);
        assert_eq!(tier("it8688_4008090b"), Tier::Verified, "the fork's name, with Gigabyte's SIV id");
        assert_eq!(tier("thinkpad"), Tier::Documented);
        assert_eq!(tier("mystery"), Tier::Documented);
    }
}
