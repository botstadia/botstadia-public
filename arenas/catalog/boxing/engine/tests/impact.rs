//! Impact scoring: a punch is judged on its whole contact, at its peak normal
//! force, and harder punches score more. The band edges are measured from
//! the reference drill's landings (`print_landing_forces`).

mod support;

use arena_kit::session::Session;
use arena_kit::testing::{NoOp, Policy, Random};
use arena_kit::{Values, World};
use boxing_declaration::*;
use boxing_engine::{Arena, LANDING_WINDOW, Landing, PUNCH_COOLDOWN, punch_points, punch_strength};
use support::Scripted;

/// Every landing of one bout between `policies`, played to its finish.
fn landings(seed: u64, policies: &mut [Box<dyn Policy>]) -> Vec<Landing> {
    let (mut session, _) = Session::<Arena>::setup(&seed.to_le_bytes(), 2).unwrap();
    let declaration = session.declaration().clone();
    for _ in 0..declaration.max_turns {
        let inputs = [session.world().input(0), session.world().input(1)];
        let outputs: Vec<Option<Vec<Values>>> = (0..2)
            .map(|seat| policies[seat].act(&declaration, seat as u32, &inputs[seat]))
            .collect();
        session.step_values(&outputs);
        if session.finished().is_some() {
            break;
        }
    }
    session.world().landings().to_vec()
}

fn scripted_pair() -> Vec<Box<dyn Policy>> {
    vec![Box::new(Scripted::new()), Box::new(Scripted::new())]
}

/// The sample the bands come from: the reference drill, the scripted bot's
/// committed straights on a fighter that only stands, over 20 seeds. It
/// measures what the robot delivers, whatever a defence would take off.
fn sample() -> Vec<Landing> {
    let mut all = Vec::new();
    for seed in 0..20 {
        let mut drill: Vec<Box<dyn Policy>> = vec![Box::new(Scripted::new()), Box::new(NoOp)];
        all.extend(landings(seed, &mut drill));
    }
    all
}

/// Bouts as house bots fight them, for the README: scripted against
/// scripted and against random.
fn bouts() -> Vec<Landing> {
    let mut all = Vec::new();
    for seed in 0..20 {
        all.extend(landings(seed, &mut scripted_pair()));
        let mut mixed: Vec<Box<dyn Policy>> =
            vec![Box::new(Scripted::new()), Box::new(Random::new(seed))];
        all.extend(landings(seed, &mut mixed));
    }
    all
}

fn percentile(sorted: &[f32], p: f64) -> f32 {
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index]
}

#[test]
#[ignore = "prints the landed-punch force distribution the bands come from"]
fn print_landing_forces() {
    for (name, all) in [("drill", sample()), ("bouts", bouts())] {
        describe(name, &all);
    }
}

fn describe(name: &str, all: &[Landing]) {
    let mut forces: Vec<f32> = all.iter().map(|l| l.force).collect();
    forces.sort_by(f32::total_cmp);
    let head = all.iter().filter(|l| l.head).count();
    println!(
        "{name}: {} landings ({head} to the head); peak force N: min {:.1}, p25 {:.1}, median {:.1}, p75 {:.1}, p90 {:.1}, p99 {:.1}, max {:.1}",
        forces.len(),
        forces[0],
        percentile(&forces, 0.25),
        percentile(&forces, 0.5),
        percentile(&forces, 0.75),
        percentile(&forces, 0.9),
        percentile(&forces, 0.99),
        forces[forces.len() - 1],
    );
}

#[test]
fn bands_rise_with_force_and_points_with_the_band() {
    const { assert!(LANDING_FORCE_N < SOLID_PUNCH_N && SOLID_PUNCH_N < POWER_PUNCH_N) };
    assert_eq!(punch_strength(LANDING_FORCE_N), 0);
    assert_eq!(punch_strength(SOLID_PUNCH_N), 1);
    assert_eq!(punch_strength(POWER_PUNCH_N), 2);
    assert_eq!(punch_points(1, 0), BODY_POINTS);
    assert_eq!(punch_points(2, 0), HEAD_POINTS);
    assert_eq!(punch_points(2, 2), HEAD_POINTS + 2);
    // A power punch to the body outscores a plain one to the head.
    assert!(punch_points(1, 2) > punch_points(2, 0));
}

#[test]
fn a_punch_is_judged_once_over_its_contact_within_the_window() {
    let mut all = landings(3, &mut scripted_pair());
    let mut drill: Vec<Box<dyn Policy>> = vec![Box::new(Scripted::new()), Box::new(NoOp)];
    let drilled = landings(3, &mut drill);
    all.extend(drilled.iter().copied());
    assert!(!drilled.is_empty(), "the drill lands punches");
    for landing in &all {
        assert!(landing.force >= LANDING_FORCE_N, "{landing:?}");
        // Judged at its contact's end, at most the window after the touch.
        assert!(
            landing.turn >= landing.opened && landing.turn - landing.opened < LANDING_WINDOW,
            "{landing:?}"
        );
    }
    // One punch per fist per cooldown: no contact scores twice.
    for bout in [&drilled] {
        for seat in 0..2 {
            for side in 0..2 {
                let opened: Vec<u32> = bout
                    .iter()
                    .filter(|l| l.seat == seat && l.side == side)
                    .map(|l| l.opened)
                    .collect();
                for pair in opened.windows(2) {
                    assert!(pair[1] - pair[0] >= PUNCH_COOLDOWN, "{opened:?}");
                }
            }
        }
    }
    // A committed punch's contact spans turns, so judging only the first
    // turn would miss its later force.
    assert!(
        drilled.iter().any(|l| l.turn > l.opened),
        "no drill punch's contact outlasted its first turn"
    );
}

#[test]
fn every_landing_scores_by_its_band() {
    let mut drilled = Vec::new();
    for seed in 0..4 {
        let mut drill: Vec<Box<dyn Policy>> = vec![Box::new(Scripted::new()), Box::new(NoOp)];
        drilled.extend(landings(seed, &mut drill));
    }
    // The drill reaches every band, and each landing's points follow the
    // table: the base for what it hit plus its band.
    let bands: std::collections::BTreeSet<u8> =
        drilled.iter().map(|l| punch_strength(l.force)).collect();
    assert_eq!(bands.len(), 3, "{bands:?}");
    for landing in &drilled {
        let base = if landing.head {
            HEAD_POINTS
        } else {
            BODY_POINTS
        };
        let extra = if landing.force >= POWER_PUNCH_N {
            2
        } else if landing.force >= SOLID_PUNCH_N {
            1
        } else {
            0
        };
        assert_eq!(landing.points, base + extra, "{landing:?}");
    }
}
