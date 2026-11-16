//! The kit end to end, natively, on a tiny two-seat world.

include!("support/swing_world.rs");

use arena_kit::testing::{self, Coverage, NoOp, Policy, Random};

/// Pump energy in: push the way the arm is already turning.
fn pump(_: &Declaration, _: u32, input: &[Values]) -> Option<Vec<Values>> {
    let speed = input[0].get(1)?;
    Some(vec![Values::F32(vec![if speed >= 0.0 {
        1.0
    } else {
        -1.0
    }])])
}

#[test]
fn the_declaration_is_complete() {
    assert_eq!(
        testing::declaration_problems::<Swing>(),
        Vec::<String>::new()
    );
}

#[test]
fn bounds_match_the_model() {
    let sim = MjSim::load(MODEL).unwrap();
    assert_eq!(
        testing::bounds_match_ctrlrange(&Swing::declaration(), "torque", &sim, &["motor"]),
        Vec::<String>::new()
    );
    assert_eq!(
        testing::bounds_match_ctrlrange(&Swing::declaration(), "torque", &sim, &["nope"]).len(),
        1
    );
}

#[test]
fn sessions_run_to_a_result_with_everything_declared_emitted() {
    let declaration = Swing::declaration();
    let mut coverage = Coverage::default();
    for seed in 0..4 {
        let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(pump), Box::new(Random::new(seed))];
        let played = testing::play::<Swing>(seed, &mut policies).unwrap();
        assert_eq!(played.turns, declaration.max_turns);
        assert_eq!(played.finish.stats.len(), 1);
        coverage.add(&played);
    }
    assert_eq!(coverage.problems, Vec::<String>::new());
    assert_eq!(coverage.missing(&declaration), Vec::<String>::new());
}

#[test]
fn the_scripted_bot_beats_no_op_and_random() {
    let against_noop =
        testing::win_rate::<Swing>(0..10, |_| vec![Box::new(pump), Box::new(NoOp)]).unwrap();
    let against_random = testing::win_rate::<Swing>(0..10, |seed| {
        vec![Box::new(pump), Box::new(Random::new(seed))]
    })
    .unwrap();
    assert!(against_noop > 0.9, "{against_noop}");
    assert!(against_random > 0.7, "{against_random}");
}

#[test]
fn bad_output_bytes_are_no_action_never_a_trap() {
    let (mut session, _) = arena_kit::session::Session::<Swing>::setup(&[1], 2).unwrap();
    let turn = session.step(&[Some(b"garbage".to_vec()), None]);
    assert_eq!(turn.inputs.len(), 2);
    assert!(arena_kit::session::Session::<Swing>::setup(&[1], 3).is_err());
}
