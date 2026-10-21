//! Properties of the control loop over random curves, limits, temperatures, failures and requests (DESIGN.md §9.2).
mod common;
use common::*;
use penna_core::Sample;
use penna_core::control::{Command, Request, Scope, State};
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct Setup {
    points: Vec<(f64, f64)>,
    min: f64,
    stop: f64,
    start: f64,
    may_stop: bool,
    gpu_may_stop: bool,
    step_up: f64,
    step_down: f64,
    failsafe: f64,
}

#[derive(Clone, Debug)]
struct Tick {
    cpu: Option<f64>,
    gpu: Option<f64>,
    request: u8,
    duty: f64,
}

fn setup() -> impl Strategy<Value = Setup> {
    (
        prop::collection::vec((20.0..100.0f64, 0.0..100.0f64), 2..6),
        0.0..40.0f64,
        10.0..30.0f64,
        0.0..30.0f64,
        any::<bool>(),
        any::<bool>(),
        1.0..50.0f64,
        1.0..50.0f64,
        50.0..100.0f64,
    )
        .prop_map(|(raw, min, stop, extra, may_stop, gpu_may_stop, step_up, step_down, failsafe)| {
            let mut temps: Vec<f64> = raw.iter().map(|p| p.0.round()).collect();
            temps.sort_by(f64::total_cmp);
            temps.dedup();
            let mut pcts: Vec<f64> = raw.iter().map(|p| p.1.round()).collect();
            pcts.sort_by(f64::total_cmp);
            let mut points: Vec<(f64, f64)> = temps.into_iter().zip(pcts).collect();
            if points.len() < 2 {
                points = vec![(40.0, 20.0), (80.0, 90.0)];
            }
            Setup {
                points,
                min: min.round(),
                stop: stop.round(),
                start: (stop + 5.0 + extra).round(),
                may_stop,
                gpu_may_stop,
                step_up,
                step_down,
                failsafe: failsafe.round(),
            }
        })
}

fn ticks() -> impl Strategy<Value = Vec<Tick>> {
    prop::collection::vec(
        (prop::option::weighted(0.9, 15.0..105.0f64), prop::option::weighted(0.9, 15.0..100.0f64), 0u8..30, 0.0..100.0f64)
            .prop_map(|(cpu, gpu, request, duty)| Tick { cpu, gpu, request, duty }),
        1..80,
    )
}

fn config(s: &Setup) -> String {
    let pts: Vec<String> = s.points.iter().map(|(t, p)| format!("[{t}, {p}]")).collect();
    CONFIG
        .replace("[[45, 30], [65, 40], [78, 65], [88, 100]]", &format!("[{}]", pts.join(", ")))
        .replace(
            "label = \"CPU cooler\"\nmin = 25",
            &format!(
                "label = \"CPU cooler\"\nmin = {}\nmay_stop = {}\nstep_up = {}\nstep_down = {}\nfailsafe = {}",
                s.min, s.may_stop, s.step_up, s.step_down, s.failsafe
            ),
        )
        .replace("label = \"GPU fan 1\"\nmay_stop = true", &format!("label = \"GPU fan 1\"\nmay_stop = {}", s.gpu_may_stop))
}

fn apply(rig: &mut Rig, t: &Tick) {
    let plan = rig.plan.clone();
    let now = rig.now;
    let r = match t.request {
        0 => Some(Request::FullSpeed(true)),
        1 => Some(Request::FullSpeed(false)),
        2 => Some(Request::Override { scope: Scope::Control(pwm()), duty: Some(t.duty), seconds: Some(5.0) }),
        3 => Some(Request::Override { scope: Scope::All, duty: Some(t.duty), seconds: None }),
        4 => Some(Request::Override { scope: Scope::All, duty: None, seconds: None }),
        5 => Some(Request::Profile("boost".into())),
        6 => Some(Request::Profile("firmware".into())),
        7 | 8 => Some(Request::Profile("balanced".into())),
        _ => None,
    };
    if let Some(r) = r {
        rig.st.apply(&plan, r, now).unwrap();
    }
    match t.cpu {
        Some(c) => rig.set(&cpu(), c),
        None => {
            rig.temps.insert(cpu(), Sample::Error("Input/output error".into()));
        }
    }
    match t.gpu {
        Some(g) => rig.set(&gpu_temp(), g),
        None => {
            rig.temps.insert(gpu_temp(), Sample::Error("GPU is lost".into()));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn output_stays_within_limits_or_a_declared_safety_state(s in setup(), ts in ticks()) {
        let mut rig = Rig::new(plan(&config(&s), &measured(s.stop, s.start)));
        let c = rig.plan.controls[&pwm()].clone();
        for t in &ts {
            apply(&mut rig, t);
            let cmds = rig.step();
            let cs = &rig.st.controls[&pwm()];
            match cs.state {
                State::Curve => {
                    prop_assert!(cs.duty <= c.max.max(c.start) + 1e-9, "curve duty {} above max {}", cs.duty, c.max);
                }
                State::Override | State::Failsafe | State::Emergency => prop_assert!(cs.duty <= 100.0 + 1e-9),
                State::Firmware | State::Absent => {}
                other => prop_assert!(false, "unexpected state {other:?} with cooperative hardware"),
            }
            let just_taken = cmds.iter().any(|c| matches!(c, Command::Take { key } if key == &pwm()));
            if cs.held && !just_taken {
                if s.may_stop {
                    prop_assert!(cs.duty == 0.0 || cs.duty >= s.stop - 1e-9, "duty {} inside the stall zone below {}", cs.duty, s.stop);
                } else {
                    prop_assert!(cs.duty >= c.floor - 1e-9 && c.floor >= s.stop, "duty {} below floor {}", cs.duty, c.floor);
                }
            }
            for cmd in &cmds {
                match cmd {
                    Command::Write { key, raw } | Command::Reassert { key, raw } if key == &gpu_fan() => prop_assert!((30..=100).contains(raw)),
                    Command::Write { key, raw } | Command::Reassert { key, raw } if key == &pwm() => {
                        let stop_raw = (s.stop * 2.55).round() as u32;
                        prop_assert!(*raw <= 255);
                        if s.may_stop { prop_assert!(*raw == 0 || *raw + 1 >= stop_raw, "wrote {raw} below stop {stop_raw}"); }
                        else { prop_assert!(*raw + 1 >= (c.floor * 2.55).round() as u32, "wrote {raw} below the floor"); }
                    }
                    _ => {}
                }
            }
            prop_assert_ne!(rig.state(&gpu_fan()), State::Fault, "never wrote the GPU out of range");
        }
    }

    #[test]
    fn a_failed_read_reaches_failsafe_within_one_tick(s in setup(), ts in ticks()) {
        let mut rig = Rig::new(plan(&config(&s), &measured(s.stop, s.start)));
        for t in &ts {
            apply(&mut rig, t);
            let held_before = rig.st.controls.get(&pwm()).is_some_and(|c| c.held);
            rig.step();
            let adopted = rig.plan.assignment(&rig.st.profile, &pwm()).is_some();
            if t.cpu.is_none() && adopted && held_before {
                let cs = &rig.st.controls[&pwm()];
                prop_assert!(matches!(cs.state, State::Failsafe | State::Emergency), "{:?} after a failed read", cs.state);
                prop_assert!(cs.duty >= s.failsafe - 1e-9);
            }
        }
    }

    #[test]
    fn an_emergency_is_always_full_speed(s in setup(), ts in ticks()) {
        let mut rig = Rig::new(plan(&config(&s), &measured(s.stop, s.start)));
        for t in &ts {
            apply(&mut rig, t);
            rig.step();
            if rig.st.emergency.is_empty() {
                continue;
            }
            for key in [pwm(), gpu_fan()] {
                let cs = &rig.st.controls[&key];
                if cs.held && rig.plan.assignment(&rig.st.profile, &key).is_some() {
                    prop_assert_eq!(cs.state, State::Emergency);
                    prop_assert_eq!(cs.duty, 100.0);
                    let raw = rig.hw[&key].raw;
                    prop_assert_eq!(raw, if key == pwm() { 255 } else { 100 });
                }
            }
        }
    }

    #[test]
    fn a_contested_control_is_written_only_at_full_in_an_emergency(s in setup(), ts in ticks(), flip_from in 3usize..20) {
        let mut rig = Rig::new(plan(&config(&s), &measured(s.stop, s.start)));
        for (i, t) in ts.iter().enumerate() {
            apply(&mut rig, t);
            if i >= flip_from {
                rig.hw.get_mut(&pwm()).unwrap().mode = penna_core::ControlMode::Automatic(2);
            }
            let contested = rig.state(&pwm()) == State::Contested;
            let cmds = rig.step();
            if contested {
                let mine: Vec<_> = cmds.iter().filter(|c| c.key() == &pwm()).collect();
                if rig.st.emergency.is_empty() {
                    prop_assert!(mine.is_empty(), "{cmds:?}");
                } else {
                    prop_assert!(mine.iter().all(|c| matches!(c, Command::Reassert { raw: 255, .. })), "{cmds:?}");
                }
            }
        }
    }
}
