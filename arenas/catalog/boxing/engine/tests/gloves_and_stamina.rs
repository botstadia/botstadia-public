//! Boxing's gloves and stamina: punches land from either glove, the start
//! leaves the gloves apart, and the motors heat, derate and cool as the
//! design note says. The standing check is the hard gate: a fighter that
//! only stands must never lose torque.

mod support;

use arena_kit::testing::{self, NoOp, Policy};
use arena_kit::{Report, Seed, Values, World};
use boxing_declaration::*;
use boxing_engine::{Arena, UP_HEIGHT};
use support::Scripted;

fn arena(seed: u64) -> (Arena, Report) {
    let mut report = Report::new(&declaration(), 2);
    let arena = Arena::setup(&Seed::from_u64(seed), 2, &mut report).unwrap();
    (arena, report)
}

fn step(arena: &mut Arena, report: &mut Report, red: Option<&[f32]>, blue: Option<&[f32]>) {
    report.next_turn();
    let out = |t: Option<&[f32]>| t.map(|t| vec![Values::F32(t.to_vec())]);
    arena.step(&[out(red), out(blue)], report);
}

fn max(values: impl IntoIterator<Item = f64>) -> f64 {
    values.into_iter().fold(0.0, f64::max)
}

#[test]
fn either_glove_lands_a_punch() {
    for side in 0..2 {
        let mut policies: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::one_handed(side)), Box::new(NoOp)];
        let played = testing::play::<Arena>(4, &mut policies).unwrap();
        let index = declaration()
            .stats
            .iter()
            .position(|s| s.key == POINTS.0)
            .unwrap();
        let points = &played.finish.stats[index];
        assert!(points[0] > 0.0, "the {side} glove never landed: {points:?}");
    }
}

#[test]
fn the_gloves_start_apart() {
    for seed in 0..20 {
        let (arena, _) = arena(seed);
        let sim = arena.sim();
        let (red, blue) = (arena.seat_bodies(0), arena.seat_bodies(1));
        let touching: Vec<_> = sim
            .contacts()
            .into_iter()
            .filter(|c| {
                (red.contains(&c.body1) && blue.contains(&c.body2))
                    || (blue.contains(&c.body1) && red.contains(&c.body2))
            })
            .collect();
        assert!(
            touching.is_empty(),
            "seed {seed}: the fighters touch at the start: {touching:?}"
        );
        for r in ["red_left_glove", "red_right_glove"] {
            for b in ["blue_left_glove", "blue_right_glove"] {
                let (p, q) = (
                    sim.body_pos(sim.body_id(r).unwrap()),
                    sim.body_pos(sim.body_id(b).unwrap()),
                );
                let gap = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2))
                    .sqrt()
                    - 0.11;
                assert!(gap > 0.05, "seed {seed}: {r} and {b} are {gap:.3} m apart");
            }
        }
    }
}

#[test]
fn the_gloves_are_part_of_the_fighter() {
    // A glove on the canvas is a knockdown like any other body part, so the
    // fighter's bodies run from its pelvis to its right glove.
    let (arena, _) = arena(1);
    for (seat, prefix) in SEATS.iter().enumerate() {
        let bodies = arena.seat_bodies(seat);
        for side in ["left", "right"] {
            let glove = arena
                .sim()
                .body_id(&format!("{prefix}_{side}_glove"))
                .unwrap();
            assert!(
                bodies.contains(&glove),
                "{prefix}'s {side} glove isn't one of its bodies"
            );
        }
        assert_eq!(
            arena.sim().body_name(bodies.end - 1).unwrap(),
            format!("{prefix}_right_glove")
        );
    }
}

/// Bang-bang on both wrists at their bounds, every fifth of a second: the
/// wrists' 5 N·m motors saturate at once, and with a slight lean back the
/// fighter stays up.
fn flurry(turn: u32) -> [f32; 29] {
    let mut targets = START_TARGETS;
    targets[14] = -0.1; // the scripted bot's lean back
    let out = (turn / 10).is_multiple_of(2);
    for motor in [19, 20, 21, 26, 27, 28] {
        let (lo, hi) = JOINT_RANGES[motor];
        targets[motor] = if out { lo } else { hi };
    }
    targets
}

#[test]
fn a_derated_motor_never_exceeds_its_limit() {
    let (mut arena, mut report) = arena(2);
    let mut heat = [0.0; 29];
    for (motor, h) in heat.iter_mut().enumerate() {
        *h = 1.0 + (motor as f64 % 5.0) * 0.25; // from the knee to the cut-off
    }
    arena.set_heats(0, heat);
    for turn in 0..200 {
        // The limits in force through this turn are the ones set after the last.
        let limits = arena.torques(0).map(|(limit, _)| limit);
        let heats = arena.heats(0);
        step(&mut arena, &mut report, Some(&flurry(turn)), None);
        for (motor, (_, torque)) in arena.torques(0).into_iter().enumerate() {
            let limit = limits[motor];
            let fraction = f64::from(torque_fraction(heats[motor] as f32));
            assert!(
                torque.abs() <= limit + 1e-6,
                "turn {turn}, motor {motor}: {torque:.2} N·m over its limit {limit:.2}"
            );
            assert!(limit <= f64::from(PEAK_TORQUES[motor]) * fraction + 1e-6);
        }
    }
}

#[test]
fn a_flurry_derates_the_arms_and_rest_restores_them() {
    let (mut arena, mut report) = arena(3);
    // Eight seconds of flurry: a saturated motor passes the knee in about
    // 0.118 τc, 4.7 s.
    for turn in 0..400 {
        step(&mut arena, &mut report, Some(&flurry(turn)), None);
    }
    assert!(
        arena.pelvis_heights()[0] > UP_HEIGHT,
        "the flurry toppled the fighter"
    );
    let arms = || 15..29;
    let hot = max(arms().map(|m| arena.heats(0)[m]));
    assert!(
        hot > f64::from(HEAT_KNEE),
        "the flurry only heated the arms to {hot:.2}"
    );
    let derated = arms().any(|m| {
        let (limit, _) = arena.torques(0)[m];
        limit < f64::from(PEAK_TORQUES[m]) - 1e-6
    });
    assert!(derated, "no arm motor lost torque after the flurry");
    // Stamina weighs each limb's motors by their rated torque, so hot wrists
    // (the arm's smallest motors) show, but only a little.
    assert!(
        arena.stamina_of(0) < 0.95,
        "stamina {} after the flurry",
        arena.stamina_of(0)
    );
    // A minute in the stance.
    for _ in 0..3000 {
        step(&mut arena, &mut report, Some(&START_TARGETS), None);
    }
    let cooled = max(arms().map(|m| arena.heats(0)[m]));
    assert!(
        cooled < f64::from(HEAT_KNEE),
        "still {cooled:.2} after a minute's rest"
    );
    for m in arms() {
        let (limit, _) = arena.torques(0)[m];
        assert!(
            (limit - f64::from(PEAK_TORQUES[m])).abs() < 1e-6,
            "motor {m} still derated"
        );
    }
}

/// The hard gate: standing, in the stance or a high guard, never loses torque.
#[test]
fn a_fighter_that_only_stands_never_loses_torque() {
    // The guard: both gloves held up at the chest, with a slight lean back.
    // (A guard held out far enough to press the opponent's gloves is pushing,
    // not standing, and the check below would catch it.)
    let mut guard = START_TARGETS;
    guard[14] = -0.1;
    for (pitch, roll, elbow, out) in [(15, 16, 18, 0.3), (22, 23, 25, -0.3)] {
        guard[pitch] = -0.4;
        guard[roll] = out;
        guard[elbow] = 2.0;
    }
    for (name, pose) in [("stance", START_TARGETS), ("guard", guard)] {
        let (mut arena, mut report) = arena(5);
        let mut hottest: f64 = 0.0;
        for turn in 0..MAX_TURNS {
            step(&mut arena, &mut report, Some(&pose), Some(&pose));
            for seat in 0..2 {
                hottest = hottest.max(max(arena.heats(seat)));
            }
            let (sim, red, blue) = (arena.sim(), arena.seat_bodies(0), arena.seat_bodies(1));
            assert!(
                !sim.contacts()
                    .iter()
                    .any(|c| (red.contains(&c.body1) && blue.contains(&c.body2))
                        || (blue.contains(&c.body1) && red.contains(&c.body2))),
                "{name}: the fighters touch at turn {turn}, so this isn't only standing"
            );
        }
        let heights = arena.pelvis_heights();
        assert!(
            heights.iter().all(|h| *h > UP_HEIGHT),
            "{name}: a fighter fell: {heights:?}"
        );
        assert!(
            hottest < f64::from(HEAT_KNEE),
            "{name}: a motor reached heat {hottest:.3}, past the knee: standing must never lose torque"
        );
        println!("{name}: hottest motor {hottest:.3} after the whole bout");
    }
}

#[test]
fn a_bout_of_punching_leaves_the_legs_under_the_knee() {
    let (mut session, _) =
        arena_kit::session::Session::<Arena>::setup(&6u64.to_le_bytes(), 2).unwrap();
    let mut scripted = Scripted::new();
    let mut legs: f64 = 0.0;
    for _ in 0..MAX_TURNS {
        let declaration = session.declaration().clone();
        let input = session.world().input(0);
        let red = scripted.act(&declaration, 0, &input);
        session.step_values(&[red, None]);
        legs = legs.max(max(session.world().heats(0)[..12].iter().copied()));
        if session.finished().is_some() {
            break;
        }
    }
    assert!(
        legs < f64::from(HEAT_KNEE),
        "the legs reached heat {legs:.3}"
    );
    println!("a bout of punching: hottest leg motor {legs:.3}");
}
