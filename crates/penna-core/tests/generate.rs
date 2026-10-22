//! Generated curves (DESIGN.md §5.5) from this desk's own measurements: setup's fourth run, 22 Oct 2026.
use penna_core::config::{Config, CurveKind, Measured, MixOp};
use penna_core::control::graph;
use penna_core::generate::{generate, monotonic};
use penna_core::plan::Plan;
use penna_core::{ControlInfo, ControlKind, Device, Inventory, Key, Limit, LimitKind, SensorInfo, Source};

const IT8688: (&str, &str) = ("it8688_4008090b", "it87.2624");
const IT8792: (&str, &str) = ("it8792_4008090b", "it87.2656");
const UUID: &str = "GPU-5a1d3090-0000-4000-8000-00000000mock";
const CPU_MODEL: &str = "AMD Ryzen 9 5950X 16-Core Processor";

fn key((driver, device): (&str, &str), channel: &str) -> Key {
    Key::hwmon(driver, device, channel)
}

fn sensor(key: Key, label: &str, limits: Vec<Limit>) -> SensorInfo {
    SensorInfo { key, label: label.into(), limits, ignored_limits: vec![], below_ambient: false }
}

fn device(id: String, source: Source, driver: &str, sensors: Vec<SensorInfo>, controls: Vec<ControlInfo>) -> Device {
    Device {
        id,
        source,
        driver: driver.into(),
        label: driver.into(),
        location: String::new(),
        model: None,
        sensors,
        tachs: vec![],
        controls,
    }
}

fn pwms(chip: (&str, &str), headers: &[&str]) -> Vec<ControlInfo> {
    headers
        .iter()
        .enumerate()
        .map(|(i, h)| ControlInfo { key: key(chip, &format!("pwm{}", i + 1)), label: (*h).into(), kind: ControlKind::Duty { scale: 255 } })
        .collect()
}

/// This desk as discovery sees it, with the board database's names.
fn desk() -> Inventory {
    let cpu = key(("k10temp", "0000:00:18.3"), "temp1");
    let limit = |kind, celsius| Limit { kind, celsius };
    let gpu_temp = sensor(Key::nvml_temp(UUID), "GPU", vec![limit(LimitKind::GpuMax, 93.0), limit(LimitKind::Acoustic, 83.0)]);
    let gpu_fans = (0..2)
        .map(|f| ControlInfo {
            key: Key::nvml_fan(UUID, f),
            label: format!("Fan {}", f + 1),
            kind: ControlKind::Nvml { min: 30, max: 100 },
        })
        .collect();
    Inventory {
        devices: vec![
            device("hwmon/k10temp/0000:00:18.3".into(), Source::Hwmon, "k10temp", vec![sensor(cpu, "Tctl", vec![])], vec![]),
            device(format!("nvml/{UUID}"), Source::Nvml, "nvml", vec![gpu_temp], gpu_fans),
            device(
                "hwmon/it8688_4008090b/it87.2624".into(),
                Source::Hwmon,
                "it8688_4008090b",
                vec![sensor(key(IT8688, "temp3"), "CPU", vec![]), sensor(key(IT8688, "temp6"), "PCH", vec![])],
                pwms(IT8688, &["CPU_FAN", "SYS_FAN1", "SYS_FAN2", "PCH_FAN", "CPU_OPT"]),
            ),
            device(
                "hwmon/it8792_4008090b/it87.2656".into(),
                Source::Hwmon,
                "it8792_4008090b",
                vec![],
                pwms(IT8792, &["SYS_FAN5_PUMP", "SYS_FAN6_PUMP", "SYS_FAN4"]),
            ),
        ],
        ..Inventory::default()
    }
}

fn measured() -> Measured {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test/fixtures/measured/x570-desk.toml")).unwrap();
    Measured::parse(&text).unwrap()
}

/// A fresh install's configuration on this desk: what pennad writes on its first start.
fn starter() -> Config {
    penna_core::starter::starter(&desk())
}

fn points(cfg: &Config, id: &str) -> Vec<[f64; 2]> {
    match &cfg.curve(id).unwrap_or_else(|| panic!("no curve {id}")).kind {
        CurveKind::Graph { points, .. } => points.clone(),
        other => panic!("{id} is {other:?}"),
    }
}

#[test]
fn this_desk_gets_a_curve_per_role_and_profile() {
    let m = measured();
    let g = generate(&starter(), &desk(), &m, Some(CPU_MODEL));
    assert!(g.notes.is_empty(), "{:?}", g.notes);
    assert!(g.config.validate().is_empty(), "{:?}", g.config.validate());
    Plan::new(g.config.clone(), &desk(), &m, Some(CPU_MODEL)).expect("the draft plans");
    assert_eq!(g.config.profile, "defaults", "the draft leaves Defaults on until it is saved");
    let ids = |prefix: &str| g.curves.iter().filter(|c| c.starts_with(prefix)).cloned().collect::<Vec<_>>();
    assert_eq!(ids("cpu-"), ["cpu-quiet", "cpu-balanced", "cpu-render"]);
    assert_eq!(ids("gpu-"), ["gpu-quiet", "gpu-balanced", "gpu-render"]);
    assert_eq!(ids("pump-"), ["pump-quiet", "pump-balanced", "pump-render"]);
    assert!(g.curves.contains(&"chipset".to_owned()) && g.curves.contains(&"case-balanced".to_owned()), "{:?}", g.curves);
    assert_eq!(g.config.curve("cpu-balanced").unwrap().label.as_deref(), Some("CPU · Balanced"));

    // Every fan in every profile, with its role's curve.
    let assigned = |profile: &str, k: &Key| g.config.profiles[profile].get(k).cloned();
    for (p, suffix) in [("quiet", "quiet"), ("balanced", "balanced"), ("render", "render")] {
        assert_eq!(assigned(p, &key(IT8688, "pwm1")), Some(format!("cpu-{suffix}")));
        assert_eq!(assigned(p, &key(IT8688, "pwm5")), Some(format!("pump-{suffix}")), "CPU_OPT carries the pump");
        assert_eq!(assigned(p, &key(IT8688, "pwm4")), Some("chipset".into()));
        assert_eq!(assigned(p, &Key::nvml_fan(UUID, 1)), Some(format!("gpu-{suffix}")));
        for case in [key(IT8688, "pwm2"), key(IT8688, "pwm3"), key(IT8792, "pwm1"), key(IT8792, "pwm3")] {
            assert_eq!(assigned(p, &case), Some(format!("case-{suffix}")), "{case}");
        }
        assert_eq!(assigned(p, &key(IT8792, "pwm2")), None, "SYS_FAN6_PUMP is empty");
    }

    // The anchors: Tctl idled at 42 °C and the 5950X's TjMax is 90; the 3090 idled at 43, aims at 83 and tops out at 93.
    let cpu = &g.anchors[&key(("k10temp", "0000:00:18.3"), "temp1")];
    assert_eq!((cpu.idle, cpu.limit), (42.0, Some(90.0)));
    let gpu = &g.anchors[&Key::nvml_temp(UUID)];
    assert_eq!((gpu.idle, gpu.target, gpu.limit), (43.0, Some(83.0), Some(93.0)));

    // Balanced CPU: the floor to idle + 10, 50 % of CPU_FAN's airflow at 70 °C, 100 % at 82 °C. Its floor is its stop and the
    // margin, 15 %; its airflow is half way between its floor and top speed at the middle point.
    let cpu = points(&g.config, "cpu-balanced");
    assert_eq!((cpu[0], cpu[2][0], cpu[4]), ([52.0, 15.0], 70.0, [82.0, 100.0]), "{cpu:?}");
    let table = monotonic(&m.get(&key(IT8688, "pwm1")).unwrap().calibration);
    let (lo, hi) = (graph(&table, 15.0), graph(&table, 100.0));
    let half = (graph(&table, cpu[2][1]) - lo) / (hi - lo);
    assert!((half - 0.5).abs() < 0.04, "50 % airflow at {} % duty: {half:.2}", cpu[2][1]);
    assert_eq!(points(&g.config, "cpu-quiet")[0][0], 57.0, "Quiet holds the floor to idle + 15");
    assert_eq!(points(&g.config, "cpu-render").last().unwrap()[0], 75.0, "Render reaches 100 % at the limit less 15");

    // Balanced GPU: the driver's curve below 68 °C, then the 3090's measured floor of 45 %, 50 % at 78 °C and 100 % at 88 °C.
    let gpu = points(&g.config, "gpu-balanced");
    assert_eq!((gpu[0], gpu[1], gpu[3][0], gpu.last().unwrap()[0]), ([67.0, 0.0], [68.0, 45.0], 78.0, 88.0), "{gpu:?}");
    let fan = g.config.control(&Key::nvml_fan(UUID, 0)).unwrap();
    assert!(fan.may_stop && fan.label.as_deref() == Some("GPU fan 1"), "{fan:?}");

    // Case fans: the larger of a CPU and a GPU curve, from 15 °C above idle, capped at 75 % of their airflow in Balanced.
    let CurveKind::Mix { op: MixOp::Max, curves } = &g.config.curve("case-balanced").unwrap().kind else { panic!() };
    assert_eq!(curves, &["case-balanced-cpu", "case-balanced-gpu"]);
    let case_cpu = points(&g.config, "case-balanced-cpu");
    assert_eq!((case_cpu[0][0], case_cpu.last().unwrap()[0]), (57.0, 82.0), "{case_cpu:?}");
    assert_eq!(case_cpu[0][1], 5.0, "the lowest floor among them: SYS_FAN1's 5 %; the plan lifts each to its own");
    let quiet_top = points(&g.config, "case-quiet-cpu").last().unwrap()[1];
    let render_top = points(&g.config, "case-render-cpu").last().unwrap()[1];
    assert!(quiet_top < case_cpu.last().unwrap()[1] && case_cpu.last().unwrap()[1] < render_top, "caps rise by profile");
    assert_eq!(points(&g.config, "case-balanced-gpu")[0][0], 58.0);

    // SYS_FAN4 turns no faster above 70 %: that is its top, and its maximum in the draft.
    assert_eq!(g.config.control(&key(IT8792, "pwm3")).unwrap().max, 70.0);
    assert_eq!(g.config.control(&key(IT8688, "pwm1")).unwrap().max, 100.0);

    // The pump runs flat; PCH_FAN may stop, as its BIOS stops it, and rests below about 64 °C.
    assert_eq!(g.config.curve("pump-balanced").unwrap().kind, CurveKind::Flat { percent: 85.0 });
    let pch = g.config.control(&key(IT8688, "pwm4")).unwrap();
    assert!(pch.may_stop);
    let chipset = points(&g.config, "chipset");
    assert_eq!((chipset[0], chipset[1]), ([60.0, 0.0], [65.0, 10.0]), "{chipset:?}");
    assert!(graph(&chipset, 63.0) < 10.0 && graph(&chipset, 65.0) >= 10.0);
}

#[test]
fn a_saved_curve_is_replaced_only_in_the_draft_and_others_stay() {
    let m = measured();
    let mut base = starter();
    base.curves.push(penna_core::config::Curve {
        id: "mine".into(),
        label: None,
        kind: CurveKind::Flat { percent: 40.0 },
        response: None,
        hysteresis: None,
        hysteresis_direction: Default::default(),
    });
    let g = generate(&base, &desk(), &m, Some(CPU_MODEL));
    assert!(g.config.curve("mine").is_some(), "a curve setup didn't make stays");
    assert!(g.config.curve("gpu").is_some(), "and so does the starter's GPU curve, now unused");
    assert_ne!(g.config.curve("cpu-quiet"), base.curve("cpu-quiet"), "the starter's cpu-quiet is replaced");
    let again = generate(&g.config, &desk(), &m, Some(CPU_MODEL));
    assert_eq!(again.config, g.config, "generating again from the draft changes nothing");
}

#[test]
fn a_fan_without_a_role_or_a_source_is_left_with_its_firmware_and_said_why() {
    let mut m = measured();
    let sys4 = m.controls.iter_mut().find(|c| c.key == key(IT8792, "pwm3")).unwrap();
    sys4.role = None;
    sys4.suggested = None;
    let mut inv = desk();
    inv.devices[2].sensors.retain(|s| s.label != "PCH");
    let g = generate(&starter(), &inv, &m, Some(CPU_MODEL));
    assert_eq!(
        g.notes,
        [
            "SYS_FAN4: no role, so it stays with its firmware",
            "PCH_FAN: the board has no chipset temperature, so it stays with its firmware"
        ]
    );
    assert!(!g.config.profiles["balanced"].contains_key(&key(IT8792, "pwm3")));
    assert!(!g.config.profiles["balanced"].contains_key(&key(IT8688, "pwm4")));
    assert!(g.config.validate().is_empty());
}

#[test]
fn a_table_is_made_monotonic() {
    let t = [[65.0, 1642.0], [70.0, 1717.0], [75.0, 1670.0], [80.0, 1713.0], [100.0, 1726.0]];
    assert_eq!(monotonic(&t), [[65.0, 1642.0], [70.0, 1717.0], [75.0, 1717.0], [80.0, 1717.0], [100.0, 1726.0]]);
}

#[test]
fn the_load_check_moves_the_100_percent_point_earlier() {
    let mut g = generate(&starter(), &desk(), &measured(), Some(CPU_MODEL)).config;
    penna_core::generate::earlier(&mut g, "cpu-balanced", 5.0);
    let p = points(&g, "cpu-balanced");
    assert_eq!(p.last().unwrap(), &[77.0, 100.0], "82 °C less 5");
    penna_core::generate::earlier(&mut g, "cpu-balanced", 50.0);
    let q = points(&g, "cpu-balanced");
    assert_eq!(q.last().unwrap()[0], q[q.len() - 2][0] + 1.0, "never at or before the point below");
}
