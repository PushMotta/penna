//! A fan whose firmware runs a curve itself, in the loop (DESIGN.md §3.5): the assigned graph is fitted to the firmware's points
//! and written whole when it changes, a flat curve stands for an emergency or an override, and a firmware that goes back to its
//! own curve is given Penna's again.
mod common;
use common::*;
use penna_core::control::{Command, Request, State};
use penna_core::plan::fit;

fn curves(cmds: &[Command]) -> Vec<Vec<[f64; 2]>> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Curve { key, points } if *key == pwm() => Some(points.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_graph_is_fitted_to_the_firmware_s_points_and_written_once() {
    // The `cpu` graph has 4 points; the firmware takes 5: the widest gap, 45–65 °C, is split at 55.
    assert_eq!(
        fit(&[[45.0, 30.0], [65.0, 40.0], [78.0, 65.0], [88.0, 100.0]], 5),
        [[45.0, 30.0], [55.0, 35.0], [65.0, 40.0], [78.0, 65.0], [88.0, 100.0]]
    );
    let mut rig = with_firmware_curve();
    rig.step();
    let cmds = rig.step();
    assert_eq!(curves(&cmds), [[[45.0, 30.0], [55.0, 35.0], [65.0, 40.0], [78.0, 65.0], [88.0, 100.0]]], "{cmds:?}");
    assert_eq!(rig.state(&pwm()), State::Curve);
    rig.set(&cpu(), 70.0);
    for _ in 0..30 {
        let cmds = rig.step();
        assert!(curves(&cmds).is_empty(), "the firmware runs it: nothing to write as the temperature moves: {cmds:?}");
    }
    assert!(rig.duty(&pwm()) > 40.0, "the tick still says what the curve gives now: {}", rig.duty(&pwm()));
}

#[test]
fn more_points_than_the_firmware_takes_are_read_evenly() {
    let fitted = fit(&[[40.0, 20.0], [50.0, 30.0], [60.0, 45.0], [70.0, 60.0], [75.0, 70.0], [80.0, 85.0], [90.0, 100.0]], 5);
    // Read at 52.5, 65, 77.5 °C, then rounded to whole degrees and percent.
    assert_eq!(fitted, [[40.0, 20.0], [53.0, 34.0], [65.0, 53.0], [78.0, 78.0], [90.0, 100.0]]);
}

#[test]
fn an_emergency_and_full_speed_are_flat_curves() {
    let mut rig = with_firmware_curve();
    rig.run(3);
    rig.st.apply(&rig.plan.clone(), Request::FullSpeed(true), rig.now).unwrap();
    let cmds = rig.step();
    assert_eq!(curves(&cmds), [[[45.0, 100.0], [55.0, 100.0], [65.0, 100.0], [78.0, 100.0], [88.0, 100.0]]]);
    rig.st.apply(&rig.plan.clone(), Request::FullSpeed(false), rig.now).unwrap();
    let cmds = rig.step();
    assert_eq!(curves(&cmds)[0][0], [45.0, 30.0], "its own curve again");
    rig.set(&cpu(), 95.0);
    let cmds = rig.step();
    assert_eq!((rig.state(&pwm()), curves(&cmds)[0][0][1]), (State::Emergency, 100.0));
}

#[test]
fn a_firmware_that_goes_back_to_its_own_curve_is_given_penna_s_again() {
    let mut rig = with_firmware_curve();
    rig.run(3);
    let mut rewrites = 0;
    for _ in 0..20 {
        rig.hw.get_mut(&pwm()).unwrap().mode = penna_core::ControlMode::Automatic(2); // a platform profile change switched it off
        rewrites += curves(&rig.step()).len();
    }
    assert_eq!(rewrites, 3);
    assert_eq!(rig.state(&pwm()), State::Contested);
}

#[test]
fn a_mix_or_a_virtual_source_is_refused() {
    let mut inv = inventory();
    for c in inv.devices.iter_mut().flat_map(|d| &mut d.controls).filter(|c| c.key == pwm()) {
        c.kind = penna_core::ControlKind::FirmwareCurve { points: 5 };
    }
    let plan = |text: &str| penna_core::plan::Plan::new(penna_core::config::Config::parse(text).unwrap(), &inv, &Default::default(), None);
    let boost = CONFIG.replace("\"hwmon/it87/it87.2608/pwm1\" = \"cpu\"", "\"hwmon/it87/it87.2608/pwm1\" = \"case\"");
    let err = plan(&boost).unwrap_err();
    assert!(err.iter().any(|p| p.message.starts_with("its firmware runs one curve")), "{err:?}");
    assert!(plan(CONFIG).is_err(), "the boost profile gives it a Trigger");
}
