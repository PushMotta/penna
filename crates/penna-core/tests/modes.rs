//! A fan with modes in the loop (DESIGN.md §3.5): held in its firmware's automatic mode, switched to full speed by Full speed and
//! emergencies, and read back as the mode it is in.
mod common;
use common::*;
use penna_core::control::{Command, Request, State};

#[test]
fn a_curve_below_full_speed_is_automatic_and_full_speed_is_full() {
    let mut rig = with_modes();
    rig.run(5);
    assert_eq!((rig.state(&pwm()), rig.hw[&pwm()].raw), (State::Curve, 0), "35 % is the firmware's automatic mode");
    assert_eq!(rig.plan.controls[&pwm()].given(rig.duty(&pwm())), 0.0);
    rig.st.apply(&rig.plan.clone(), Request::FullSpeed(true), rig.now).unwrap();
    rig.step();
    assert_eq!((rig.state(&pwm()), rig.hw[&pwm()].raw), (State::Override, 1));
    rig.st.apply(&rig.plan.clone(), Request::FullSpeed(false), rig.now).unwrap();
    rig.run(30);
    assert_eq!((rig.state(&pwm()), rig.hw[&pwm()].raw), (State::Curve, 0), "automatic again");
}

#[test]
fn an_emergency_is_full_speed() {
    let mut rig = with_modes();
    rig.run(5);
    rig.set(&cpu(), 95.0);
    rig.step();
    assert_eq!((rig.state(&pwm()), rig.hw[&pwm()].raw), (State::Emergency, 1));
}

#[test]
fn a_mode_changed_under_penna_is_reasserted() {
    let mut rig = with_modes();
    rig.run(5);
    rig.hw.get_mut(&pwm()).unwrap().raw = 1; // someone switched it to full speed
    let mut cmds = Vec::new();
    for _ in 0..3 {
        cmds.extend(rig.step());
    }
    assert!(cmds.contains(&Command::Reassert { key: pwm(), raw: 0 }), "{cmds:?}");
    assert_eq!(rig.hw[&pwm()].raw, 0);
}
