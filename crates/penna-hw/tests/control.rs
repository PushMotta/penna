//! The write half of the backends and the simulator, against a mock desk built in a temporary directory (DESIGN.md §6.1, §9.1).
use penna_core::held::HeldEntry;
use penna_core::{ControlKind, ControlMode, Key, Sample};
use penna_hw::sim::{self, Sim};
use penna_hw::{Machine, Options, held, hwmon::Hwmon};
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
    let machine =
        Machine::open(&Options { sysfs_root: sim.root().into(), nvml: false, mock_nvml: Some(sim.mock_nvml_root()), control: true })
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
}

#[test]
fn the_real_sys_is_refused() {
    assert!(Hwmon::controlling(Path::new("/sys")).is_err());
    let err = Machine::open(&Options { control: true, ..Options::default() }).err().expect("refused");
    assert!(err.to_string().contains("real /sys"), "{err}");
}

#[test]
fn read_only_hwmon_refuses_writes() {
    let (sim, _) = desk("readonly", "render-burst");
    let mut m = Machine::open(&Options { sysfs_root: sim.root().into(), nvml: false, mock_nvml: None, control: false }).unwrap();
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

#[test]
fn the_simulator_knows_what_pairing_would_find() {
    let (sim, _) = desk("measured", "render-burst");
    let m = sim.measured();
    let front = m.get(&pwm(2)).unwrap();
    assert_eq!(front.tachs.len(), 3);
    assert_eq!((front.stop, front.start), (Some(22.0), Some(32.0)));
    assert_eq!(m.get(&pwm(4)).unwrap().tachs, [Key::hwmon("it87", "it87.2608", "fan6")]);
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
