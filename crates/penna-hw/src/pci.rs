//! Display devices that exist but no backend can reach: a GPU bound to `vfio-pci` belongs to a VM (DESIGN.md §0, §3.2).
use penna_core::Unreachable;
use std::fs;
use std::path::Path;

const PCI_IDS: &str = "/usr/share/hwdata/pci.ids";

pub fn unreachable(sysfs_root: &Path) -> Vec<Unreachable> {
    let Ok(entries) = fs::read_dir(sysfs_root.join("bus/pci/devices")) else { return Vec::new() };
    let mut addrs: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    addrs.sort();
    let mut ids: Option<String> = None;
    let mut out = Vec::new();
    for dev in addrs {
        let class = read_hex(&dev.join("class")).unwrap_or(0);
        if class >> 16 != 0x03 {
            continue;
        }
        let driver = fs::read_link(dev.join("driver")).ok().and_then(|p| p.file_name()?.to_str().map(str::to_owned));
        if driver.as_deref() != Some("vfio-pci") {
            continue;
        }
        let addr = dev.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let (vendor, device) = (read_hex(&dev.join("vendor")).unwrap_or(0) as u16, read_hex(&dev.join("device")).unwrap_or(0) as u16);
        let ids = ids.get_or_insert_with(|| fs::read_to_string(PCI_IDS).unwrap_or_default());
        let label = match pci_name(ids, vendor, device) {
            Some((v, d)) => format!("{} {d}", v.split_whitespace().next().unwrap_or(&v)),
            None => format!("{vendor:04x}:{device:04x}"),
        };
        out.push(Unreachable { id: format!("pci/{addr}"), label, reason: "passed through to a VM (vfio-pci)".to_owned() });
    }
    out
}

fn read_hex(path: &Path) -> Option<u32> {
    let s = fs::read_to_string(path).ok()?;
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Vendor and device names from a `pci.ids` file: vendors at column 0, their devices indented by one tab.
pub fn pci_name(ids: &str, vendor: u16, device: u16) -> Option<(String, String)> {
    let (v, d) = (format!("{vendor:04x}  "), format!("\t{device:04x}  "));
    let mut lines = ids.lines().skip_while(|l| !l.starts_with(&v));
    let vendor_name = lines.next()?[v.len()..].to_owned();
    let device_name = lines.take_while(|l| l.starts_with('\t') || l.starts_with('#')).find(|l| l.starts_with(&d))?[d.len()..].to_owned();
    Some((vendor_name, device_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &str = "# comment\n10dd  Someone Else\n\t1f06  Not this one\n10de  NVIDIA Corporation\n\t1f06  TU106 [GeForce RTX 2060 SUPER]\n\t\t10de 1f06  subsystem\n\t2204  GA102 [GeForce RTX 3090]\n10df  Next Vendor\n\t2205  Not NVIDIA\n";

    #[test]
    fn names_come_from_the_right_vendor() {
        assert_eq!(pci_name(IDS, 0x10de, 0x1f06), Some(("NVIDIA Corporation".into(), "TU106 [GeForce RTX 2060 SUPER]".into())));
        assert_eq!(pci_name(IDS, 0x10de, 0x2204).map(|n| n.1), Some("GA102 [GeForce RTX 3090]".into()));
        assert_eq!(pci_name(IDS, 0x10de, 0x2205), None);
        assert_eq!(pci_name(IDS, 0x1234, 0x0001), None);
    }
}
