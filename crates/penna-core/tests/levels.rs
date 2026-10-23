//! A stepped control in the loop (DESIGN.md §3.5): a ThinkPad's 8 levels, rounded up from the curve's duty, read back as levels,
//! and written every tick when the firmware has a watchdog.
mod common;
use common::*;
use penna_core::config::{Measured, MeasuredControl};
use penna_core::control::{Command, State};
use penna_core::model::level_duty;

/// Level 1 turns the fan and level 2 starts it: what calibration finds on a ThinkPad (§5.4).
fn levels_measured() -> Measured {
    Measured {
        controls: vec![MeasuredControl { stop: Some(level_duty(8, 1)), start: Some(level_duty(8, 2)), ..MeasuredControl::new(pwm()) }],
        ..Measured::default()
    }
}

fn written(cmds: &[Command]) -> Option<u32> {
    cmds.iter().find_map(|c| match c {
        Command::Write { key, raw } | Command::Reassert { key, raw } if *key == pwm() => Some(*raw),
        _ => None,
    })
}

#[test]
fn the_curve_is_rounded_up_to_a_level_and_shown_as_that_level() {
    let mut rig = stepped(false, &levels_measured());
    rig.set(&cpu(), 65.0);
    rig.run(60);
    // The curve gives 40 % at 65 °C; level 2 is 28.6 %, level 3 is 42.9 %.
    assert_eq!(rig.hw[&pwm()].raw, 3);
    assert_eq!(rig.state(&pwm()), State::Curve);
    let c = &rig.plan.controls[&pwm()];
    assert!((c.given(rig.duty(&pwm())) - level_duty(8, 3)).abs() < 1e-9, "the protocol shows the level's duty");
    assert_eq!(c.floor, 25.0, "the configuration's minimum");
}

#[test]
fn a_level_holds_without_a_margin() {
    let kind = penna_core::ControlKind::Levels { steps: 8, refresh: false };
    let cfg = penna_core::config::ControlConfig::defaults(pwm());
    let floor = |m: &Measured| penna_core::plan::control_plan(&cfg, Some((kind, "")), m).0.floor;
    assert!((floor(&levels_measured()) - level_duty(8, 1)).abs() < 1e-9, "the lowest level that turns, not it plus 5 %");
    assert!((floor(&Measured::default()) - level_duty(8, 1)).abs() < 1e-9, "uncalibrated: the first level above off");
}

#[test]
fn without_a_watchdog_a_level_is_written_once() {
    let mut rig = stepped(false, &levels_measured());
    rig.set(&cpu(), 88.0);
    rig.run(60);
    assert_eq!(rig.hw[&pwm()].raw, 7);
    rig.step();
    assert!(rig.last.is_empty(), "steady: {:?}", rig.last);
}

#[test]
fn with_a_watchdog_the_level_is_written_every_tick() {
    let mut rig = stepped(true, &levels_measured());
    rig.set(&cpu(), 88.0);
    rig.run(60);
    for _ in 0..5 {
        let cmds = rig.step();
        assert_eq!(written(&cmds), Some(7), "the watchdog never fires while Penna runs: {cmds:?}");
    }
}

#[test]
fn a_level_the_firmware_changes_is_reasserted_then_contested() {
    let mut rig = stepped(false, &levels_measured());
    rig.set(&cpu(), 65.0);
    rig.run(60);
    let mut reasserts = 0;
    for _ in 0..20 {
        rig.hw.get_mut(&pwm()).unwrap().raw = 6; // the embedded controller picks its own level
        let cmds = rig.step();
        reasserts += cmds.iter().filter(|c| matches!(c, Command::Reassert { .. })).count();
    }
    assert_eq!(reasserts, 3);
    assert_eq!(rig.state(&pwm()), State::Contested);
    assert!(
        rig.st.controls[&pwm()].reason.as_deref().is_some_and(|r| r.contains("read back level 6")),
        "{:?}",
        rig.st.controls[&pwm()].reason
    );
}

#[test]
fn an_emergency_runs_the_top_level() {
    let mut rig = stepped(false, &levels_measured());
    rig.run(5);
    rig.set(&cpu(), 95.0);
    rig.step();
    assert_eq!((rig.state(&pwm()), rig.hw[&pwm()].raw), (State::Emergency, 7));
}
