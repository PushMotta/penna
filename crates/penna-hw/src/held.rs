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
        Held { path: path.into(), file: HeldFile { version: VERSION, pid: std::process::id(), controls: Vec::new() } }
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

/// Puts one control back exactly as it was found. Writes nothing under the real `/sys` in M1.
pub fn restore(entry: &HeldEntry) -> Result<String> {
    match entry {
        HeldEntry::Hwmon { key, enable, original_enable, pwm, original_pwm } => {
            crate::refuse_real_sysfs(enable)?;
            if *original_enable == 1 {
                // It was already manual when we found it: the duty is part of what it was.
                write_number(pwm, *original_pwm as i64)?;
            }
            write_number(enable, *original_enable)?;
            Ok(format!("{key}: pwm_enable {original_enable}"))
        }
        HeldEntry::MockNvml { key, policy, original_policy } => {
            crate::refuse_real_sysfs(policy)?;
            if policy.parent().is_some_and(|d| d.join("lost").exists()) {
                bail!("{key}: GPU is lost");
            }
            fs::write(policy, format!("{original_policy}\n")).with_context(|| format!("writing {}", policy.display()))?;
            Ok(format!("{key}: {original_policy} policy"))
        }
        HeldEntry::Nvml { key, .. } => bail!("{key}: restoring a real NVIDIA fan arrives in M4"),
    }
}

#[derive(Debug, Default)]
pub struct Restored {
    pub done: Vec<String>,
    pub failed: Vec<String>,
}

/// Restores every entry, and removes the file only when all of them went back.
pub fn restore_file(path: &Path) -> Result<Restored> {
    let file = load(path)?;
    let mut out = Restored::default();
    for e in &file.controls {
        match restore(e) {
            Ok(s) => out.done.push(s),
            Err(err) => out.failed.push(format!("{err:#}")),
        }
    }
    if out.failed.is_empty() {
        remove_if_present(path)?;
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
    fn restore_refuses_the_real_sys() {
        let e = HeldEntry::Hwmon {
            key: Key::parse("hwmon/it87/it87.2608/pwm1").unwrap(),
            enable: "/sys/class/hwmon/hwmon5/pwm1_enable".into(),
            original_enable: 2,
            pwm: "/sys/class/hwmon/hwmon5/pwm1".into(),
            original_pwm: 0,
        };
        assert!(restore(&e).unwrap_err().to_string().contains("real /sys"));
    }
}
