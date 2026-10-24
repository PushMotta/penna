//! The control loop on simulated time against a fake desk (DESIGN.md §4, §6.3, §6.4).
mod common;
use common::*;
use penna_core::control::{Command, Request, Scope, SourceValue, State, Trend};
use penna_core::{ControlMode, Key, Sample};

#[test]
fn first_tick_takes_every_adopted_control_and_nothing_else() {
    let mut rig = Rig::standard();
    let cmds = rig.step();
    assert_eq!(cmds, [Command::Take { key: pwm() }, Command::Take { key: gpu_fan() }]);
    assert!(rig.st.controls[&pwm()].held);
}

#[test]
fn adoption_is_bumpless() {
    let mut rig = Rig::standard();
    rig.hw.get_mut(&pwm()).unwrap().raw = 102; // the firmware was running 40 %
    rig.step();
    assert!((rig.duty(&pwm()) - 40.0).abs() < 0.01);
    rig.set(&cpu(), 45.0);
    let cmds = rig.step();
    // the curve says 30 %; it falls from 40 at 5 %/s
    assert!(cmds.contains(&Command::Write { key: pwm(), raw: 90 }), "35 % rounds up to 90: {cmds:?}");
    assert_eq!(rig.st.controls[&pwm()].trend, Trend::Falling);
}

#[test]
fn rises_at_the_step_limit_and_follows_the_smoothed_curve() {
    let mut rig = Rig::standard();
    rig.hw.get_mut(&pwm()).unwrap().raw = 77; // 30 %
    rig.run(3);
    rig.set(&cpu(), 87.0);
    let mut last = rig.duty(&pwm());
    for _ in 0..4 {
        rig.step();
        let d = rig.duty(&pwm());
        assert!(d - last <= 20.0 + 1e-9, "{last} → {d}");
        last = d;
    }
    rig.run(40);
    assert!((rig.duty(&pwm()) - 96.5).abs() < 0.5, "settles on the curve: {}", rig.duty(&pwm()));
    assert_eq!(rig.state(&pwm()), State::Curve);
}

#[test]
fn hysteresis_holds_a_falling_output() {
    let mut rig = Rig::standard();
    rig.set(&cpu(), 78.0);
    rig.run(40);
    let at_78 = rig.duty(&pwm());
    assert!((at_78 - 65.0).abs() < 0.5, "{at_78}");
    rig.set(&cpu(), 76.0);
    rig.run(40);
    assert!(rig.st.curves["cpu"].held, "a 2 °C drop is inside the 3 °C band");
    assert!((rig.duty(&pwm()) - at_78).abs() < 0.5);
    rig.set(&cpu(), 72.0);
    rig.run(40);
    let d = rig.duty(&pwm());
    // falls once past the band, and then the band's width behind the curve: f(75 °C) ≈ 59.2 %, above f(72 °C) = 53.5 %
    assert!((d - 59.2).abs() < 0.2, "{d}");
}

/// Both ways (an option), the output moves in steps: only once the temperature has moved the band's width, and then to the curve.
#[test]
fn two_way_hysteresis_moves_in_steps_of_the_band() {
    let both = CONFIG.replace("response = \"5s\"\nhysteresis = 3.0", "response = 0\nhysteresis = 3.0\nhysteresis_direction = \"both\"");
    assert_ne!(both, CONFIG);
    let mut rig = Rig::new(plan(&both, &measured(20.0, 30.0)));
    let f = |t: f64| penna_core::control::graph(&[[45.0, 30.0], [65.0, 40.0], [78.0, 65.0], [88.0, 100.0]], t);
    rig.run(3);
    for (t, want) in [(72.0, f(72.0)), (74.0, f(72.0)), (77.0, f(77.0)), (75.0, f(77.0)), (70.0, f(70.0))] {
        rig.set(&cpu(), t);
        rig.run(3);
        let out = rig.st.curves["cpu"].out.unwrap();
        assert!((out - want).abs() < 0.01, "at {t} °C: {out}, where {want} was due");
    }
}

/// The load check on the development desk (§5.5): under a steady full load, with a steep CPU curve and Tctl's jitter, the fan
/// settles. Dropping the band's worth at once when the band let go made it hunt between 45 and 57 % every 30 s.
#[test]
fn a_steady_load_settles_the_fan_instead_of_hunting() {
    let steep = CONFIG.replace("[[45, 30], [65, 40], [78, 65], [88, 100]]", "[[60, 15], [65, 40], [70, 59], [76, 79], [82, 100]]");
    let mut rig = Rig::new(plan(&steep, &measured(20.0, 30.0)));
    // The desk under full load: Tctl heads for 119 °C less 0.96 °C per % of airflow, with a 5 s lag; the fan follows its duty
    // with a 2 s lag; every reading jitters by up to 1.5 °C either way.
    let (mut t, mut air, mut seed) = (45.0_f64, 30.0_f64, 1_u64);
    let mut settled = Vec::new();
    for i in 0..900 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let jitter = ((seed >> 33) as f64 / (1_u64 << 31) as f64 - 0.5) * 3.0;
        t += (119.0 - 0.96 * air - t) * (1.0 - (-1.0_f64 / 5.0).exp());
        rig.set(&cpu(), t + jitter);
        rig.step();
        air += (rig.duty(&pwm()) - air) * (1.0 - (-1.0_f64 / 2.0).exp());
        if i >= 300 {
            settled.push(rig.duty(&pwm()));
        }
    }
    let (lo, hi) = settled.iter().fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    assert!(hi - lo < 3.0, "the fan hunts between {lo:.1} and {hi:.1} %");
}

#[test]
fn a_failed_read_is_failsafe_on_the_same_tick() {
    let mut rig = Rig::standard();
    rig.run(5);
    rig.temps.insert(cpu(), Sample::Error("No such file or directory".into()));
    let cmds = rig.step();
    assert_eq!(rig.state(&pwm()), State::Failsafe);
    assert!(cmds.contains(&Command::Write { key: pwm(), raw: 255 }));
    assert!(rig.st.controls[&pwm()].reason.as_deref().unwrap().contains("No such file"));
    rig.set(&cpu(), 45.0);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Curve);
    assert!(rig.duty(&pwm()) > 90.0, "it comes down at the step rate, not at once");
}

#[test]
fn an_implausible_reading_is_a_failed_read() {
    let mut rig = Rig::standard();
    rig.run(3);
    rig.temps.insert(cpu(), Sample::Implausible(4000.0));
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Failsafe);
}

#[test]
fn an_emergency_puts_every_fan_at_full_until_five_degrees_below() {
    let mut rig = Rig::standard();
    rig.run(5);
    rig.set(&cpu(), 90.5); // TjMax for the 5950X is 90 °C
    let cmds = rig.step();
    assert_eq!(rig.state(&pwm()), State::Emergency);
    assert_eq!(rig.state(&gpu_fan()), State::Emergency);
    assert!(cmds.contains(&Command::Write { key: pwm(), raw: 255 }));
    assert!(cmds.contains(&Command::Write { key: gpu_fan(), raw: 100 }));
    rig.set(&cpu(), 86.0);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Emergency, "86 °C is not yet 5 °C below 90");
    rig.set(&cpu(), 84.9);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Curve);
    rig.temps.insert(cpu(), Sample::Error("gone".into()));
    rig.set(&gpu_temp(), 94.0);
    rig.step();
    assert_eq!(rig.state(&gpu_fan()), State::Emergency, "the GPU's own limit is 93 °C");
}

#[test]
fn the_gpu_hands_back_below_its_floor_and_resumes_above_it() {
    let mut rig = Rig::standard();
    rig.set(&gpu_temp(), 38.0);
    rig.run(3);
    assert_eq!(rig.state(&gpu_fan()), State::Handback);
    assert_eq!(rig.hw[&gpu_fan()].mode, ControlMode::Driver, "taking an NVML fan does not switch it to manual");
    assert!(!rig.log.iter().any(|(_, c)| matches!(c, Command::Write { key, .. } if key == &gpu_fan())));
    rig.set(&gpu_temp(), 56.0); // curve 36.7 %, above 30 + 5
    rig.step();
    assert_eq!(rig.state(&gpu_fan()), State::Curve);
    assert_eq!(rig.hw[&gpu_fan()].mode, ControlMode::Manual);
    rig.set(&gpu_temp(), 54.0); // 31.5 %: above the floor, below floor + margin: stays manual
    rig.run(2);
    assert_eq!(rig.state(&gpu_fan()), State::Curve);
    rig.set(&gpu_temp(), 50.0); // 17.5 % with 2 °C of hysteresis: falls once past it
    rig.run(3);
    assert_eq!(rig.state(&gpu_fan()), State::Handback);
    assert_eq!(rig.hw[&gpu_fan()].mode, ControlMode::Driver);
    assert!(rig.hw[&gpu_fan()].raw >= 30, "never wrote below the floor");
}

#[test]
fn a_mode_that_keeps_flipping_is_reasserted_three_times_then_contested() {
    let mut rig = Rig::standard();
    rig.run(5);
    let mut reasserts = 0;
    for _ in 0..20 {
        rig.hw.get_mut(&pwm()).unwrap().mode = ControlMode::Automatic(2);
        let cmds = rig.step();
        reasserts += cmds.iter().filter(|c| matches!(c, Command::Reassert { key, .. } if key == &pwm())).count();
        if rig.state(&pwm()) == State::Contested {
            break;
        }
    }
    assert_eq!(reasserts, 3);
    assert_eq!(rig.state(&pwm()), State::Contested);
    assert!(rig.st.controls[&pwm()].reason.as_deref().unwrap().contains("stopped writing"));
    rig.set(&cpu(), 80.0);
    for _ in 0..5 {
        assert!(!rig.step().iter().any(|c| c.key() == &pwm()), "a contested control is never written");
    }
    rig.st.apply(&rig.plan.clone(), Request::Retry(pwm()), rig.now).unwrap();
    let cmds = rig.step();
    assert!(cmds.iter().any(|c| matches!(c, Command::Reassert { key, .. } if key == &pwm())), "{cmds:?}");
}

#[test]
fn an_emergency_outranks_a_contest_then_the_contest_resumes() {
    let mut rig = Rig::standard();
    rig.run(5);
    for _ in 0..20 {
        rig.hw.get_mut(&pwm()).unwrap().mode = ControlMode::Automatic(2);
        rig.step();
        if rig.state(&pwm()) == State::Contested {
            break;
        }
    }
    assert_eq!(rig.state(&pwm()), State::Contested);
    rig.set(&cpu(), 91.0);
    for _ in 0..3 {
        rig.hw.get_mut(&pwm()).unwrap().mode = ControlMode::Automatic(2);
        let cmds = rig.step();
        assert!(cmds.contains(&Command::Reassert { key: pwm(), raw: 255 }), "{cmds:?}");
        assert_eq!(rig.state(&pwm()), State::Emergency);
    }
    rig.set(&cpu(), 80.0);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Contested);
    assert!(rig.st.controls[&pwm()].reason.as_deref().unwrap().contains("stopped writing"));
    assert!(!rig.step().iter().any(|c| c.key() == &pwm()), "contested again: no writes");
}

#[test]
fn a_resume_reset_is_reasserted_once_and_forgotten() {
    let mut rig = Rig::standard();
    rig.run(5);
    rig.hw.get_mut(&pwm()).unwrap().mode = ControlMode::Automatic(2);
    rig.hw.get_mut(&pwm()).unwrap().raw = 200;
    rig.run(4);
    let reasserts = rig.log.iter().filter(|(_, c)| matches!(c, Command::Reassert { .. })).count();
    assert_eq!(reasserts, 1);
    assert_eq!(rig.state(&pwm()), State::Curve);
    assert_eq!(rig.hw[&pwm()].mode, ControlMode::Manual);
}

#[test]
fn waking_from_sleep_writes_every_driven_fan_at_once_and_counts_nothing() {
    let mut rig = Rig::standard();
    rig.set(&gpu_temp(), 60.0);
    rig.run(5);
    assert_eq!((rig.state(&pwm()), rig.state(&gpu_fan())), (State::Curve, State::Curve));
    // The firmware took both back while the machine slept. A tick after waking writes them, before read-back has looked.
    rig.hw.get_mut(&pwm()).unwrap().mode = ControlMode::Automatic(2);
    rig.hw.get_mut(&gpu_fan()).unwrap().mode = ControlMode::Driver;
    rig.st.woke();
    let cmds = rig.step();
    for key in [pwm(), gpu_fan()] {
        assert!(cmds.iter().any(|c| matches!(c, Command::Reassert { key: k, .. } if k == &key)), "{key}: {cmds:?}");
        assert!(rig.st.controls[&key].reasserts.is_empty(), "{key}: sleep is not a contest");
    }
    assert_eq!((rig.hw[&pwm()].mode, rig.hw[&gpu_fan()].mode), (ControlMode::Manual, ControlMode::Manual));
    let before = rig.log.len();
    rig.run(3);
    assert!(!rig.log[before..].iter().any(|(_, c)| matches!(c, Command::Reassert { .. })), "{:?}", &rig.log[before..]);
}

#[test]
fn waking_from_sleep_leaves_a_contested_fan_and_a_handed_back_one_alone() {
    let mut rig = Rig::standard();
    rig.run(5);
    assert_eq!(rig.state(&gpu_fan()), State::Handback, "a cool GPU runs on the driver's curve");
    for _ in 0..20 {
        rig.hw.get_mut(&pwm()).unwrap().mode = ControlMode::Automatic(2);
        rig.step();
        if rig.state(&pwm()) == State::Contested {
            break;
        }
    }
    assert_eq!(rig.state(&pwm()), State::Contested);
    rig.st.woke();
    let cmds = rig.step();
    assert!(cmds.is_empty(), "{cmds:?}");
    assert_eq!(rig.state(&pwm()), State::Contested);
}

#[test]
fn a_value_overwritten_by_someone_else_counts_too() {
    let mut rig = Rig::standard();
    rig.run(5);
    for _ in 0..12 {
        rig.hw.get_mut(&pwm()).unwrap().raw = 255;
        rig.step();
    }
    assert_eq!(rig.state(&pwm()), State::Contested);
}

#[test]
fn a_failed_write_is_handed_back_and_faulted() {
    let mut rig = Rig::standard();
    rig.run(3);
    rig.hw.get_mut(&pwm()).unwrap().fail_writes = true;
    rig.set(&cpu(), 80.0);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Fault);
    assert!(!rig.st.controls[&pwm()].held);
    assert_eq!(rig.hw[&pwm()].mode, ControlMode::Automatic(2), "the original mode, exactly");
    assert!(rig.st.controls[&pwm()].reason.as_deref().unwrap().contains("write failed"));
    rig.run(3);
    assert!(!rig.last.iter().any(|c| c.key() == &pwm()), "nothing more until a retry");
}

#[test]
fn a_lost_gpu_faults_even_when_it_cannot_be_handed_back() {
    let mut rig = Rig::standard();
    rig.set(&gpu_temp(), 60.0);
    rig.run(4);
    assert_eq!(rig.state(&gpu_fan()), State::Curve);
    rig.hw.get_mut(&gpu_fan()).unwrap().unreadable = true;
    rig.temps.insert(gpu_temp(), Sample::Error("GPU is lost".into()));
    rig.step();
    assert_eq!(rig.state(&gpu_fan()), State::Fault);
    let reason = rig.st.controls[&gpu_fan()].reason.clone().unwrap();
    assert!(reason.contains("could not hand back"), "{reason}");
    assert!(rig.st.controls[&gpu_fan()].held, "still in held.json, so restore can try again");
}

#[test]
fn a_fan_that_may_not_stop_never_goes_below_its_floor() {
    let mut rig = Rig::new(plan(&CONFIG.replace("min = 25", "min = 0"), &measured(20.0, 30.0)));
    rig.hw.get_mut(&pwm()).unwrap().raw = 0;
    rig.set(&cpu(), 20.0);
    rig.run(10);
    assert!((rig.duty(&pwm()) - 30.0).abs() < 1e-9, "curve 30 %, floor 25 %: {}", rig.duty(&pwm()));
    let quiet = CONFIG
        .replace("min = 25", "min = 0")
        .replace("\"hwmon/it87/it87.2608/pwm1\" = \"cpu\"", "\"hwmon/it87/it87.2608/pwm1\" = \"quiet\"")
        .replace("percent = 35", "percent = 5");
    let mut rig = Rig::new(plan(&quiet, &measured(20.0, 30.0)));
    rig.run(10);
    assert!((rig.duty(&pwm()) - 25.0).abs() < 1e-9, "stop 20 % + 5 % margin: {}", rig.duty(&pwm()));
}

#[test]
fn a_stopped_fan_is_kicked_then_settles() {
    let cfg = CONFIG
        .replace("min = 25", "min = 0\nmay_stop = true")
        .replace("[[45, 30], [65, 40], [78, 65], [88, 100]]", "[[45, 0], [65, 22], [78, 65], [88, 100]]");
    let mut rig = Rig::new(plan(&cfg, &measured(20.0, 45.0)));
    rig.hw.get_mut(&pwm()).unwrap().raw = 0;
    rig.set(&cpu(), 40.0);
    rig.run(5);
    assert_eq!(rig.duty(&pwm()), 0.0);
    rig.set(&cpu(), 66.0);
    rig.st.curves.get_mut("cpu").unwrap().smoothed = Some(66.0);
    rig.step();
    assert!(rig.duty(&pwm()) >= 45.0, "kicked: {}", rig.duty(&pwm()));
    rig.step();
    assert!(rig.duty(&pwm()) >= 45.0, "still kicking at 1 s");
    rig.run(12);
    let d = rig.duty(&pwm());
    assert!((22.0..45.0).contains(&d), "settled near the curve: {d}");
    rig.set(&cpu(), 40.0);
    rig.run(40);
    assert_eq!(rig.duty(&pwm()), 0.0, "stops cleanly, never parked between 0 and the stop duty");
}

#[test]
fn switching_to_a_firmware_profile_releases_exactly() {
    let mut rig = Rig::standard();
    rig.run(3);
    rig.st.apply(&rig.plan.clone(), Request::Profile("firmware".into()), rig.now).unwrap();
    let cmds = rig.step();
    assert!(cmds.contains(&Command::Release { key: pwm() }));
    assert_eq!(rig.hw[&pwm()].mode, ControlMode::Automatic(2));
    assert_eq!(rig.state(&pwm()), State::Firmware);
    rig.run(3);
    assert!(rig.last.is_empty());
}

#[test]
fn timed_overrides_expire_and_full_speed_wins() {
    let mut rig = Rig::standard();
    rig.run(3);
    rig.st
        .apply(&rig.plan.clone(), Request::Override { scope: Scope::Control(pwm()), duty: Some(70.0), seconds: Some(5.0) }, rig.now)
        .unwrap();
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Override);
    assert_eq!(rig.hw[&pwm()].raw, 179, "70 % of 255 = 178.5, rounded up");
    rig.run(5);
    assert_eq!(rig.state(&pwm()), State::Curve);
    rig.st.apply(&rig.plan.clone(), Request::FullSpeed(true), rig.now).unwrap();
    rig.step();
    assert_eq!(rig.hw[&pwm()].raw, 255);
    assert_eq!(rig.hw[&gpu_fan()].raw, 100);
    assert_eq!(rig.state(&gpu_fan()), State::Override);
}

#[test]
fn a_mix_fails_when_any_input_fails() {
    let mut rig = Rig::standard();
    rig.st.apply(&rig.plan.clone(), Request::Profile("boost".into()), rig.now).unwrap();
    rig.set(&gpu_temp(), 60.0);
    rig.run(3);
    assert_eq!(rig.state(&gpu_fan()), State::Curve);
    rig.temps.insert(cpu(), Sample::Error("gone".into()));
    rig.step();
    assert_eq!(rig.state(&gpu_fan()), State::Failsafe, "case = max(cpu, gpu) and cpu is gone");
    assert_eq!(rig.state(&pwm()), State::Failsafe, "boost reads virt/avg, which reads the CPU");
}

#[test]
fn a_trigger_switches_on_its_time_average() {
    let mut rig = Rig::standard();
    rig.st.apply(&rig.plan.clone(), Request::Profile("boost".into()), rig.now).unwrap();
    rig.set(&cpu(), 45.0);
    rig.run(12);
    assert!((rig.st.outputs["boost"].clone().unwrap() - 35.0).abs() < 1e-9);
    rig.set(&cpu(), 75.0);
    rig.run(3);
    assert_eq!(rig.st.outputs["boost"].clone().unwrap(), 35.0, "the 10 s average has not reached 60 °C yet");
    rig.run(10);
    assert_eq!(rig.st.outputs["boost"].clone().unwrap(), 70.0);
    rig.set(&cpu(), 55.0);
    rig.run(12);
    assert_eq!(rig.st.outputs["boost"].clone().unwrap(), 70.0, "55 °C is between off and on: stays loaded");
    rig.set(&cpu(), 45.0);
    rig.run(12);
    assert_eq!(rig.st.outputs["boost"].clone().unwrap(), 35.0);
}

#[test]
fn a_control_missing_from_the_machine_is_absent() {
    let cfg = CONFIG.replace("[profiles.firmware]", "\"hwmon/it87/it87.2608/pwm7\" = \"cpu\"\n\n[profiles.firmware]");
    let mut rig = Rig::new(plan(&cfg, &measured(20.0, 30.0)));
    let cmds = rig.step();
    assert!(!cmds.iter().any(|c| c.key().channel() == "pwm7"));
    let absent = penna_core::Key::parse("hwmon/it87/it87.2608/pwm7").unwrap();
    assert_eq!(rig.state(&absent), State::Absent);
}

#[test]
fn may_stop_without_calibration_is_refused() {
    let cfg = penna_core::config::Config::parse(&CONFIG.replace("min = 25", "min = 25\nmay_stop = true")).unwrap();
    let err = penna_core::plan::Plan::new(cfg, &inventory(), &Default::default(), None).unwrap_err();
    assert!(err.iter().any(|p| p.message.contains("calibrated stop")), "{err:?}");
}

#[test]
fn writes_only_when_the_value_changes() {
    let mut rig = Rig::standard();
    rig.set(&cpu(), 50.0);
    rig.run(40);
    rig.step();
    assert!(rig.last.is_empty(), "steady: {:?}", rig.last);
}

/// A hybrid laptop's dGPU, runtime-suspended, can't be read and makes no heat (§4.2): a curve on it rests at its lowest point
/// instead of failing safe at 100 %, and a Mix leaves it out. When it wakes, the curve follows it again.
#[test]
fn a_gpu_asleep_rests_its_curve_and_a_mix_leaves_it_out() {
    let mut rig = Rig::standard();
    rig.st.apply(&rig.plan.clone(), Request::Profile("boost".into()), rig.now).unwrap();
    let mut cold = Rig::standard();
    cold.st.apply(&cold.plan.clone(), Request::Profile("boost".into()), cold.now).unwrap();
    for r in [&mut rig, &mut cold] {
        r.set(&cpu(), 70.0);
        r.set(&gpu_temp(), 80.0);
        r.run(20);
    }
    // Asleep on one desk, cold on the other: both run the GPU fan on the CPU's half of the Mix.
    rig.temps.insert(gpu_temp(), Sample::Asleep);
    cold.set(&gpu_temp(), 30.0);
    for _ in 0..60 {
        rig.step();
        cold.step();
        assert_eq!(rig.state(&gpu_fan()), State::Curve, "asleep is not a failed read");
    }
    assert_eq!(rig.st.sources[&gpu_temp()], SourceValue::Asleep);
    assert_eq!(rig.st.outputs["gpu"], Ok(0.0), "the gpu curve rests at its first point");
    assert_eq!(rig.st.outputs["case"], cold.st.outputs["case"]);
    assert_eq!(rig.duty(&gpu_fan()), cold.duty(&gpu_fan()));

    rig.set(&gpu_temp(), 80.0);
    rig.step();
    assert!(rig.st.outputs["gpu"].clone().unwrap() > 70.0, "awake again, the curve follows it at once");
}

/// A virtual sensor leaves out an input asleep; with every input asleep it is asleep itself, and a failed input still fails it.
#[test]
fn a_virtual_sensor_leaves_out_an_input_asleep() {
    let config = CONFIG.replace(
        "[[sensor]]\nkey = \"virt/avg\"",
        "[[sensor]]\nkey = \"virt/hottest\"\nmax = [\"hwmon/k10temp/0000:00:18.3/temp1\", \"nvml/GPU-1/temp\"]\n\n[[sensor]]\nkey = \"virt/avg\"",
    );
    let mut rig = Rig::new(plan(&config, &measured(20.0, 30.0)));
    let hottest = Key::parse("virt/hottest").unwrap();
    rig.set(&cpu(), 50.0);
    rig.temps.insert(gpu_temp(), Sample::Asleep);
    rig.step();
    assert_eq!(rig.st.sources[&hottest], SourceValue::Celsius(50.0));
    rig.temps.insert(cpu(), Sample::Asleep);
    rig.step();
    assert_eq!(rig.st.sources[&hottest], SourceValue::Asleep);
    rig.temps.insert(cpu(), Sample::Error("gone".into()));
    rig.step();
    assert!(matches!(rig.st.sources[&hottest], SourceValue::Failed(_)), "{:?}", rig.st.sources[&hottest]);
}

/// An emergency does not end because its sensor fell asleep: only a reading 5 °C below the limit ends it (§6.3).
#[test]
fn an_emergency_outlasts_its_sensor_falling_asleep() {
    let mut rig = Rig::standard();
    rig.run(3);
    rig.set(&gpu_temp(), 94.0);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Emergency);
    rig.temps.insert(gpu_temp(), Sample::Asleep);
    rig.run(5);
    assert_eq!(rig.state(&pwm()), State::Emergency);
    rig.set(&gpu_temp(), 60.0);
    rig.step();
    assert_eq!(rig.state(&pwm()), State::Curve);
}
