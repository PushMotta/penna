//! What Penna knows about each fan driver (DESIGN.md §3.5, §3.6): the kind of control it offers, what a write needs, how far that
//! knowledge has been checked, and where it came from. Data, not code paths: a driver missing from here gets the generic hwmon ABI.
//!
//! Checked against the kernel source and documentation as of Linux 7.3, and the out-of-tree drivers' READMEs, on 21 Oct 2026; the
//! laptop and GPU drivers again on 23 Oct 2026, against master at 734bef548a96 (7.3, byte for byte in these files), for M9.
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
    /// A target RPM: `fanN_target` (`lenovo_wmi_other`; applesmc from 7.3, with `pwmN_enable`) or `fanN_output` (applesmc up to
    /// 7.2, with `fanN_manual`), between `fanN_min` and `fanN_max`.
    Speed,
    FirmwareCurve {
        points: u32,
    },
    /// `pwmN_enable` 0 (full speed) or 2 (automatic). `duty`: a channel with a `pwmN` of its own takes a duty instead
    /// (asus-wmi's AGFN CPU fan).
    Modes {
        duty: bool,
    },
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

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Motherboard => "motherboard",
            Class::Laptop => "laptop",
            Class::Gpu => "gpu",
            Class::Other => "other",
        }
    }
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
    /// The `pwmN_enable` to put back when the driver can't say which mode it was in (§6.1): `dell_smm`'s global enable is
    /// write-only.
    pub restore: Option<i64>,
    /// A firmware watchdog that takes the fan back when nothing writes it for a while: Penna arms it and writes every tick, so a
    /// Penna that stops for any reason hands the fan back with nothing else running (§3.5).
    pub watchdog: Option<Watchdog>,
    /// A module parameter, under the sysfs root, and the value writes need: without it the driver refuses them (`EPERM`) while
    /// its files still look writable, so the channel is read-only to Penna and the doctor names `needs`.
    pub allowed: Option<Param>,
    /// A firmware fan curve the device offers beside `pwmN`, as a path from the hwmon directory: amdgpu's OverDrive curve, which
    /// Penna drives when `pwm1` isn't writable (RDNA4).
    pub curve: Option<&'static str>,
    /// What a firmware curve on `pwmN` cools, by channel: `cpu` or `gpu`. Its generated curve follows that source (§5.5).
    pub curve_on: &'static [&'static str],
    /// Where this entry was written from.
    pub source: &'static str,
}

/// A module parameter's file under the sysfs root, and the value that allows writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub path: &'static str,
    pub value: &'static str,
}

/// A watchdog file, under the sysfs root, and the seconds Penna arms it with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watchdog {
    pub path: &'static str,
    pub seconds: u32,
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "Documentation/hwmon/w83627ehf.rst",
    },
    Driver {
        name: "amdgpu",
        hwmon: &["amdgpu"],
        class: Class::Gpu,
        label: "AMD GPU",
        offers: Offers::Duty { scale: 255 },
        tier: Tier::Documented,
        // OverDrive needs PP_OVERDRIVE_MASK (0x4000), which the default mask 0xfff7bfff leaves clear.
        needs: Some("amdgpu ppfeaturemask=0xfff7ffff"),
        restore: None,
        watchdog: None,
        allowed: None,
        // Up to RDNA3 pwm1 takes a duty; on RDNA4 it is read-only, and OverDrive's 5-point curve is the lever.
        curve: Some("device/gpu_od/fan_ctrl/fan_curve"),
        curve_on: &["gpu"],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        // fan_watchdog_store: 0–120 s; fan_watchdog_fire hands the fan to the embedded controller, at level 4 or above.
        watchdog: Some(Watchdog { path: "bus/platform/drivers/thinkpad_hwmon/fan_watchdog", seconds: 5 }),
        // module_param_named(fan_control, fan_control_allowed, bool, 0444): pwm1 stays 0644 without it, and writes get EPERM.
        allowed: Some(Param { path: "module/thinkpad_acpi/parameters/fan_control", value: "Y" }),
        curve: None,
        curve_on: &[],
        source: "drivers/platform/x86/lenovo/thinkpad_acpi.c (fan_pwm1_store, fan_pwm1_enable_store, fan_watchdog_store, \
                 tpacpi_hwmon_pdriver); Documentation/admin-guide/laptops/thinkpad-acpi.rst",
    },
    Driver {
        name: "dell-smm-hwmon",
        hwmon: &["dell_smm"],
        class: Class::Laptop,
        label: "Dell laptop",
        // fan_max + 1 states, 3 by default: pwm 0, 128, 255 (85 apart with 4). A machine with 4 runs on 0, 2 and 3 here.
        offers: Offers::Levels { steps: 3 },
        tier: Tier::Documented,
        needs: None,
        // A whitelisted model's one pwm1_enable is write-only (0200); automatic is what the BIOS runs at boot.
        restore: Some(2),
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "drivers/hwmon/dell-smm-hwmon.c (dell_smm_read, dell_smm_write, dell_smm_is_visible); \
                 Documentation/hwmon/dell-smm-hwmon.rst",
    },
    Driver {
        name: "asus-wmi",
        hwmon: &["asus"],
        class: Class::Laptop,
        label: "ASUS laptop",
        offers: Offers::Modes { duty: true },
        tier: Tier::Documented,
        needs: None,
        // pwmN_enable reads a cache: pwm2_enable and pwm3_enable read 0, full speed, until written, though probe left them
        // automatic. Automatic is what goes back.
        restore: Some(2),
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "drivers/platform/x86/asus-wmi.c (pwm1_enable_store, pwm2_enable_show, asus_hwmon_sysfs_is_visible)",
    },
    Driver {
        name: "asus-wmi custom fan curves",
        hwmon: &["asus_custom_fan_curve"],
        class: Class::Laptop,
        label: "ASUS fan curves",
        offers: Offers::FirmwareCurve { points: 8 },
        tier: Tier::Documented,
        needs: None,
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        // pwm1 the CPU fan (0x00110024), pwm2 the GPU fan (0x00110025), pwm3 the middle fan (0x00110032).
        curve_on: &["cpu", "gpu", "cpu"],
        source: "drivers/platform/x86/asus-wmi.c",
    },
    Driver {
        name: "hp-wmi",
        hwmon: &["hp"],
        class: Class::Laptop,
        label: "HP laptop",
        // Manual duty (since 7.0, Victus S and Omen) sends both fans in one call, and the GPU fan has no enable of its own: taking
        // the CPU fan would hold the GPU fan where it was. Penna keeps to the modes.
        offers: Offers::Modes { duty: false },
        tier: Tier::Documented,
        needs: None,
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "drivers/platform/x86/hp/hp-wmi.c",
    },
    Driver {
        name: "alienware-wmi",
        hwmon: &["alienware_wmi"],
        class: Class::Laptop,
        label: "Alienware",
        // fanN_boost adds to the firmware's own speed; there is no pwmN and no pwmN_enable.
        offers: Offers::ReadOnly,
        tier: Tier::Documented,
        needs: None,
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "drivers/platform/x86/dell/alienware-wmi-wmax.c; Documentation/admin-guide/laptops/alienware-wmi.rst",
    },
    Driver {
        name: "lenovo-wmi-other",
        hwmon: &["lenovo_wmi_other"],
        class: Class::Laptop,
        label: "Lenovo laptop",
        // fanN_target: 0 is automatic, otherwise within fanN_min..fanN_max, rounded down to 100 RPM; it reads back the cache.
        offers: Offers::Speed,
        tier: Tier::Documented,
        needs: None,
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "drivers/platform/x86/lenovo/wmi-other.c (lwmi_om_hwmon_write); Documentation/wmi/devices/lenovo-wmi-other.rst",
    },
    Driver {
        name: "applesmc",
        hwmon: &["applesmc"],
        class: Class::Laptop,
        label: "Mac",
        offers: Offers::Speed,
        tier: Tier::Documented,
        needs: None,
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
        source: "drivers/hwmon/applesmc.c (applesmc_hwmon_write from 7.3; applesmc_store_fan_manual up to 7.2)",
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        restore: None,
        watchdog: None,
        allowed: None,
        curve: None,
        curve_on: &[],
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
        assert_eq!(lookup("alienware_wmi").unwrap().offers, Offers::ReadOnly, "a boost is not a mode");
        assert_eq!(lookup("lenovo_wmi_other").unwrap().offers, Offers::Speed);
    }

    #[test]
    fn tiers() {
        assert_eq!(tier("nvml"), Tier::Verified);
        assert_eq!(tier("it8688_4008090b"), Tier::Verified, "the fork's name, with Gigabyte's SIV id");
        assert_eq!(tier("thinkpad"), Tier::Documented);
        assert_eq!(tier("mystery"), Tier::Documented);
    }
}
