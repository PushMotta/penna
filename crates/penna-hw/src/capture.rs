//! A read-only snapshot of a machine's hwmon class and its PCI display devices, laid out so [`crate::hwmon::Hwmon`] and
//! [`crate::pci::unreachable`] read it unmodified (DESIGN.md §9.2). Symlinks keep their relative targets and the directories
//! they point at are created, so `class/hwmon/hwmonN` and every `device` and `driver` link resolve inside the capture.
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// Attributes worth keeping from a chip's parent device: what it calls itself, never its serial number.
const DEVICE_FILES: &[&str] = &["model", "vendor", "device", "class", "subsystem_vendor", "subsystem_device"];

#[derive(Debug, Default)]
pub struct Report {
    pub chips: usize,
    pub files: usize,
    pub pci_devices: usize,
    /// Attributes that could not be read (`EIO`, `ENODATA`, write-only), as `path: reason`.
    pub skipped: Vec<String>,
}

pub fn capture(sysfs_root: &Path, dest: &Path) -> Result<Report> {
    if dest.exists() && fs::read_dir(dest)?.next().is_some() {
        bail!("{} is not empty", dest.display());
    }
    let root = fs::canonicalize(sysfs_root).with_context(|| format!("reading {}", sysfs_root.display()))?;
    let mut report = Report::default();

    let class = root.join("class/hwmon");
    let mut chips: Vec<_> = fs::read_dir(&class).with_context(|| format!("reading {}", class.display()))?.filter_map(|e| e.ok()).collect();
    chips.sort_by_key(|e| e.file_name());
    for entry in chips {
        let real = fs::canonicalize(entry.path())?;
        let rel = inside(&root, &real)?;
        copy_files(&real, &dest.join(&rel), |_| true, &mut report)?;
        link_and_create_target(&real.join("device"), &root, dest, &rel.join("device"), &mut report)?;
        mirror_link(&entry.path(), dest, &Path::new("class/hwmon").join(entry.file_name()))?;
        report.chips += 1;
    }

    if let Ok(entries) = fs::read_dir(root.join("bus/pci/devices")) {
        let mut devices: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        devices.sort_by_key(|e| e.file_name());
        for entry in devices {
            let real = fs::canonicalize(entry.path())?;
            let is_display = fs::read_to_string(real.join("class")).is_ok_and(|c| c.trim().starts_with("0x03"));
            if !is_display {
                continue;
            }
            let rel = inside(&root, &real)?;
            copy_files(&real, &dest.join(&rel), |name| DEVICE_FILES.contains(&name), &mut report)?;
            if real.join("driver").exists() {
                link_and_create_target(&real.join("driver"), &root, dest, &rel.join("driver"), &mut report)?;
            }
            mirror_link(&entry.path(), dest, &Path::new("bus/pci/devices").join(entry.file_name()))?;
            report.pci_devices += 1;
        }
    }
    Ok(report)
}

fn inside(root: &Path, real: &Path) -> Result<PathBuf> {
    Ok(real.strip_prefix(root).with_context(|| format!("{} is outside {}", real.display(), root.display()))?.to_path_buf())
}

/// Copies the regular, readable files directly in `from` (no subdirectories, no links).
fn copy_files(from: &Path, to: &Path, keep: impl Fn(&str) -> bool, report: &mut Report) -> Result<()> {
    fs::create_dir_all(to)?;
    let mut entries: Vec<_> = fs::read_dir(from)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "uevent" || !keep(name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        match fs::read(entry.path()) {
            Ok(bytes) => {
                fs::write(to.join(name), bytes)?;
                report.files += 1;
            }
            Err(e) => report.skipped.push(format!("{}: {e}", entry.path().display())),
        }
    }
    Ok(())
}

/// Recreates the symlink at `link` (inside the real tree) at `dest/rel` with the same relative target, creates the directory
/// it resolves to inside `dest`, and for a parent device copies its identifying attributes.
fn link_and_create_target(link: &Path, root: &Path, dest: &Path, rel: &Path, report: &mut Report) -> Result<()> {
    let (Ok(target), Ok(real_target)) = (fs::read_link(link), fs::canonicalize(link)) else { return Ok(()) };
    let rel_target = inside(root, &real_target)?;
    copy_files(&real_target, &dest.join(&rel_target), |name| DEVICE_FILES.contains(&name), report)?;
    let at = dest.join(rel);
    if !at.exists() {
        symlink(&target, &at)?;
    }
    Ok(())
}

fn mirror_link(link: &Path, dest: &Path, rel: &Path) -> Result<()> {
    let target = fs::read_link(link)?;
    let at = dest.join(rel);
    fs::create_dir_all(at.parent().expect("a class or bus path has a parent"))?;
    symlink(target, at)?;
    Ok(())
}
