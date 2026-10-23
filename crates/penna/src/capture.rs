//! `penna capture`: the snapshot a machine report is made from (DESIGN.md §3.6). The tree is [`penna_hw::capture`]'s; from the
//! live machine this adds what sysfs doesn't hold: the kernel version, the doctor's findings, and setup's measurements with GPU
//! UUIDs replaced. With no directory named, it packs everything into a tarball to attach to a report.
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where a machine report is filed.
pub const REPORT_URL: &str = "https://github.com/PushMotta/penna/issues/new?template=machine.yml";

const MEASURED: &str = "/var/lib/penna/measured.toml";

pub fn run(sysfs_root: &Path, dir: Option<PathBuf>) -> Result<()> {
    let live = penna_hw::is_real_sysfs(sysfs_root);
    let (dir, pack) = match dir {
        Some(d) => (d, false),
        None => (PathBuf::from(format!("penna-capture-{}-{}", slug(sysfs_root), today())), true),
    };
    let report = penna_hw::capture::capture(sysfs_root, &dir).with_context(|| format!("capturing into {}", dir.display()))?;
    let mut about = format!(
        "# What `penna capture` recorded, and from what (DESIGN.md §3.6). No serial number, UUID or asset tag is kept.\n\
         penna = {:?}\ncaptured = {:?}\n",
        env!("PENNA_VERSION"),
        today()
    );
    if live {
        if let Ok(kernel) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            about.push_str(&format!("kernel = {:?}\n", kernel.trim()));
        }
        let findings = crate::doctor::findings(&crate::doctor::gather());
        std::fs::write(dir.join("doctor.json"), serde_json::to_string_pretty(&findings)? + "\n")?;
        if let Ok(measured) = std::fs::read_to_string(MEASURED) {
            std::fs::write(dir.join("measured.toml"), without_gpu_uuids(&measured))?;
        }
    }
    about.push_str(&format!("modules = {:?}\n", report.modules));
    std::fs::write(dir.join("capture.toml"), about)?;

    println!(
        "Captured {} hwmon chips, {} PCI display devices, {} files into {}",
        report.chips,
        report.pci_devices,
        report.files,
        dir.display()
    );
    for s in &report.skipped {
        println!("  skipped {s}");
    }
    if pack {
        let tarball = dir.with_extension("tar.gz");
        let name = dir.file_name().expect("a name we made").to_owned();
        let ok = Command::new("tar").arg("-czf").arg(&tarball).arg(&name).status().context("running tar")?.success();
        if !ok {
            bail!("tar couldn't pack {}", dir.display());
        }
        println!("\nPacked into {}. Nothing has been sent anywhere.", tarball.display());
        println!("To help Penna learn this machine, attach it to a machine report: {REPORT_URL}");
    }
    Ok(())
}

/// A file name from the machine's own: `thinkpad-t14-gen-5`, `x570-aorus-pro`, else `machine`.
fn slug(sysfs_root: &Path) -> String {
    let read = |f: &str| std::fs::read_to_string(sysfs_root.join("class/dmi/id").join(f)).ok().map(|s| s.trim().to_owned());
    // Lenovo puts the model in product_version and a machine type in product_name.
    let fields: &[&str] = if read("sys_vendor").as_deref() == Some("LENOVO") {
        &["product_version", "product_name"]
    } else {
        &["product_name", "board_name"]
    };
    let placeholder = |s: &str| {
        let s = s.to_ascii_lowercase();
        ["to be filled", "system product name", "default string", "not applicable"].iter().any(|p| s.contains(p))
    };
    let name = fields
        .iter()
        .filter_map(|f| read(f))
        .find(|s| !placeholder(s) && s.chars().filter(char::is_ascii_alphanumeric).count() > 2)
        .unwrap_or_default();
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "machine".to_owned() } else { out.to_owned() }
}

/// Today's date in UTC, as `2026-10-23`.
fn today() -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The calendar date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Each NVIDIA GPU UUID replaced by `GPU-0`, `GPU-1`, … in order of first appearance: a UUID names one card, like a serial.
fn without_gpu_uuids(text: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("GPU-") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 4..];
        let len = tail.find(|c: char| !(c.is_ascii_hexdigit() || c == '-')).unwrap_or(tail.len());
        if len == 36 {
            let uuid = &tail[..len];
            let n = seen.iter().position(|s| s == uuid).unwrap_or_else(|| {
                seen.push(uuid.to_owned());
                seen.len() - 1
            });
            out.push_str(&format!("GPU-{n}"));
            rest = &tail[len..];
        } else {
            out.push_str("GPU-");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_384), (2025, 10, 23));
        assert_eq!(civil(20_749), (2026, 10, 23));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn gpu_uuids_become_numbers() {
        let text = "\"nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/temp\" = 42.0\n\
                    key = \"nvml/GPU-00000000-1111-2222-3333-444444444444/fan0\"\n\
                    \"nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan1\" = 1\nGPU-short stays\n";
        assert_eq!(
            without_gpu_uuids(text),
            "\"nvml/GPU-0/temp\" = 42.0\nkey = \"nvml/GPU-1/fan0\"\n\"nvml/GPU-0/fan1\" = 1\nGPU-short stays\n"
        );
    }

    #[test]
    fn a_slug_skips_placeholders() {
        let root = std::env::temp_dir().join(format!("penna-slug-{}", std::process::id()));
        let dmi = root.join("class/dmi/id");
        std::fs::create_dir_all(&dmi).unwrap();
        std::fs::write(dmi.join("product_version"), "-CF\n").unwrap();
        std::fs::write(dmi.join("product_name"), "To Be Filled By O.E.M.\n").unwrap();
        std::fs::write(dmi.join("board_name"), "X570 AORUS PRO\n").unwrap();
        assert_eq!(slug(&root), "x570-aorus-pro");
        std::fs::write(dmi.join("sys_vendor"), "LENOVO\n").unwrap();
        std::fs::write(dmi.join("product_name"), "21MLS0AB00\n").unwrap();
        std::fs::write(dmi.join("product_version"), "ThinkPad T14 Gen 5\n").unwrap();
        assert_eq!(slug(&root), "thinkpad-t14-gen-5");
        assert_eq!(slug(&root.join("nowhere")), "machine");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
