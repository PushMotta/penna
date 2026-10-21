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

/// The daemon's live copy of `held.json`.
pub struct Held {
    path: PathBuf,
    file: HeldFile,
}

impl Held {
    pub fn new(path: impl Into<PathBuf>) -> Held {
        Held::resume(path, Vec::new())
    }

    /// Starts from entries a previous daemon left for devices that were gone (§3.6): they wait here, on disk, until the
    /// device comes back and can be put back as it was found.
    pub fn resume(path: impl Into<PathBuf>, entries: Vec<HeldEntry>) -> Held {
        Held { path: path.into(), file: HeldFile { version: VERSION, pid: std::process::id(), controls: entries } }
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

    /// Adds the way back for a control and puts it on disk before returning: the write-ahead of §6.1.
    pub fn add(&mut self, entry: HeldEntry) -> Result<()> {
        self.file.controls.retain(|e| e.key() != entry.key());
        self.file.controls.push(entry);
        self.save()
    }

    pub fn remove(&mut self, key: &Key) -> Result<()> {
        self.file.controls.retain(|e| e.key() != key);
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
    {
        let mut f =
            OpenOptions::new().write(true).create(true).truncate(true).open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
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

/// Puts one control back exactly as it was found. A real NVIDIA fan opens NVML for the purpose (M4).
pub fn restore(entry: &HeldEntry) -> Result<String> {
    match entry {
        HeldEntry::Hwmon { key, enable, original_enable, pwm, original_pwm } => {
            if *original_enable == 1 {
                // It was already manual when we found it: the duty is part of what it was.
                write_number(pwm, *original_pwm as i64)?;
            }
            write_number(enable, *original_enable)?;
            Ok(format!("{key}: pwm_enable {original_enable}"))
        }
        HeldEntry::MockNvml { key, policy, original_policy } => {
            if policy.parent().is_some_and(|d| d.join("lost").exists()) {
                bail!("{key}: GPU is lost");
            }
            fs::write(policy, format!("{original_policy}\n")).with_context(|| format!("writing {}", policy.display()))?;
            Ok(format!("{key}: {original_policy} policy"))
        }
        HeldEntry::Nvml { .. } => crate::nvml::restore_entry(entry),
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
    }
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
    fn add_writes_before_returning_and_remove_deletes_when_empty() {
        let d = dir("add");
        let enable = d.join("pwm1_enable");
        fs::write(&enable, "5\n").unwrap();
        let mut held = Held::new(d.join(FILE));
        let key = Key::parse("hwmon/it87/it87.2608/pwm1").unwrap();
        held.add(HeldEntry::Hwmon { key: key.clone(), enable: enable.clone(), original_enable: 5, pwm: d.join("pwm1"), original_pwm: 77 })
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
            original_pwm: 90,
        })
        .unwrap();
        let r = restore_file(&d.join(FILE)).unwrap();
        assert_eq!(r.failed, Vec::<String>::new());
        assert_eq!(fs::read_to_string(&enable).unwrap().trim(), "5");
        assert_eq!(fs::read_to_string(&pwm).unwrap().trim(), "200", "an automatic mode ignores the duty: left alone");
        assert!(!d.join(FILE).exists());
    }

    #[test]
    fn an_entry_whose_device_is_gone_waits_in_the_file() {
        let d = dir("gone");
        let (enable, pwm) = (d.join("pwm1_enable"), d.join("pwm1"));
        fs::write(&enable, "1\n").unwrap();
        fs::write(&pwm, "90\n").unwrap();
        let mut held = Held::new(d.join(FILE));
        let here = HeldEntry::Hwmon { key: Key::parse("hwmon/it87/a/pwm1").unwrap(), enable, original_enable: 2, pwm, original_pwm: 0 };
        let away = HeldEntry::Hwmon {
            key: Key::parse("hwmon/it8792/it87.2656/pwm1").unwrap(),
            enable: d.join("unloaded/pwm1_enable"),
            original_enable: 2,
            pwm: d.join("unloaded/pwm1"),
            original_pwm: 0,
        };
        held.add(here).unwrap();
        held.add(away.clone()).unwrap();
        let r = restore_file(&d.join(FILE)).unwrap();
        assert_eq!((r.done.len(), r.failed.len()), (1, 0), "a gone device is not a failure: {r:?}");
        assert_eq!(r.gone, std::slice::from_ref(&away));
        assert_eq!(load(&d.join(FILE)).unwrap().controls, [away], "only the gone entry is kept");
    }
}
