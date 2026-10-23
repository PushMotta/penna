//! The write half of the backends and the simulator, against a mock desk built in a temporary directory (DESIGN.md §6.1, §9.1).
use penna_core::held::HeldEntry;
use penna_core::{ControlKind, ControlMode, Key, Sample};
use penna_hw::sim::{self, Sim};
use penna_hw::{Machine, Options, held};
use std::fs;
use std::path::{Path, PathBuf};

fn fresh(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hw-{name}"));
    if d.exists() {
        let _ = std::process::Command::new("chmod").args(["-R", "u+rwX"]).arg(&d).status();
        fs::remove_dir_all(&d).unwrap();
    }
    d
}

fn desk(name: &str, scenario: &str) -> (Sim, Machine) {
    let (sc, _) = sim::load(scenario).unwrap();
    let sim = Sim::create(sc, &fresh(name)).unwrap();
    let machine = Machine::open(&Options {
        sysfs_root: sim.root().into(),
        nvml: false,
        mock_nvml: Some(sim.mock_nvml_root()),
        control: true,
        boards: None,
    })
    .unwrap();
    (sim, machine)
}

fn pwm(n: u32) -> Key {
    Key::hwmon("it87", "it87.2608", &format!("pwm{n}"))
}
fn gpu_fan(n: u32) -> Key {
    Key::nvml_fan("GPU-5a1d3090-0000-4000-8000-00000000mock", n)
}
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_owned()
}

#[test]
fn the_mock_desk_is_discovered_like_a_real_one() {
    let (_sim, machine) = desk("discover", "render-burst");
    let ids: Vec<&str> = machine.inventory.devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "hwmon/acpitz/thermal_zone0",
            "hwmon/nvme/0000:04:00.0",
            "hwmon/k10temp/0000:00:18.3",
            "hwmon/it87/it87.2608",
            "nvml/GPU-5a1d3090-0000-4000-8000-00000000mock"
        ]
    );
    let it87 = &machine.inventory.devices[3];
    assert_eq!(it87.controls.len(), 4);
    assert_eq!(it87.tachs.len(), 6);
    let gpu = &machine.inventory.devices[4];
    assert_eq!(gpu.controls[0].kind, ControlKind::Nvml { min: 30, max: 100 });
    assert!(machine.inventory.notes.is_empty(), "{:?}", machine.inventory.notes);
}

#[test]
fn take_is_bumpless_and_the_way_back_is_exact() {
    let (sim, mut machine) = desk("take", "render-burst");
    let dir = sim.root().join("devices/platform/it87.2608/hwmon/hwmon5");
    let before = read(&dir.join("pwm3"));
    let entry = machine.original(&pwm(3)).unwrap();
    assert!(matches!(&entry, HeldEntry::Hwmon { original_enable: 5, .. }), "{entry:?}");
    let duty = machine.take(&pwm(3)).unwrap();
    assert_eq!(read(&dir.join("pwm3_enable")), "1");
    assert_eq!(read(&dir.join("pwm3")), before, "no jump");
    assert!((duty - before.parse::<f64>().unwrap() * 100.0 / 255.0).abs() < 1e-9);
    let rb = machine.write(&pwm(3), 128).unwrap();
    assert_eq!((rb.raw, rb.mode), (Some(128), ControlMode::Manual));
    held::restore(&entry).unwrap();
    assert_eq!(read(&dir.join("pwm3_enable")), "5", "the exact original, not an assumed 2");
    // On the IT8688E the manual duty register is also `pwm3_auto_start`: the firmware's curve would start at 128 (§6.1).
    assert_eq!(read(&dir.join("pwm3")), before, "the duty it was found at, too");
}

/// Adds an hwmon device to a mock tree the way a driver loaded later would: its directory under `devices`, then the class link.
fn plug(root: &Path, platform: &str, hwmon: u32, name: &str, pwms: &[(u32, u32)]) -> PathBuf {
    let dir = root.join(format!("devices/platform/{platform}/hwmon/hwmon{hwmon}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("name"), format!("{name}\n")).unwrap();
    std::os::unix::fs::symlink("../..", dir.join("device")).unwrap();
    for (n, mode) in pwms {
        for (file, v) in [(format!("pwm{n}"), "128"), (format!("pwm{n}_enable"), "2"), (format!("fan{n}_input"), "900")] {
            fs::write(dir.join(&file), format!("{v}\n")).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.join(format!("pwm{n}")), fs::Permissions::from_mode(*mode)).unwrap();
    }
    std::os::unix::fs::symlink(
        format!("../../devices/platform/{platform}/hwmon/hwmon{hwmon}"),
        root.join(format!("class/hwmon/hwmon{hwmon}")),
    )
    .unwrap();
    dir
}

#[test]
fn a_driver_loaded_later_is_found_without_a_restart() {
    let (sim, mut machine) = desk("later", "render-burst");
    assert!(!machine.rediscover(), "nothing changed yet");
    let before = machine.inventory.devices.len();
    plug(sim.root(), "it87.2624", 9, "it8792", &[(1, 0o644), (2, 0o644)]);
    assert!(machine.rediscover());
    assert_eq!(machine.inventory.devices.len(), before + 1);
    let chip = machine.inventory.devices.iter().find(|d| d.driver == "it8792").expect("the new chip");
    assert_eq!(chip.label, "Motherboard");
    assert!(chip.controls.iter().all(|c| c.kind == ControlKind::Duty { scale: 255 }));
    let key = Key::hwmon("it8792", "it87.2624", "pwm1");
    let entry = machine.original(&key).unwrap();
    let HeldEntry::Hwmon { enable, .. } = &entry else { panic!("{entry:?}") };
    assert!(!enable.to_string_lossy().contains("class/hwmon"), "held.json names the device's real path: {}", enable.display());
    machine.take(&key).unwrap();
    assert!(!machine.rediscover(), "taking a control changes no device");
}

#[test]
fn held_entries_follow_a_control_to_its_new_path() {
    let (sim, mut machine) = desk("relocate", "render-burst");
    plug(sim.root(), "it87.2624", 9, "it8792", &[(1, 0o644)]);
    machine.rediscover();
    let key = Key::hwmon("it8792", "it87.2624", "pwm1");
    let entry = machine.original(&key).unwrap();
    // The driver is reloaded and the chip comes back as another hwmonN.
    fs::remove_file(sim.root().join("class/hwmon/hwmon9")).unwrap();
    fs::rename(sim.root().join("devices/platform/it87.2624/hwmon/hwmon9"), sim.root().join("devices/platform/it87.2624/hwmon/hwmon11"))
        .unwrap();
    std::os::unix::fs::symlink("../../devices/platform/it87.2624/hwmon/hwmon11", sim.root().join("class/hwmon/hwmon11")).unwrap();
    assert!(machine.rediscover());
    let moved = machine.relocate(&entry).expect("still a control here");
    let (HeldEntry::Hwmon { enable: a, original_enable: oa, .. }, HeldEntry::Hwmon { enable: b, original_enable: ob, .. }) =
        (&entry, &moved)
    else {
        panic!()
    };
    assert_ne!(a, b);
    assert!(b.ends_with("hwmon11/pwm1_enable"), "{}", b.display());
    assert_eq!(oa, ob, "what it was found as is kept");
}

#[test]
fn channels_penna_does_not_drive_are_listed_and_never_written() {
    let (sim, mut machine) = desk("kinds", "render-burst");
    plug(sim.root(), "asus-nb-wmi", 12, "asus", &[(1, 0o644)]);
    plug(sim.root(), "nct6683.2592", 13, "nct6687", &[(1, 0o444)]);
    plug(sim.root(), "thinkpad_hwmon", 14, "thinkpad", &[(1, 0o444)]);
    machine.rediscover();
    let kind = |driver: &str| machine.inventory.devices.iter().find(|d| d.driver == driver).unwrap().controls[0].kind;
    assert_eq!(kind("asus"), ControlKind::Modes);
    assert_eq!(kind("nct6687"), ControlKind::ReadOnly, "nct6683's pwm is read-only");
    assert_eq!(kind("thinkpad"), ControlKind::ReadOnly, "without fan_control=1 a ThinkPad's pwm is read-only");
    let asus = Key::hwmon("asus", "asus-nb-wmi", "pwm1");
    let err = machine.original(&asus).unwrap_err().to_string();
    assert!(err.contains("firmware's modes"), "{err}");
    assert!(machine.write(&asus, 255).is_err());
    let dir = sim.root().join("devices/platform/asus-nb-wmi/hwmon/hwmon12");
    assert_eq!(read(&dir.join("pwm1")), "128");
}

#[test]
fn a_stepped_control_is_written_and_read_as_its_levels() {
    let (sim, mut machine) = desk("levels", "render-burst");
    plug(sim.root(), "thinkpad_hwmon", 12, "thinkpad", &[(1, 0o644)]);
    machine.rediscover();
    let tp = Key::hwmon("thinkpad", "thinkpad_hwmon", "pwm1");
    let dir = sim.root().join("devices/platform/thinkpad_hwmon/hwmon/hwmon12");
    assert!(matches!(machine.inventory.devices.iter().flat_map(|d| &d.controls).find(|c| c.key == tp).unwrap().kind,
        ControlKind::Levels { steps: 8, .. }));
    let entry = machine.original(&tp).unwrap();
    assert!(matches!(&entry, HeldEntry::Hwmon { original_enable: 2, original_pwm: Some(128), .. }), "{entry:?}");
    // 128 is level 4 (`128 >> 5`), which the driver reads back as 4 × 255 / 7.
    let duty = machine.take(&tp).unwrap();
    assert!((duty - 400.0 / 7.0).abs() < 1e-6, "{duty}");
    assert_eq!((read(&dir.join("pwm1_enable")), read(&dir.join("pwm1"))), ("1".to_owned(), "146".to_owned()));
    for (level, file) in [(7, "255"), (1, "36"), (0, "0"), (5, "182")] {
        let r = machine.write(&tp, level).unwrap();
        assert_eq!((read(&dir.join("pwm1")), r.raw), (file.to_owned(), Some(level)), "level {level}");
    }
    let mut readings = penna_core::Readings::default();
    readings = { let _ = &mut readings; machine.read() };
    let r = readings.controls[&tp].value().unwrap();
    assert_eq!(r.raw, Some(5));
    assert!((r.percent - 500.0 / 7.0).abs() < 1e-6);
}

#[test]
fn read_only_hwmon_refuses_writes() {
    let (sim, _) = desk("readonly", "render-burst");
    let mut m =
        Machine::open(&Options { sysfs_root: sim.root().into(), nvml: false, mock_nvml: None, control: false, boards: None }).unwrap();
    assert!(m.write(&pwm(1), 100).is_err());
}

#[test]
fn the_mock_gpu_behaves_like_the_desk_3090() {
    let (mut sim, mut machine) = desk("gpu", "render-burst");
    sim.advance(5.0).unwrap();
    let r = machine.read();
    let Sample::Value(c) = &r.controls[&gpu_fan(0)] else { panic!() };
    assert_eq!(c.mode, ControlMode::Driver);
    assert_eq!(r.rpms[&gpu_fan(0)], Sample::Value(0), "the driver stops the fans when cool");
    assert!(machine.write(&gpu_fan(0), 20).unwrap_err().to_string().contains("outside 30–100"));
    machine.write(&gpu_fan(0), 100).unwrap();
    sim.advance(15.0).unwrap();
    let r = machine.read();
    let Sample::Value(c) = &r.controls[&gpu_fan(0)] else { panic!() };
    assert_eq!(c.mode, ControlMode::Manual);
    assert!(c.percent > 100.0, "percent of the noise-tolerance speed, not duty: {}", c.percent);
    machine.handback(&gpu_fan(0)).unwrap();
    let r = machine.read();
    assert!(matches!(&r.controls[&gpu_fan(0)], Sample::Value(c) if c.mode == ControlMode::Driver));
}

#[test]
fn faults_happen_on_the_timeline() {
    let (mut sim, mut machine) = desk("faults", "faults");
    let tctl = Key::hwmon("k10temp", "0000:00:18.3", "temp1");
    sim.advance(25.5).unwrap();
    assert!(matches!(machine.read().temps[&tctl], Sample::Error(_)), "sensor-gone at 25 s");
    sim.advance(6.0).unwrap();
    assert!(machine.read().temps[&tctl].value().is_some(), "back after 6 s");
    sim.advance(10.0).unwrap(); // 41.5 s: NVMe garbage
    assert!(matches!(&machine.read().temps[&Key::hwmon("nvme", "0000:04:00.0", "temp1")], Sample::Error(e) if e.contains("garbage")));
    sim.advance(11.0).unwrap(); // 52.5 s: Tccd1 unreadable
    assert!(
        matches!(&machine.read().temps[&Key::hwmon("k10temp", "0000:00:18.3", "temp3")], Sample::Error(e) if e.contains("Permission denied"))
    );
    sim.advance(10.0).unwrap(); // 62.5 s: the firmware takes pwm2 back
    machine.original(&pwm(2)).unwrap();
    machine.take(&pwm(2)).unwrap();
    sim.advance(0.5).unwrap();
    let r = machine.read();
    assert!(matches!(&r.controls[&pwm(2)], Sample::Value(c) if c.mode == ControlMode::Automatic(2)));
    sim.advance(13.0).unwrap(); // 76 s: the rear tach is stuck
    machine.write(&pwm(3), 200).ok();
    assert_eq!(machine.read().rpms[&Key::hwmon("it87", "it87.2608", "fan5")], Sample::Value(0));
    sim.advance(52.5).unwrap(); // 128.5 s: the GPU is lost
    let r = machine.read();
    assert!(matches!(&r.temps[&Key::nvml_temp("GPU-5a1d3090-0000-4000-8000-00000000mock")], Sample::Error(e) if e.contains("lost")));
    assert!(machine.write(&gpu_fan(0), 50).is_err());
}

#[test]
fn a_resume_resets_every_control_to_its_firmware_mode() {
    let (mut sim, mut machine) = desk("resume", "faults");
    for n in [1, 3] {
        machine.original(&pwm(n)).unwrap();
        machine.take(&pwm(n)).unwrap();
    }
    sim.advance(91.0).unwrap();
    let dir = sim.root().join("devices/platform/it87.2608/hwmon/hwmon5");
    assert_eq!((read(&dir.join("pwm1_enable")), read(&dir.join("pwm3_enable"))), ("2".into(), "5".into()));
}

/// The x570 desk's tachs as setup found them on the real one (§5.4): it87 keeps a reading 1.5 s, so on a 1 s tick each shows
/// twice, and SYS_FAN4 overshoots a raise to 100 % and comes back to its own speed over some 20 s.
#[test]
fn the_x570_desk_tachs_repeat_and_sys_fan4_overshoots() {
    let (mut sim, mut machine) = desk("x570-tachs", "x570-desk");
    let sys4 = Key::hwmon("it8792_4008090b", "it87.2656", "pwm3");
    let tach = Key::hwmon("it8792_4008090b", "it87.2656", "fan3");
    sim.advance(1.0).unwrap();
    machine.take(&sys4).unwrap();
    machine.write(&sys4, 153).unwrap();
    sim.advance(30.0).unwrap();
    machine.write(&sys4, 255).unwrap();
    let mut seen = Vec::new();
    for _ in 0..30 {
        sim.advance(1.0).unwrap();
        let Sample::Value(r) = machine.read().rpms[&tach] else { panic!() };
        seen.push(r as f64);
    }
    assert!(seen.windows(3).all(|w| (w[0] == w[1]) != (w[1] == w[2])), "each reading twice: {seen:?}");
    let peak = seen.iter().copied().fold(0.0, f64::max);
    let last = *seen.last().unwrap();
    assert!(peak > 2050.0 && (last - 1800.0).abs() < 1800.0 * 0.03, "up to {peak}, back to {last}: {seen:?}");
}

#[test]
fn the_simulator_knows_what_pairing_would_find() {
    let (sim, _) = desk("measured", "render-burst");
    let m = sim.measured();
    let front = m.get(&pwm(2)).unwrap();
    assert_eq!(front.tachs.len(), 3);
    assert_eq!((front.stop, front.start), (Some(22.0), Some(32.0)));
    assert_eq!(m.get(&pwm(4)).unwrap().tachs, [Key::hwmon("it87", "it87.2608", "fan6")]);

    // Calibration: from the stop duty to 100 %, ascending, as the fan model spins the fans.
    let cpu = &m.get(&pwm(1)).unwrap().calibration;
    assert_eq!(cpu.first(), Some(&[20.0, (1800.0 * 0.2f64.powf(0.8)).round()]));
    assert_eq!(cpu.last(), Some(&[100.0, 1800.0]));
    assert!(cpu.windows(2).all(|w| w[0][0] < w[1][0] && w[0][1] <= w[1][1]));
    assert_eq!(front.calibration.first().map(|p| p[0]), Some(22.0), "a stop off the 5 % grid is measured too");
    let gpu = m.get(&Key::nvml_fan("GPU-5a1d3090-0000-4000-8000-00000000mock", 1)).unwrap();
    assert_eq!((gpu.calibration.first(), gpu.calibration.last()), (Some(&[30.0, 600.0]), Some(&[100.0, 1820.0])));
}

#[test]
fn builtin_scenarios_and_their_config_load() {
    for name in sim::BUILTIN {
        let (sc, config) = sim::load(name).unwrap();
        assert_eq!(&sc.name, name);
        let config = penna_core::config::Config::parse(&config).unwrap();
        assert_eq!(config.validate(), []);
    }
    assert!(sim::load("nope").is_err());
}
