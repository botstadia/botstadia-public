//! The checks every arena's engine makes (the arena authoring guide's steps 4
//! and 5), plus dance-off's own: the mirrored start, falls and stand-ups, the
//! draw.

mod support;

use arena_kit::declaration::Declaration;
use arena_kit::mujoco::MjSim;
use arena_kit::testing::{self, Coverage, NoOp, Policy, Random, Spread};
use arena_kit::{Values, World};
use dance_off_declaration::*;
use dance_off_engine::{Arena, MODEL};
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
fn every_declared_series_and_event_is_emitted_and_nothing_undeclared() {
    let declaration = Arena::declaration();
    let mut coverage = Coverage::default();
    for seed in 0..2 {
        let mut policies: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::new()), Box::new(Faller::new())];
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
        let mut policies: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::new()), Box::new(Random::new(seed))];
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
    }
    assert_eq!(coverage.problems, Vec::<String>::new());
    assert_eq!(coverage.missing(&declaration), Vec::<String>::new());
}

#[test]
fn inputs_are_egocentric() {
    // The start is symmetric, and both dancers see the same routine, so both
    // seats see the same input at turn 0, and keep seeing the same while they
    // act the same.
    let (mut session, _) = arena_kit::session::Session::<Arena>::setup(&[7], 2).unwrap();
    let same = |a: &[Values], b: &[Values]| {
        a.iter().zip(b).all(|(x, y)| {
            (0..x.len()).all(|i| (x.get(i).unwrap() - y.get(i).unwrap()).abs() < 0.02)
        })
    };
    assert!(same(&session.world().input(0), &session.world().input(1)));
    let mut scripted = [Scripted::new(), Scripted::new()];
    for _ in 0..400 {
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

/// Seat 0's share of wins over `seeds`, with each session's outcome for the message.
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
            "seed {seed}: {:?}, {}",
            played.finish.places, played.finish.decided_by
        ));
    }
    let count = (seeds.end - seeds.start).max(1) as f64;
    (f64::from(won) / count, outcomes)
}

#[test]
fn the_scripted_baseline_beats_no_op_and_random() {
    let (rate, outcomes) = wins(0..6, |_| vec![Box::new(Scripted::new()), Box::new(NoOp)]);
    assert!(
        rate > 0.9,
        "scripted beat no-op in {rate} of sessions:\n{}",
        outcomes.join("\n")
    );
    let (rate, outcomes) = wins(0..6, |seed| {
        vec![Box::new(Scripted::new()), Box::new(Random::new(seed))]
    });
    assert!(
        rate > 0.9,
        "scripted beat random in {rate} of sessions:\n{}",
        outcomes.join("\n")
    );
}

#[test]
fn outcomes_spread() {
    // Two scripted dancers, each with its own small wobble on its targets:
    // timing and falls decide some sessions one way and some the other.
    let mut spread = Spread::default();
    for seed in 0..8 {
        let mut policies: Vec<Box<dyn Policy>> = vec![
            Box::new(Scripted::noisy(seed, 0.05)),
            Box::new(Scripted::noisy(seed + 100, 0.05)),
        ];
        spread.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
    }
    assert!(
        spread.winners.len() > 1,
        "the same seat always wins: {:?}",
        spread.winners
    );
}

#[test]
fn a_dancer_that_keeps_falling_is_stood_back_up_each_time() {
    let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(Faller::new()), Box::new(NoOp)];
    let played = testing::play::<Arena>(1, &mut policies).unwrap();
    assert_eq!(played.turns, MAX_TURNS);
    let index = declaration()
        .stats
        .iter()
        .position(|s| s.key == FALLS.0)
        .unwrap();
    let falls = &played.finish.stats[index];
    // Down 2 s, then a second standing, then down again: about 25 in 90 s.
    assert!(falls[0] >= 20.0, "fell {} times", falls[0]);
    assert_eq!(falls[1], 0.0);
    assert_eq!(played.finish.places, vec![vec![0, 1]]);
}

#[test]
fn two_dancers_who_only_stand_draw() {
    let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(NoOp), Box::new(NoOp)];
    let played = testing::play::<Arena>(3, &mut policies).unwrap();
    assert_eq!(played.turns, MAX_TURNS);
    assert_eq!(played.finish.places, vec![vec![0, 1]]);
    assert_eq!(played.finish.decided_by, "A draw, 0 to 0");
}

#[test]
fn test_bots_never_link_the_engine() {
    // The scripted baseline and the faller read only the declaration crate.
    let _: fn(&Declaration, u32, &[Values]) -> Option<Vec<Values>> = support::stand;
}

#[test]
fn the_scripted_dancer_scores_at_both_ends_of_the_tempo_range() {
    let hits = Arena::declaration().stat_index(HITS.0).unwrap();
    for bpm in [BPM_MIN, BPM_MAX] {
        let seeds: Vec<u64> = (0..5000)
            .filter(|s| dance_off_engine::tempo(&arena_kit::Seed::from_u64(*s)) == bpm)
            .take(3)
            .collect();
        assert_eq!(seeds.len(), 3, "no seeds at {bpm} BPM");
        for seed in seeds {
            let cards = dance_off_engine::routine(&arena_kit::Seed::from_u64(seed))
                .1
                .len();
            let mut policies: Vec<Box<dyn Policy>> =
                vec![Box::new(Scripted::new()), Box::new(NoOp)];
            let played = testing::play::<Arena>(seed, &mut policies).unwrap();
            let hit = played.finish.stats[hits][0];
            assert!(
                hit >= 0.6 * cards as f64,
                "{bpm} BPM, seed {seed}: {hit} of {cards} cards"
            );
            assert_eq!(played.finish.places, vec![vec![0], vec![1]]);
        }
    }
}
