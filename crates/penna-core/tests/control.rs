//! The control loop on simulated time against a fake desk (DESIGN.md §4, §6.3, §6.4).
mod common;
use common::*;
use penna_core::control::{Command, Request, Scope, State, Trend};
use penna_core::{ControlMode, Sample};

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
    // falls once past the band, in steps of the band: held at f(≈75 °C) ≈ 59 %, above f(72 °C) = 53.5 %
    assert!((53.5..60.0).contains(&d) && d < at_78 - 5.0, "{d}");
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
