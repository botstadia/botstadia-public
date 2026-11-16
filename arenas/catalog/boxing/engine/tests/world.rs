//! The checks every arena's engine makes (the arena authoring guide's step 4
//! and 5), plus boxing's own: the mirrored start, the ten count and knockout.

mod support;

use arena_kit::declaration::Declaration;
use arena_kit::mujoco::MjSim;
use arena_kit::testing::{self, Coverage, NoOp, Policy, Random, Spread};
use arena_kit::{Values, World};
use boxing_declaration::*;
use boxing_engine::{Arena, MODEL};
use support::{Faller, Scripted};

#[test]
fn the_declaration_documents_everything() {
    assert_eq!(
        testing::declaration_problems::<Arena>(),
        Vec::<String>::new()
    );
}

#[test]
fn action_bounds_equal_the_models_ctrlrange() {
    let sim = MjSim::load(MODEL).unwrap();
    let declaration = Arena::declaration();
    for seat in SEATS {
        let names: Vec<String> = JOINTS
            .iter()
            .map(|joint| format!("{seat}_{joint}_joint"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            testing::bounds_match_ctrlrange(&declaration, OUT_TARGETS, &sim, &refs),
            Vec::<String>::new()
        );
    }
}

#[test]
fn peak_torques_are_the_models_torque_limits() {
    let sim = MjSim::load(MODEL).unwrap();
    for seat in SEATS {
        for (motor, joint) in JOINTS.iter().enumerate() {
            let id = sim.joint_id(&format!("{seat}_{joint}_joint")).unwrap();
            let peak = f64::from(PEAK_TORQUES[motor]);
            assert_eq!(
                sim.joint_actfrcrange(id),
                Some((-peak, peak)),
                "{seat} {joint}"
            );
        }
    }
}

#[test]
fn every_declared_series_and_event_is_emitted_and_nothing_undeclared() {
    let declaration = Arena::declaration();
    let mut coverage = Coverage::default();
    for seed in 0..4 {
        // A boxer against a random flailer: punches, a knockdown and a knockout.
        let mut policies: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::new()), Box::new(Random::new(seed))];
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
        // Two boxers: points, and a time or a knockdown ending.
        let mut policies: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::new()), Box::new(Scripted::new())];
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
        // A boxer on a fighter that only stands: committed straights, power
        // punches among them (the reference drill the bands come from).
        let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(Scripted::new()), Box::new(NoOp)];
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
    }
    assert_eq!(coverage.problems, Vec::<String>::new());
    // `up` (beating the count) is the one event no test policy reaches: no fixed
    // sequence of joint targets stands a fallen G1 up. The engine's own unit
    // tests cover it by setting the count state.
    assert_eq!(
        coverage.missing(&declaration),
        vec!["event `up`".to_string()]
    );
}

#[test]
fn inputs_are_egocentric() {
    // The start is symmetric under a half turn of the ring, so both seats see the
    // same input at turn 0, and keep seeing the same while they act the same.
    let (mut session, _) = arena_kit::session::Session::<Arena>::setup(&[7], 2).unwrap();
    let same = |a: &[Values], b: &[Values]| {
        a.iter().zip(b).all(|(x, y)| {
            (0..x.len()).all(|i| (x.get(i).unwrap() - y.get(i).unwrap()).abs() < 0.02)
        })
    };
    assert!(same(&session.world().input(0), &session.world().input(1)));
    let mut scripted = [Scripted::new(), Scripted::new()];
    for _ in 0..100 {
        let declaration = session.declaration().clone();
        let inputs = [session.world().input(0), session.world().input(1)];
        let outputs: Vec<Option<Vec<Values>>> = (0..2)
            .map(|seat| scripted[seat].act(&declaration, seat as u32, &inputs[seat]))
            .collect();
        session.step_values(&outputs);
    }
    assert!(
        same(&session.world().input(0), &session.world().input(1)),
        "seats diverged while acting alike"
    );
}

/// Seat 0's share of wins over `seeds`, with each bout's outcome for the message.
fn wins(
    seeds: std::ops::Range<u64>,
    make: impl Fn(u64) -> Vec<Box<dyn Policy>>,
) -> (f64, Vec<String>) {
    let mut outcomes = Vec::new();
    let mut won = 0;
    for seed in seeds.clone() {
        let mut policies = make(seed);
        let played = testing::play::<Arena>(seed, &mut policies).unwrap();
        if played.finish.places.first() == Some(&vec![0]) {
            won += 1;
        }
        outcomes.push(format!(
            "seed {seed}: {:?} after {} turns, {}",
            played.finish.places, played.turns, played.finish.decided_by
        ));
    }
    let count = (seeds.end - seeds.start).max(1) as f64;
    (f64::from(won) / count, outcomes)
}

#[test]
fn the_scripted_baseline_beats_no_op_and_random() {
    let (rate, outcomes) = wins(0..10, |_| vec![Box::new(Scripted::new()), Box::new(NoOp)]);
    assert!(
        rate > 0.9,
        "scripted beat no-op in {rate} of bouts:\n{}",
        outcomes.join("\n")
    );
    let (rate, outcomes) = wins(0..20, |seed| {
        vec![Box::new(Scripted::new()), Box::new(Random::new(seed))]
    });
    assert!(
        rate > 0.9,
        "scripted beat random in {rate} of bouts:\n{}",
        outcomes.join("\n")
    );
}

#[test]
fn outcomes_spread() {
    let mut spread = Spread::default();
    for seed in 0..10 {
        let mut policies: Vec<Box<dyn Policy>> = vec![
            Box::new(Random::new(seed)),
            Box::new(Random::new(seed + 100)),
        ];
        spread.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
    }
    assert!(
        spread.winners.len() > 1,
        "random play always has the same winner: {:?}",
        spread.winners
    );
}

#[test]
fn a_fighter_down_for_ten_is_knocked_out() {
    let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(Faller::new()), Box::new(NoOp)];
    let played = testing::play::<Arena>(1, &mut policies).unwrap();
    assert_eq!(played.finish.places, vec![vec![1], vec![0]]);
    assert_eq!(played.finish.decided_by, "Knockout: didn't beat the count");
    assert!(played.turns < 1000, "took {} turns", played.turns);
}

#[test]
fn two_fighters_who_only_stand_draw_on_points() {
    let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(NoOp), Box::new(NoOp)];
    let played = testing::play::<Arena>(3, &mut policies).unwrap();
    assert_eq!(played.turns, MAX_TURNS);
    assert_eq!(played.finish.places, vec![vec![0, 1]]);
    assert_eq!(played.finish.decided_by, "A draw on points, 0 to 0");
}

#[test]
fn test_bots_never_link_the_engine() {
    // The scripted baseline and the faller read only the declaration crate.
    let _: fn(&Declaration, u32, &[Values]) -> Option<Vec<Values>> = support::stand;
}
