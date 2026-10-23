//! A read-only snapshot of a machine's hwmon class and its PCI display devices, laid out so [`crate::hwmon::Hwmon`] and
//! [`crate::pci::unreachable`] read it unmodified (DESIGN.md §9.2). Symlinks keep their relative targets and the directories
//! they point at are created, so `class/hwmon/hwmonN` and every `device` and `driver` link resolve inside the capture.
//!
//! Beside the tree it keeps what a board entry is made from (§3.6): the DMI fields, the platform profile, and the parameters of
//! each fan driver's module, all at their sysfs paths. Files keep their modes, since a `pwmN` that is 0444 is a fact about the
//! driver. No serial number, UUID or asset tag is copied.
use anyhow::{Context, Result, bail};
use penna_core::drivers::DRIVERS;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// Attributes worth keeping from a chip's parent device: what it calls itself, never its serial number.
const DEVICE_FILES: &[&str] = &["model", "vendor", "device", "class", "subsystem_vendor", "subsystem_device"];

/// The DMI fields that name a machine and its firmware. `board_serial`, `product_serial`, `product_uuid` and the asset tags are
/// left out on purpose.
pub const DMI_FIELDS: &[&str] = &[
    "sys_vendor",
    "product_name",
    "product_family",
    "product_version",
    "product_sku",
    "board_vendor",
    "board_name",
    "board_version",
    "bios_vendor",
    "bios_version",
    "bios_date",
    "chassis_vendor",
    "chassis_type",
];

#[derive(Debug, Default)]
pub struct Report {
    pub chips: usize,
    pub files: usize,
    pub pci_devices: usize,
    /// Modules whose parameters were kept.
    pub modules: Vec<String>,
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
        // A module that registers its device with no driver link (coretemp): the module named like the chip.
        if let Some(name) = fs::read_to_string(real.join("name")).ok().map(|n| n.trim().replace('-', "_"))
            && fs::metadata(real.join("device/driver")).is_err()
            && root.join("module").join(&name).join("parameters").is_dir()
            && !report.modules.contains(&name)
        {
            copy_files(
                &root.join("module").join(&name).join("parameters"),
                &dest.join("module").join(&name).join("parameters"),
                |_| true,
                &mut report,
            )?;
            report.modules.push(name);
        }
        if let Ok(device) = fs::canonicalize(real.join("device")) {
            let device_rel = inside(&root, &device)?;
            // Older drivers keep their channels on the device rather than in the hwmon directory (§3.5).
            copy_files(&device, &dest.join(&device_rel), is_channel, &mut report)?;
            // The driver, its module and the module's parameters, linked as they are here.
            if let Ok(driver) = fs::canonicalize(device.join("driver")) {
                link_and_create_target(&device.join("driver"), &root, dest, &device_rel.join("driver"), &mut report)?;
                if let Ok(module) = fs::canonicalize(driver.join("module")) {
                    link_and_create_target(&driver.join("module"), &root, dest, &inside(&root, &driver)?.join("module"), &mut report)?;
                    let name = module.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if !report.modules.contains(&name) {
                        copy_files(
                            &module.join("parameters"),
                            &dest.join(inside(&root, &module)?).join("parameters"),
                            |_| true,
                            &mut report,
                        )?;
                        report.modules.push(name);
                    }
                }
            }
        }
        // A firmware curve beside the channels: amdgpu's OverDrive (§3.5).
        for curve in DRIVERS.iter().filter_map(|d| d.curve) {
            if let Some(dir) = fs::canonicalize(real.join(curve)).ok().and_then(|f| f.parent().map(Path::to_path_buf)) {
                copy_files(&dir, &dest.join(inside(&root, &dir)?), |_| true, &mut report)?;
            }
        }
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
            if fs::symlink_metadata(real.join("driver")).is_ok() {
                link_and_create_target(&real.join("driver"), &root, dest, &rel.join("driver"), &mut report)?;
            }
            mirror_link(&entry.path(), dest, &Path::new("bus/pci/devices").join(entry.file_name()))?;
            report.pci_devices += 1;
        }
    }

    let dmi = root.join("class/dmi/id");
    if dmi.is_dir() {
        copy_files(&dmi, &dest.join("class/dmi/id"), |name| DMI_FIELDS.contains(&name), &mut report)?;
    }
    let acpi = root.join("firmware/acpi");
    if acpi.join("platform_profile").exists() {
        copy_files(&acpi, &dest.join("firmware/acpi"), |name| name.starts_with("platform_profile"), &mut report)?;
    }
    // What the driver table reads beside the channels: a module parameter that allows writes, a firmware watchdog (§3.5).
    for path in DRIVERS.iter().flat_map(|d| d.allowed.map(|a| a.path).into_iter().chain(d.watchdog.map(|w| w.path))) {
        let (from, to) = (root.join(path), dest.join(path));
        if from.is_file() && !to.exists() {
            let name = to.file_name().and_then(|n| n.to_str()).map(str::to_owned);
            copy_files(
                from.parent().expect("a sysfs file has a parent"),
                to.parent().expect("as here"),
                |n| Some(n) == name.as_deref(),
                &mut report,
            )?;
        }
    }
    // The thermal class: the zones the firmware cools by, and its cooling devices. A fan the firmware runs through ACPI is one of
    // these, of type `Fan`, and nowhere in hwmon.
    if let Ok(entries) = fs::read_dir(root.join("class/thermal")) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let keep = |n: &str| ["type", "max_state", "cur_state", "temp", "policy"].contains(&n) || n.starts_with("trip_point_");
            copy_files(&entry.path(), &dest.join("class/thermal").join(&name), keep, &mut report)?;
        }
    }
    report.modules.sort();
    Ok(report)
}

/// A channel attribute: `fan1_input`, `pwm2`, `temp3_label`, and the chip's `name`.
fn is_channel(name: &str) -> bool {
    name == "name"
        || ["fan", "pwm", "temp", "in", "curr", "power"]
            .iter()
            .any(|p| name.strip_prefix(p).is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit())))
}

fn inside(root: &Path, real: &Path) -> Result<PathBuf> {
    Ok(real.strip_prefix(root).with_context(|| format!("{} is outside {}", real.display(), root.display()))?.to_path_buf())
}

/// Copies the regular, readable files directly in `from` (no subdirectories, no links), keeping their modes. A file already
/// there is left alone. Nothing happens when `from` isn't there.
fn copy_files(from: &Path, to: &Path, keep: impl Fn(&str) -> bool, report: &mut Report) -> Result<()> {
    let Ok(entries) = fs::read_dir(from) else { return Ok(()) };
    fs::create_dir_all(to)?;
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "uevent" || !keep(name) || !entry.file_type().is_ok_and(|t| t.is_file()) || to.join(name).exists() {
            continue;
        }
        match fs::read(entry.path()) {
            Ok(bytes) => {
                fs::write(to.join(name), bytes)?;
                if let Ok(meta) = entry.metadata() {
                    fs::set_permissions(to.join(name), meta.permissions())?;
                }
                report.files += 1;
            }
            Err(e) => report.skipped.push(format!("{}: {e}", entry.path().display())),
        }
    }
    Ok(())
}

/// Recreates the symlink at `link` (inside the real tree) at `dest/rel` with the same relative target, creates the directory
/// it resolves to inside `dest`, and for a parent device copies its identifying attributes. A link whose target is gone is
/// kept as it is: only its name matters (a capture checked out from git has no empty directories).
fn link_and_create_target(link: &Path, root: &Path, dest: &Path, rel: &Path, report: &mut Report) -> Result<()> {
    let Ok(target) = fs::read_link(link) else { return Ok(()) };
    if let Ok(real_target) = fs::canonicalize(link) {
        let rel_target = inside(root, &real_target)?;
        copy_files(&real_target, &dest.join(&rel_target), |name| DEVICE_FILES.contains(&name), report)?;
        // A directory with nothing kept still has to exist for the link to resolve.
        fs::create_dir_all(dest.join(&rel_target))?;
    }
    let at = dest.join(rel);
    if fs::symlink_metadata(&at).is_err() {
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
