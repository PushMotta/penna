//! Real machines other than the development desk, as `penna capture` recorded them (DESIGN.md §3.6, §9.2).
//!
//! `ac8-n`: an Intel N100 mini PC on Omarchy 4.0.4 and Linux 7.2.5, captured on 23 Oct 2026. Its firmware leaves the board's DMI
//! fields as placeholders and runs its fan through ACPI: five on/off fan devices, no fan chip, nothing in hwmon to read or drive.
use penna_core::LimitKind;
use penna_hw::hwmon::Hwmon;
use penna_hw::platform::Platform;
use penna_hw::{Backend, acpi_fans, dmi_board, dmi_machine, dmi_placeholder};
use std::path::{Path, PathBuf};

fn ac8n() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures/machines/ac8-n")
}

#[test]
fn a_mini_pc_is_named_past_its_placeholders() {
    let (vendor, board) = dmi_board(&ac8n()).unwrap();
    assert!(dmi_placeholder(&vendor) && dmi_placeholder(&board), "{vendor} / {board}");
    assert_eq!(dmi_machine(&ac8n()).as_deref(), Some("AC8-N"));
    assert!(!dmi_placeholder("X570 AORUS PRO") && !dmi_placeholder("AC8-N"));
}

#[test]
fn its_fans_are_acpi_s_on_off_devices_and_nothing_in_hwmon_drives_one() {
    assert_eq!(acpi_fans(&ac8n()), 5);
    let devices = Hwmon::new(ac8n()).discover().unwrap();
    let labels: Vec<&str> = devices.iter().map(|d| d.label.as_str()).collect();
    assert_eq!(labels, ["ACPI thermal zone", "Intel CPU", "Wi-Fi"]);
    assert!(devices.iter().all(|d| d.controls.is_empty() && d.tachs.is_empty()), "no fan in hwmon");
}

#[test]
fn coretemp_brings_its_own_critical_limit() {
    let devices = Hwmon::new(ac8n()).discover().unwrap();
    let cpu = devices.iter().find(|d| d.driver == "coretemp").unwrap();
    let package = cpu.sensors.iter().find(|s| s.label == "Package id 0").unwrap();
    assert!(package.limits.iter().any(|l| l.kind == LimitKind::Crit && l.celsius == 105.0), "{:?}", package.limits);
}

#[test]
fn without_a_firmware_platform_profile_the_power_profile_is_no_fan_lever() {
    // power-profiles-daemon ran on this machine, on "performance"; with no platform_profile it changes only the CPU's power.
    assert!(!ac8n().join("firmware/acpi/platform_profile").exists());
    assert!(Platform::open(&ac8n(), false).is_none());
}
