//! The real hwmon and PCI code against this desk, captured on 21 Oct 2026 with `penna capture` (DESIGN.md §0, §9.2).
use penna_core::{Device, Key, Limit, LimitKind, Readings, Sample};
use penna_hw::{Backend, Machine, Options, capture, hwmon::Hwmon, pci};
use std::path::{Path, PathBuf};

fn desk() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures/desk-2026-10-21")
}

fn discover(root: &Path) -> Vec<Device> {
    Hwmon::new(root).discover().expect("the fixture is readable")
}

fn device<'a>(devices: &'a [Device], id: &str) -> &'a Device {
    devices.iter().find(|d| d.id == id).unwrap_or_else(|| panic!("no {id} in {:?}", devices.iter().map(|d| &d.id).collect::<Vec<_>>()))
}

#[test]
fn three_chips_and_the_gap_where_hwmon1_was() {
    assert!(!desk().join("class/hwmon/hwmon1").exists());
    let ids: Vec<String> = discover(&desk()).into_iter().map(|d| d.id).collect();
    assert_eq!(ids, ["hwmon/acpitz/thermal_zone0", "hwmon/nvme/0000:04:00.0", "hwmon/k10temp/0000:00:18.3"]);
}

#[test]
fn k10temp_labels_and_keys() {
    let devices = discover(&desk());
    let cpu = device(&devices, "hwmon/k10temp/0000:00:18.3");
    assert_eq!(cpu.label, "AMD CPU");
    let sensors: Vec<(&str, &str)> = cpu.sensors.iter().map(|s| (s.key.as_str(), s.label.as_str())).collect();
    assert_eq!(
        sensors,
        [
            ("hwmon/k10temp/0000:00:18.3/temp1", "Tctl"),
            ("hwmon/k10temp/0000:00:18.3/temp3", "Tccd1"),
            ("hwmon/k10temp/0000:00:18.3/temp4", "Tccd2"),
        ]
    );
    assert!(cpu.sensors.iter().all(|s| s.limits.is_empty() && !s.below_ambient), "k10temp gives no limits");
}

#[test]
fn nvme_is_keyed_by_its_pci_address_and_its_placeholder_limits_are_ignored() {
    let devices = discover(&desk());
    let nvme = device(&devices, "hwmon/nvme/0000:04:00.0");
    assert_eq!(nvme.model.as_deref(), Some("Samsung SSD 980 PRO 1TB"));
    let composite = &nvme.sensors[0];
    assert_eq!(composite.label, "Composite");
    assert_eq!(composite.limits, [Limit { kind: LimitKind::Max, celsius: 81.85 }, Limit { kind: LimitKind::Crit, celsius: 84.85 }]);
    assert!(composite.ignored_limits.is_empty());
    for s in &nvme.sensors[1..] {
        assert!(s.limits.is_empty(), "{}", s.label);
        assert_eq!(s.ignored_limits, [Limit { kind: LimitKind::Max, celsius: 65261.85 }], "{}", s.label);
    }
}

#[test]
fn acpitz_is_listed_but_flagged() {
    let devices = discover(&desk());
    let acpi = device(&devices, "hwmon/acpitz/thermal_zone0");
    assert_eq!(acpi.sensors.len(), 2);
    assert!(acpi.sensors.iter().all(|s| s.below_ambient));
}

#[test]
fn readings_are_temperatures() {
    let mut hwmon = Hwmon::new(desk());
    hwmon.discover().unwrap();
    let mut readings = Readings::default();
    hwmon.read(&mut readings).unwrap();
    assert_eq!(readings.temps.len(), 8);
    for (key, sample) in &readings.temps {
        let c = sample.value().unwrap_or_else(|| panic!("{key}: {sample:?}"));
        assert!((10.0..100.0).contains(&c), "{key}: {c}");
    }
    assert!(readings.rpms.is_empty() && readings.controls.is_empty(), "no Super I/O driver: no fans, no controls");
}

#[test]
fn a_vanished_attribute_is_a_failed_read_not_an_error() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("vanished");
    let _ = std::fs::remove_dir_all(&root);
    capture::capture(&desk(), &root).unwrap();
    let mut hwmon = Hwmon::new(&root);
    hwmon.discover().unwrap();
    std::fs::remove_file(root.join("class/hwmon/hwmon3/temp1_input")).unwrap();
    let mut readings = Readings::default();
    hwmon.read(&mut readings).unwrap();
    assert!(matches!(readings.temps[&Key::hwmon("k10temp", "0000:00:18.3", "temp1")], Sample::Error(_)));
    assert!(readings.temps[&Key::hwmon("k10temp", "0000:00:18.3", "temp3")].value().is_some());
}

#[test]
fn the_2060_is_passed_through() {
    let u = pci::unreachable(&desk());
    assert_eq!(u.len(), 1, "{u:?}");
    assert_eq!(u[0].id, "pci/0000:0a:00.0");
    assert_eq!(u[0].reason, "passed through to a VM (vfio-pci)");
    assert!(u[0].label.contains("2060") || u[0].label == "10de:1f06", "{}", u[0].label);
}

#[test]
fn a_capture_of_a_capture_reads_the_same() {
    let again = Path::new(env!("CARGO_TARGET_TMPDIR")).join("recapture");
    let _ = std::fs::remove_dir_all(&again);
    let report = capture::capture(&desk(), &again).unwrap();
    assert_eq!((report.chips, report.pci_devices), (3, 2));
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(discover(&again), discover(&desk()));
    assert_eq!(pci::unreachable(&again), pci::unreachable(&desk()));
}

#[test]
fn capture_refuses_a_directory_with_something_in_it() {
    let dest = Path::new(env!("CARGO_TARGET_TMPDIR")).join("occupied");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("keep-me"), "").unwrap();
    assert!(capture::capture(&desk(), &dest).is_err());
    assert!(dest.join("keep-me").exists());
}

#[test]
fn the_machine_says_why_there_are_no_motherboard_fans() {
    let machine = Machine::open(&Options { sysfs_root: desk(), nvml: false, ..Options::default() }).unwrap();
    assert_eq!(machine.inventory.devices.len(), 3);
    assert!(machine.inventory.notes.iter().any(|n| n.starts_with("Motherboard fans: none visible")), "{:?}", machine.inventory.notes);
}

/// What pennad writes on first start, for this desk: the fixture's hwmon devices plus the 3090 as NVML reports it (§0).
#[test]
fn the_starter_for_this_desk_curves_the_3090_and_tctl_and_adopts_nothing() {
    use penna_core::config::{Config, Measured};
    use penna_core::plan::Plan;
    use penna_core::{ControlInfo, ControlKind, Inventory, SensorInfo, Source, starter};
    let uuid = "GPU-168e2769-d673-f36a-ea7b-64df57f8d090";
    let mut devices = discover(&desk());
    devices.push(Device {
        id: format!("nvml/{uuid}"),
        source: Source::Nvml,
        driver: "nvml".into(),
        label: "NVIDIA GeForce RTX 3090".into(),
        location: "0000:09:00.0".into(),
        model: None,
        sensors: vec![SensorInfo {
            key: Key::nvml_temp(uuid),
            label: "GPU".into(),
            limits: vec![],
            ignored_limits: vec![],
            below_ambient: false,
        }],
        tachs: vec![],
        controls: (0..2)
            .map(|f| ControlInfo {
                key: Key::nvml_fan(uuid, f),
                label: format!("Fan {}", f + 1),
                kind: ControlKind::Nvml { min: 30, max: 100 },
            })
            .collect(),
    });
    let inventory = Inventory { devices, ..Inventory::default() };
    let config = starter::starter(&inventory);
    let text = starter::text(&config);
    assert!(text.starts_with("# Penna's configuration, generated by pennad on its first start"));
    let read = Config::parse(&text).expect("the starter reads back");
    assert_eq!(read.curves.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["gpu", "cpu-quiet"]);
    assert_eq!(read.curves[1].kind.source(), Some(&Key::hwmon("k10temp", "0000:00:18.3", "temp1")), "Tctl, not a CCD");
    let plan = Plan::new(read, &inventory, &Measured::default(), Some("AMD Ryzen 9 5950X 16-Core Processor")).expect("it fits the desk");
    assert!(plan.config.profiles.values().all(|p| p.is_empty()), "the firmware keeps every fan");
}
