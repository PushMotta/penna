//! `held.json` on disk (DESIGN.md §6.1, §6.2): written through a private temporary file, synced and renamed into place, so a
//! reader sees the old list or the new one and never half of either. [`restore_file`] is what `penna restore`, the panic hook and
//! the daemon's own shutdown all run.
use crate::hwmon::write_number;
use anyhow::{Context, Result, bail};
use penna_core::Key;
use penna_core::held::{HeldEntry, HeldFile, VERSION};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "held.json";
/// Beside `measured.toml`: the way back for what outlives a reboot (§6.7).
pub const FIRMWARE_FILE: &str = "firmware.json";

/// The daemon's live copy of `held.json`.
pub struct Held {
    path: PathBuf,
    file: HeldFile,
    /// `firmware.json`: entries whose change outlives a reboot are written there too, and leave it with `held.json` (§6.7).
    firmware: Option<PathBuf>,
}

impl Held {
    pub fn new(path: impl Into<PathBuf>) -> Held {
        Held::resume(path, Vec::new())
    }

    /// Starts from entries a previous daemon left for devices that were gone (§3.6): they wait here, on disk, until the
    /// device comes back and can be put back as it was found.
    pub fn resume(path: impl Into<PathBuf>, entries: Vec<HeldEntry>) -> Held {
        Held { path: path.into(), file: HeldFile { version: VERSION, pid: std::process::id(), controls: entries }, firmware: None }
    }

    /// Mirrors what outlives a reboot to `firmware.json`, so it is put back even after a reboot has emptied `/run` (§6.7).
    pub fn with_firmware(mut self, path: impl Into<PathBuf>) -> Held {
        self.firmware = Some(path.into());
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[HeldEntry] {
        &self.file.controls
    }

    pub fn get(&self, key: &Key) -> Option<&HeldEntry> {
        self.file.controls.iter().find(|e| e.key() == key)
    }

    /// Adds the way back for a control and puts it on disk before returning: the write-ahead of §6.1. What outlives a reboot
    /// goes to `firmware.json` first.
    pub fn add(&mut self, entry: HeldEntry) -> Result<()> {
        if let (Some(fw), true) = (&self.firmware, entry.outlives_reboot()) {
            let mut f = load(fw).unwrap_or_else(|_| HeldFile { version: VERSION, pid: std::process::id(), controls: Vec::new() });
            f.controls.retain(|e| e.key() != entry.key());
            f.controls.push(entry.clone());
            write_atomically(fw, f.to_json().as_bytes())?;
        }
        self.file.controls.retain(|e| e.key() != entry.key());
        self.file.controls.push(entry);
        self.save()
    }

    pub fn remove(&mut self, key: &Key) -> Result<()> {
        self.file.controls.retain(|e| e.key() != key);
        if let Some(fw) = &self.firmware {
            forget(fw, std::slice::from_ref(key))?;
        }
        if self.file.controls.is_empty() { remove_if_present(&self.path) } else { self.save() }
    }

    fn save(&self) -> Result<()> {
        write_atomically(&self.path, self.file.to_json().as_bytes())
    }
}

/// Temporary file beside the target, `fsync`, rename, then `fsync` the directory so the rename itself survives a power cut.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("held.json");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    // A new file, never one already there: one left by an earlier process with this pid goes first, and a link put in its place
    // since is refused rather than written through.
    let _ = fs::remove_file(&tmp);
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    File::open(dir).and_then(|d| d.sync_all()).with_context(|| format!("syncing {}", dir.display()))?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

pub fn load(path: &Path) -> Result<HeldFile> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    HeldFile::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Puts one control back exactly as it was found. A real NVIDIA fan opens NVML for the purpose (M4).
pub fn restore(entry: &HeldEntry) -> Result<String> {
    match entry {
        HeldEntry::Hwmon { key, enable, original_enable, pwm, original_pwm, also } => {
            // The duty it was found at, in manual mode, and only then the mode. On it87's newer chips the manual duty register
            // is also the automatic curve's start PWM (`pwmN_auto_start`): putting back only the mode left the firmware's curve
            // starting wherever Penna last held the fan (§6.1; the M5 hardware test, IT8688E). Elsewhere an automatic mode takes
            // over at once and the duty is a moment's value. Manual first, as in take: a channel someone else switched back to
            // automatic refuses the duty. A duty that couldn't be read when the fan was taken isn't written.
            let duty = original_pwm.map(|raw| {
                let _ = write_number(enable, 1);
                write_number(pwm, raw as i64).map(|()| raw)
            });
            write_number(enable, *original_enable)?;
            // Files beside the control go back once the mode has: a watchdog put back first could hand the fan to its firmware
            // while the duty above was still being written.
            let mut beside = Vec::new();
            for a in also {
                match fs::write(&a.path, format!("{}\n", a.value)) {
                    Ok(()) => beside.push(format!(", {} {}", file_name(&a.path), a.value)),
                    Err(e) => beside.push(format!(", but not {} {}: {e}", file_name(&a.path), a.value)),
                }
            }
            let beside = beside.concat();
            match duty {
                Some(Ok(raw)) => Ok(format!("{key}: pwm_enable {original_enable}, pwm {raw}{beside}")),
                // The mode is what hands the fan back; a duty the driver refused is reported, not retried on every start.
                Some(Err(e)) => {
                    Ok(format!("{key}: pwm_enable {original_enable}, but not pwm {}: {e:#}{beside}", original_pwm.unwrap_or(0)))
                }
                None => Ok(format!("{key}: pwm_enable {original_enable}{beside}")),
            }
        }
        HeldEntry::MockNvml { key, policy, original_policy } => {
            if policy.parent().is_some_and(|d| d.join("lost").exists()) {
                bail!("{key}: GPU is lost");
            }
            fs::write(policy, format!("{original_policy}\n")).with_context(|| format!("writing {}", policy.display()))?;
            Ok(format!("{key}: {original_policy} policy"))
        }
        HeldEntry::Nvml { .. } => crate::nvml::restore_entry(entry),
        HeldEntry::Platform { via, original, .. } => crate::platform::restore(via, original),
        HeldEntry::Target { key, target, original, switch, original_switch } => {
            write_number(target, *original)?;
            match (switch, original_switch) {
                (Some(path), Some(v)) => {
                    write_number(path, *v)?;
                    Ok(format!("{key}: target {original}, {} {v}", file_name(path)))
                }
                _ => Ok(format!("{key}: target {original}")),
            }
        }
        HeldEntry::OdCurve { key, path, original } => {
            // The points as found, when the driver takes them back. A boot table can hold points outside OverDrive's range, which
            // the driver refuses (EINVAL): `r` puts the boot table back instead.
            match crate::hwmon::od_restore(path, original) {
                Ok(true) => Ok(format!("{key}: its OverDrive curve as found")),
                Ok(false) => Ok(format!("{key}: its OverDrive curve reset to the boot table")),
                Err(e) => Err(e),
            }
        }
        HeldEntry::PointCurve { key, enable, original_enable, points } => {
            // The points first: writing one switches the curve off, and the enable then puts it back as it was.
            for p in points {
                fs::write(&p.path, format!("{}\n", p.value)).with_context(|| format!("writing {}", p.path.display()))?;
            }
            write_number(enable, *original_enable)?;
            Ok(format!("{key}: its curve as found, enable {original_enable}"))
        }
    }
}

#[derive(Debug, Default)]
pub struct Restored {
    pub done: Vec<String>,
    pub failed: Vec<String>,
    /// Entries whose device is gone, such as a driver unloaded while Penna held its fans: there is nothing to write to.
    /// They stay in the file, so the control is put back when its device returns (§3.6).
    pub gone: Vec<HeldEntry>,
}

/// Whether an entry's device has gone away, so there is nothing to restore it on, rather than a write that failed.
pub fn gone(entry: &HeldEntry) -> bool {
    match entry {
        HeldEntry::Hwmon { enable, .. } => !enable.exists(),
        HeldEntry::MockNvml { policy, .. } => !policy.exists(),
        HeldEntry::Nvml { .. } => false,
        HeldEntry::Platform { via, .. } => crate::platform::gone(via),
        HeldEntry::Target { target, .. } => !target.exists(),
        HeldEntry::OdCurve { path, .. } => !path.exists(),
        HeldEntry::PointCurve { enable, .. } => !enable.exists(),
    }
}

/// Takes entries out of a file of ways back, removing it when none are left.
fn forget(path: &Path, keys: &[Key]) -> Result<()> {
    let Ok(mut f) = load(path) else { return Ok(()) };
    let before = f.controls.len();
    f.controls.retain(|e| !keys.contains(e.key()));
    if f.controls.is_empty() {
        remove_if_present(path)
    } else if f.controls.len() < before {
        write_atomically(path, f.to_json().as_bytes())
    } else {
        Ok(())
    }
}

/// `held.json`, then `firmware.json` (§6.7): what went back from the first leaves the second too, and what is only in the second
/// is a change that outlived a reboot, put back now.
pub fn restore_both(held: &Path, firmware: &Path) -> Result<Restored> {
    let mut out = Restored::default();
    if held.exists() {
        let entries = load(held)?.controls;
        out = restore_file(held)?;
        // What restore_file kept is what didn't go back: gone, or failed.
        let kept = if held.exists() { load(held)?.controls } else { Vec::new() };
        let back: Vec<Key> = entries.iter().filter(|e| !kept.contains(e)).map(|e| e.key().clone()).collect();
        forget(firmware, &back)?;
    }
    if firmware.exists() {
        let r = restore_file(firmware)?;
        out.done.extend(r.done);
        out.failed.extend(r.failed);
        out.gone.extend(r.gone);
    }
    Ok(out)
}

/// Restores every entry. The file is removed when every entry went back; otherwise it keeps exactly the ones that didn't.
pub fn restore_file(path: &Path) -> Result<Restored> {
    let file = load(path)?;
    let mut out = Restored::default();
    let mut kept = Vec::new();
    for e in &file.controls {
        if gone(e) {
            out.gone.push(e.clone());
            kept.push(e.clone());
            continue;
        }
        match restore(e) {
            Ok(s) => out.done.push(s),
            Err(err) => {
                out.failed.push(format!("{err:#}"));
                kept.push(e.clone());
            }
        }
    }
    if kept.is_empty() {
        remove_if_present(path)?;
    } else if kept.len() < file.controls.len() {
        write_atomically(path, HeldFile { controls: kept, ..file }.to_json().as_bytes())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("penna-held-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_link_where_the_temporary_file_goes_is_not_written_through() {
        let d = dir("link");
        let victim = d.join("victim");
        fs::write(&victim, "mine\n").unwrap();
        let target = d.join("config.toml");
        let tmp = d.join(format!(".config.toml.{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        write_atomically(&target, b"penna\n").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "mine\n");
        assert_eq!(fs::read_to_string(&target).unwrap(), "penna\n");
        assert!(!fs::symlink_metadata(&target).unwrap().is_symlink());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn add_writes_before_returning_and_remove_deletes_when_empty() {
        let d = dir("add");
        let enable = d.join("pwm1_enable");
        fs::write(&enable, "5\n").unwrap();
        let mut held = Held::new(d.join(FILE));
        let key = Key::parse("hwmon/it87/it87.2608/pwm1").unwrap();
        held.add(HeldEntry::Hwmon {
            key: key.clone(),
            enable: enable.clone(),
            original_enable: 5,
            pwm: d.join("pwm1"),
            original_pwm: Some(77),
            also: vec![],
        })
        .unwrap();
        assert_eq!(load(&d.join(FILE)).unwrap().controls.len(), 1);
        assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
        held.remove(&key).unwrap();
        assert!(!d.join(FILE).exists());
    }

    #[test]
    fn restore_puts_back_the_exact_value() {
        let d = dir("restore");
        let (enable, pwm) = (d.join("pwm3_enable"), d.join("pwm3"));
        fs::write(&enable, "1\n").unwrap();
        fs::write(&pwm, "200\n").unwrap();
        let mut held = Held::new(d.join(FILE));
        held.add(HeldEntry::Hwmon {
            key: Key::parse("hwmon/it87/x/pwm3").unwrap(),
            enable: enable.clone(),
            original_enable: 5,
            pwm: pwm.clone(),
            original_pwm: Some(90),
            also: vec![],
        })
        .unwrap();
        let r = restore_file(&d.join(FILE)).unwrap();
        assert_eq!(r.failed, Vec::<String>::new());
        assert_eq!(fs::read_to_string(&enable).unwrap().trim(), "5");
        assert_eq!(fs::read_to_string(&pwm).unwrap().trim(), "90", "on it87 the duty is also the automatic curve's start");
        assert!(!d.join(FILE).exists());
    }

    #[test]
    fn an_unread_duty_is_left_alone_and_a_watchdog_goes_back_after_the_mode() {
        let d = dir("beside");
        let (enable, pwm, watchdog) = (d.join("pwm1_enable"), d.join("pwm1"), d.join("fan_watchdog"));
        fs::write(&enable, "1\n").unwrap();
        fs::write(&pwm, "146\n").unwrap();
        fs::write(&watchdog, "5\n").unwrap();
        let mut held = Held::new(d.join(FILE));
        held.add(HeldEntry::Hwmon {
            key: Key::parse("hwmon/thinkpad/thinkpad_hwmon/pwm1").unwrap(),
            enable: enable.clone(),
            original_enable: 2,
            pwm: pwm.clone(),
            original_pwm: None,
            also: vec![penna_core::held::Also { path: watchdog.clone(), value: "0".into() }],
        })
        .unwrap();
        let r = restore_file(&d.join(FILE)).unwrap();
        assert_eq!(r.failed, Vec::<String>::new());
        assert_eq!(fs::read_to_string(&enable).unwrap().trim(), "2");
        assert_eq!(fs::read_to_string(&pwm).unwrap().trim(), "146", "a duty that couldn't be read isn't made up");
        assert_eq!(fs::read_to_string(&watchdog).unwrap().trim(), "0");
        assert!(r.done[0].ends_with("pwm_enable 2, fan_watchdog 0"), "{:?}", r.done);
    }

    #[test]
    fn a_change_that_outlives_a_reboot_is_in_firmware_json_too_and_goes_back_after_one() {
        let d = dir("firmware");
        let profile = d.join("platform_profile");
        fs::write(&profile, "low-power\n").unwrap();
        let entry = HeldEntry::Platform {
            key: Key::platform_profile(),
            via: penna_core::held::PlatformVia::Sysfs { path: profile.clone() },
            original: "balanced".into(),
        };
        let (run, fw) = (d.join("run").join(FILE), d.join(FIRMWARE_FILE));
        let mut held = Held::new(&run).with_firmware(&fw);
        held.add(entry.clone()).unwrap();
        assert_eq!(load(&fw).unwrap().controls, std::slice::from_ref(&entry), "written ahead to firmware.json");
        // A reboot empties /run: only firmware.json is left, and it puts the profile back.
        fs::remove_file(&run).unwrap();
        let r = restore_both(&run, &fw).unwrap();
        assert_eq!((r.done.len(), r.failed.len()), (1, 0), "{r:?}");
        assert_eq!(fs::read_to_string(&profile).unwrap().trim(), "balanced");
        assert!(!fw.exists());
        // Without a reboot, held.json puts it back, and firmware.json forgets it: a profile chosen later is left alone.
        let mut held = Held::new(&run).with_firmware(&fw);
        held.add(entry).unwrap();
        fs::write(&profile, "performance\n").unwrap();
        restore_both(&run, &fw).unwrap();
        assert!(!fw.exists() && !run.exists());
        fs::write(&profile, "quiet\n").unwrap();
        restore_both(&run, &fw).unwrap();
        assert_eq!(fs::read_to_string(&profile).unwrap().trim(), "quiet", "nothing left to put back");
        // Removing the entry takes it out of both.
        let mut held = Held::new(&run).with_firmware(&fw);
        held.add(HeldEntry::Platform {
            key: Key::platform_profile(),
            via: penna_core::held::PlatformVia::Sysfs { path: profile },
            original: "balanced".into(),
        })
        .unwrap();
        held.remove(&Key::platform_profile()).unwrap();
        assert!(!fw.exists() && !run.exists());
    }

    #[test]
    fn an_entry_whose_device_is_gone_waits_in_the_file() {
        let d = dir("gone");
        let (enable, pwm) = (d.join("pwm1_enable"), d.join("pwm1"));
        fs::write(&enable, "1\n").unwrap();
        fs::write(&pwm, "90\n").unwrap();
        let mut held = Held::new(d.join(FILE));
        let here = HeldEntry::Hwmon {
            key: Key::parse("hwmon/it87/a/pwm1").unwrap(),
            enable,
            original_enable: 2,
            pwm,
            original_pwm: Some(0),
            also: vec![],
        };
        let away = HeldEntry::Hwmon {
            key: Key::parse("hwmon/it8792/it87.2656/pwm1").unwrap(),
            enable: d.join("unloaded/pwm1_enable"),
            original_enable: 2,
            pwm: d.join("unloaded/pwm1"),
            original_pwm: Some(0),
            also: vec![],
        };
        held.add(here).unwrap();
        held.add(away.clone()).unwrap();
        let r = restore_file(&d.join(FILE)).unwrap();
        assert_eq!((r.done.len(), r.failed.len()), (1, 0), "a gone device is not a failure: {r:?}");
        assert_eq!(r.gone, std::slice::from_ref(&away));
        assert_eq!(load(&d.join(FILE)).unwrap().controls, [away], "only the gone entry is kept");
    }
}
