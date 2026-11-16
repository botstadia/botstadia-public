//! The checks every arena's engine makes (the arena authoring guide's steps 4
//! and 5), plus racing's own: crashes and respawns, the finish order of a
//! mixed field, and a six-drone session's step time.

mod support;

use arena_kit::declaration::Declaration;
use arena_kit::mujoco::MjSim;
use arena_kit::session::Session;
use arena_kit::testing::{self, Coverage, NoOp, Policy, Random, Spread};
use arena_kit::{Values, World};
use racing_declaration::*;
use racing_engine::{Arena, MODEL};
use support::{Flipper, Scripted};

#[test]
fn the_declaration_documents_everything() {
    assert_eq!(
        testing::declaration_problems::<Arena>(),
        Vec::<String>::new()
    );
}

#[test]
fn thrust_bounds_equal_every_drones_ctrlrange() {
    let sim = MjSim::load(MODEL).unwrap();
    let declaration = Arena::declaration();
    for drone in 1..=MAX_SEATS {
        let names: Vec<String> = (1..=4)
            .map(|rotor| format!("drone_{drone}_thrust{rotor}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            testing::bounds_match_ctrlrange(&declaration, OUT_THRUST, &sim, &refs),
            Vec::<String>::new()
        );
    }
}

#[test]
fn the_model_carries_six_drones_of_the_declared_mass() {
    let sim = MjSim::load(MODEL).unwrap();
    for drone in 1..=MAX_SEATS {
        let body = sim.body_id(&format!("drone_{drone}_x2")).unwrap();
        assert!((sim.body_mass(body) - f64::from(DRONE_MASS)).abs() < 1e-6);
    }
}

fn field(
    seed: u64,
    seats: usize,
    make: impl Fn(usize, u64) -> Box<dyn Policy>,
) -> Vec<Box<dyn Policy>> {
    (0..seats).map(|seat| make(seat, seed)).collect()
}

#[test]
fn every_declared_series_and_event_is_emitted_and_nothing_undeclared() {
    let declaration = Arena::declaration();
    let mut coverage = Coverage::default();
    for seed in 0..4 {
        // A racer against a random flailer: gates, laps, the finish, crashes
        // and respawns.
        let mut policies: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::new()), Box::new(Random::new(seed))];
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
        // Racers of different pace: overtakes.
        let mut policies = field(seed, 4, |seat, _| {
            Box::new(Scripted::with_cruise(2.5 + 1.5 * seat as f32)) as Box<dyn Policy>
        });
        coverage.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
    }
    assert_eq!(coverage.problems, Vec::<String>::new());
    assert_eq!(coverage.missing(&declaration), Vec::<String>::new());
}

#[test]
fn inputs_are_egocentric() {
    // Swap two drones' whole state and their inputs swap.
    let (mut session, _) = Session::<Arena>::setup(&[7], 2).unwrap();
    let close = |a: &[Values], b: &[Values]| {
        a.iter().zip(b).all(|(x, y)| {
            (0..x.len()).all(|i| (x.get(i).unwrap() - y.get(i).unwrap()).abs() < 1e-3)
        })
    };
    let (a, b) = (([-9.0, -8.5, 2.0], 0.3), ([-7.0, -7.0, 2.5], -0.4));
    // The session's world is borrowed mutably only through a step, so teleport
    // through a fresh world for each arrangement.
    let mut report = arena_kit::Report::new(&Arena::declaration(), 2);
    let mut world = Arena::setup(&arena_kit::Seed::new(vec![7]), 2, &mut report).unwrap();
    world.teleport(0, a.0, a.1);
    world.teleport(1, b.0, b.1);
    let before = [world.input(0), world.input(1)];
    world.teleport(0, b.0, b.1);
    world.teleport(1, a.0, a.1);
    let after = [world.input(0), world.input(1)];
    assert!(
        close(&before[0], &after[1]),
        "seat 0's view didn't move with its state"
    );
    assert!(
        close(&before[1], &after[0]),
        "seat 1's view didn't move with its state"
    );
    // And a session runs both seats from their own frames.
    session.step_values(&[None, None]);
}

/// Seat 0's share of outright wins over `seeds`, with each race's outcome.
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
    let (rate, outcomes) = wins(0..6, |_| vec![Box::new(Scripted::new()), Box::new(NoOp)]);
    assert!(
        rate > 0.9,
        "scripted beat no-op in {rate}:\n{}",
        outcomes.join("\n")
    );
    let (rate, outcomes) = wins(0..6, |seed| {
        field(seed, 6, |seat, seed| {
            if seat == 0 {
                Box::new(Scripted::new()) as Box<dyn Policy>
            } else {
                Box::new(Random::new(seed * 10 + seat as u64))
            }
        })
    });
    assert!(
        rate > 0.9,
        "scripted beat five random in {rate}:\n{}",
        outcomes.join("\n")
    );
}

/// The fairness gate on the courses the seed draws: the scripted bot
/// finishes three laps on 20 of 20 courses alone against a no-op, and with
/// a full field of six every race is won by a finisher.
#[test]
fn the_scripted_baseline_finishes_three_laps_on_every_course() {
    let mut outcomes = Vec::new();
    for seed in 0..20 {
        let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(Scripted::new()), Box::new(NoOp)];
        let played = testing::play::<Arena>(seed, &mut policies).unwrap();
        if !played.finish.decided_by.starts_with("First to the finish") {
            outcomes.push(format!(
                "seed {seed}: {} after {} turns",
                played.finish.decided_by, played.turns
            ));
        }
    }
    assert!(outcomes.is_empty(), "{}", outcomes.join("\n"));
}

#[test]
fn a_full_field_of_scripted_racers_finishes() {
    let mut finished = 0;
    let mut unwon = Vec::new();
    for seed in 0..20 {
        let mut policies = field(seed, 6, |_, _| Box::new(Scripted::new()) as Box<dyn Policy>);
        let played = testing::play::<Arena>(seed, &mut policies).unwrap();
        let laps = &played.finish.stats[Arena::declaration().stat_index(LAPS_DONE.0).unwrap()];
        finished += laps.iter().filter(|l| **l >= f64::from(LAPS)).count();
        if !played.finish.decided_by.starts_with("First to the finish") {
            unwon.push(format!(
                "seed {seed}: {}, laps {laps:?}",
                played.finish.decided_by
            ));
        }
    }
    assert!(unwon.is_empty(), "{}", unwon.join("\n"));
    assert!(finished >= 72, "{finished} of 120 finished");
}

#[test]
fn outcomes_spread() {
    let mut spread = Spread::default();
    for seed in 0..8 {
        let mut policies = field(seed, 3, |seat, seed| {
            Box::new(Scripted::with_cruise(
                4.0 + ((seed as usize + seat) % 3) as f32,
            )) as Box<dyn Policy>
        });
        spread.add(&testing::play::<Arena>(seed, &mut policies).unwrap());
    }
    assert!(
        spread.winners.len() > 1,
        "always the same winner: {:?}",
        spread.winners
    );
}

#[test]
fn a_flipped_drone_crashes_respawns_and_still_places() {
    let declaration = Arena::declaration();
    let mut policies: Vec<Box<dyn Policy>> = vec![Box::new(Flipper), Box::new(Scripted::new())];
    let played = testing::play::<Arena>(2, &mut policies).unwrap();
    let crashes = &played.finish.stats[declaration.stat_index(CRASHES.0).unwrap()];
    assert!(crashes[0] >= 5.0, "flipper crashed {} times", crashes[0]);
    assert!(played.events_seen[declaration.event_index(RESPAWN.0).unwrap()]);
    assert_eq!(played.finish.places, vec![vec![1], vec![0]]);
}

#[test]
fn a_mixed_field_places_finishers_then_flyers_then_the_grounded() {
    // Two racers, one slow racer that can't finish, and two that never leave
    // their pads (placed by their pads' distance to the first gate).
    let mut policies: Vec<Box<dyn Policy>> = vec![
        Box::new(NoOp),
        Box::new(Scripted::new()),
        Box::new(Scripted::with_cruise(1.2)),
        Box::new(NoOp),
        Box::new(Scripted::with_cruise(5.0)),
    ];
    let played = testing::play::<Arena>(5, &mut policies).unwrap();
    assert_eq!(played.turns, MAX_TURNS, "the slow racer can't finish");
    let places = &played.finish.places;
    let first_two: Vec<u32> = places.iter().take(2).flatten().copied().collect();
    assert!(
        first_two.contains(&1) && first_two.contains(&4),
        "{places:?}"
    );
    assert_eq!(places[2], vec![2], "{places:?}");
    let last: Vec<u32> = places.iter().skip(3).flatten().copied().collect();
    assert_eq!(places.len(), 5, "{places:?}");
    assert!(last.contains(&0) && last.contains(&3), "{places:?}");
    assert!(
        played
            .finish
            .decided_by
            .starts_with("First to the finish in")
    );
}

#[test]
fn six_seats_step_well_under_budget() {
    let mut policies = field(1, 6, |seat, seed| {
        Box::new(Random::new(seed + seat as u64)) as Box<dyn Policy>
    });
    let (mut session, _) = Session::<Arena>::setup(&1u64.to_le_bytes(), 6).unwrap();
    let declaration = session.declaration().clone();
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..1000 {
        let outputs: Vec<Option<Vec<Values>>> = (0..6)
            .map(|seat| {
                let input = session.world().input(seat as u32);
                policies[seat].act(&declaration, seat as u32, &input)
            })
            .collect();
        let start = std::time::Instant::now();
        session.step_values(&outputs);
        worst = worst.max(start.elapsed());
    }
    // A turn is 10 ms of race time; an optimised native step takes a small part
    // of it. Unoptimised test builds are several times slower, and the
    // fuzzer measures the real worst step on the WASM build.
    let limit = if cfg!(debug_assertions) { 100 } else { 10 };
    assert!(worst.as_millis() < limit, "worst step {worst:?}");
}

#[test]
fn test_bots_never_link_the_engine() {
    // The scripted baseline reads only the declaration crate.
    let _: fn(&Declaration, u32, &[Values]) -> Option<Vec<Values>> = support::hover;
}
