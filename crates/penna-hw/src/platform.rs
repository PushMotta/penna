//! The platform profile (DESIGN.md §3.4, §3.5, §4.7): a machine-wide power profile, the only fan lever some laptops have. Through
//! power-profiles-daemon when it runs, since it, and tuned-ppd, treat a direct write to `/sys/firmware/acpi/platform_profile` as
//! the user's own choice and would fight it; through that file when neither runs.
use anyhow::{Context, Result, anyhow, bail};
use penna_core::Key;
use penna_core::held::{HeldEntry, PlatformVia};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// power-profiles-daemon's bus name, object and interface since 0.20; tuned-ppd answers to the same.
pub const BUS_NAME: &str = "org.freedesktop.UPower.PowerProfiles";
pub const OBJECT: &str = "/org/freedesktop/UPower/PowerProfiles";
pub const INTERFACE: &str = "org.freedesktop.UPower.PowerProfiles";

pub struct Platform {
    via: PlatformVia,
    choices: Vec<String>,
}

impl Platform {
    /// What this machine has: the daemon's profiles when it owns its bus name (`bus`), else the firmware's own choices under the
    /// sysfs root. `None` when the firmware has no platform profile: power-profiles-daemon then changes only the CPU's power (a
    /// mini PC's `intel_pstate`), which is no fan lever (§3.5).
    pub fn open(sysfs_root: &Path, bus: bool) -> Option<Platform> {
        let dir = sysfs_root.join("firmware/acpi");
        if !dir.join("platform_profile").exists() {
            return None;
        }
        if bus && let Some(choices) = daemon_profiles() {
            return Some(Platform { via: PlatformVia::Daemon, choices });
        }
        let choices: Vec<String> =
            fs::read_to_string(dir.join("platform_profile_choices")).ok()?.split_whitespace().map(str::to_owned).collect();
        let path = dir.join("platform_profile");
        (!choices.is_empty() && path.exists()).then_some(Platform { via: PlatformVia::Sysfs { path }, choices })
    }

    pub fn choices(&self) -> &[String] {
        &self.choices
    }

    /// Whether power-profiles-daemon sets it, rather than the file.
    pub fn daemon(&self) -> bool {
        self.via == PlatformVia::Daemon
    }

    pub fn current(&self) -> Result<String> {
        current(&self.via)
    }

    pub fn set(&self, name: &str) -> Result<()> {
        if !self.choices.iter().any(|c| c == name) {
            bail!("{name:?} isn't one of this machine's power profiles ({})", self.choices.join(", "));
        }
        set(&self.via, name)
    }

    /// The way back, before the first change: the profile that was set.
    pub fn original(&self) -> Result<HeldEntry> {
        Ok(HeldEntry::Platform { key: Key::platform_profile(), via: self.via.clone(), original: self.current()? })
    }
}

/// The power profile a Penna profile asks for (§4.7): Quiet, Balanced and Render are `power-saver`, `balanced` and `performance`
/// through the daemon. Written directly, Quiet takes the firmware's `quiet` or `low-power`, and Render its `performance` or
/// `balanced-performance`. `None` for Defaults, a profile of the user's own, or one the machine doesn't offer.
pub fn for_profile(profile: &str, choices: &[String], daemon: bool) -> Option<String> {
    let wanted: &[&str] = match (profile, daemon) {
        ("quiet", true) => &["power-saver"],
        ("balanced", _) => &["balanced"],
        ("render", true) => &["performance"],
        ("quiet", false) => &["quiet", "low-power", "cool"],
        ("render", false) => &["performance", "balanced-performance"],
        _ => &[],
    };
    wanted.iter().find(|w| choices.iter().any(|c| c == *w)).map(|w| (*w).to_owned())
}

fn current(via: &PlatformVia) -> Result<String> {
    match via {
        PlatformVia::Daemon => {
            let out = busctl(&["get-property", BUS_NAME, OBJECT, INTERFACE, "ActiveProfile"])?;
            // `s "balanced"`
            out.trim().strip_prefix("s ").map(|v| v.trim_matches('"').to_owned()).ok_or_else(|| anyhow!("busctl said {out:?}"))
        }
        PlatformVia::Sysfs { path } => {
            Ok(fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?.trim().to_owned())
        }
    }
}

fn set(via: &PlatformVia, name: &str) -> Result<()> {
    match via {
        PlatformVia::Daemon => busctl(&["set-property", BUS_NAME, OBJECT, INTERFACE, "ActiveProfile", "s", name]).map(|_| ()),
        PlatformVia::Sysfs { path } => {
            fs::write(path, format!("{name}\n")).with_context(|| format!("writing {name} to {}", path.display()))
        }
    }
}

/// Puts the power profile back as it was found.
pub fn restore(via: &PlatformVia, original: &str) -> Result<String> {
    set(via, original)?;
    Ok(format!("{}: {original}", Key::platform_profile()))
}

/// Whether a held platform profile has nothing to go back to: its file is gone, or the daemon no longer runs.
pub fn gone(via: &PlatformVia) -> bool {
    match via {
        PlatformVia::Daemon => daemon_profiles().is_none(),
        PlatformVia::Sysfs { path } => !path.exists(),
    }
}

/// The daemon's profiles, when it owns its bus name.
fn daemon_profiles() -> Option<Vec<String>> {
    let out = busctl(&["get-property", BUS_NAME, OBJECT, INTERFACE, "Profiles"]).ok()?;
    // `aa{sv} 3 4 "Profile" s "power-saver" "CpuDriver" s "amd_pstate" …`: each profile's name follows `"Profile" s`.
    let names: Vec<String> = out.split("\"Profile\" s ").skip(1).filter_map(|rest| rest.split('"').nth(1)).map(str::to_owned).collect();
    (!names.is_empty()).then_some(names)
}

fn busctl(args: &[&str]) -> Result<String> {
    let out = Command::new("busctl").arg("--system").args(args).output().context("running busctl")?;
    if !out.status.success() {
        bail!("busctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Where the platform profile file is under a sysfs root.
pub fn sysfs_path(sysfs_root: &Path) -> PathBuf {
    sysfs_root.join("firmware/acpi/platform_profile")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn each_profile_asks_for_its_power_profile() {
        let daemon = names(&["power-saver", "balanced", "performance"]);
        assert_eq!(for_profile("quiet", &daemon, true).as_deref(), Some("power-saver"));
        assert_eq!(for_profile("render", &daemon, true).as_deref(), Some("performance"));
        assert_eq!(for_profile("defaults", &daemon, true), None);
        let thinkpad = names(&["low-power", "balanced", "performance"]);
        assert_eq!(for_profile("quiet", &thinkpad, false).as_deref(), Some("low-power"));
        let asus = names(&["quiet", "balanced", "performance"]);
        assert_eq!(for_profile("quiet", &asus, false).as_deref(), Some("quiet"));
        let two = names(&["balanced", "balanced-performance"]);
        assert_eq!(for_profile("render", &two, false).as_deref(), Some("balanced-performance"));
        assert_eq!(for_profile("quiet", &two, false), None, "nothing quieter to ask for");
    }

    #[test]
    fn the_file_is_read_set_and_put_back() {
        let root = std::env::temp_dir().join(format!("penna-platform-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("firmware/acpi")).unwrap();
        fs::write(root.join("firmware/acpi/platform_profile_choices"), "low-power balanced performance\n").unwrap();
        fs::write(root.join("firmware/acpi/platform_profile"), "balanced\n").unwrap();
        let p = Platform::open(&root, false).unwrap();
        assert!(!p.daemon());
        let entry = p.original().unwrap();
        p.set("low-power").unwrap();
        assert_eq!(p.current().unwrap(), "low-power");
        assert!(p.set("turbo").is_err(), "only the firmware's own choices");
        let HeldEntry::Platform { via, original, .. } = &entry else { panic!("{entry:?}") };
        assert_eq!(restore(via, original).unwrap(), "platform/profile: balanced");
        assert_eq!(p.current().unwrap(), "balanced");
        assert!(Platform::open(&root.join("nowhere"), false).is_none());
    }

    #[test]
    fn the_daemon_s_profiles_are_read_from_busctl_s_answer() {
        let out = r#"aa{sv} 3 4 "Profile" s "power-saver" "CpuDriver" s "amd_pstate" "PlatformDriver" s "placeholder" "Driver" s "multiple" 4 "Profile" s "balanced" "CpuDriver" s "amd_pstate" 3 "Profile" s "performance" "CpuDriver" s "amd_pstate""#;
        let names: Vec<&str> = out.split("\"Profile\" s ").skip(1).filter_map(|rest| rest.split('"').nth(1)).collect();
        assert_eq!(names, ["power-saver", "balanced", "performance"]);
    }
}
