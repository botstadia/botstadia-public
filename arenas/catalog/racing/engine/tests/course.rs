//! The course's fairness over many seeds: every course the seed draws is
//! flyable and fair across seats (the design note's promises), the start is
//! identical in every session, and the simulation puts every gate and
//! obstacle where the course says.

use arena_kit::{Report, Seed, World};
use racing_declaration::*;
use racing_engine::Arena;
use racing_engine::course::{self, GATE_1, OBSTACLES};

const SEEDS: u64 = 1000;

#[test]
fn every_course_passes_every_check() {
    let mut kidneys = 0;
    let mut used = [0u32; OBSTACLES + 1];
    let mut movers = [0u32; GATES];
    for seed in 0..SEEDS {
        let course = course::draw(&Seed::from_u64(seed));
        assert_eq!(
            course::problems(&course),
            Vec::<String>::new(),
            "seed {seed}"
        );
        if course.east_bend < course.bend {
            kidneys += 1;
        }
        used[course.obstacles.iter().filter(|o| o.is_some()).count()] += 1;
        movers[course.gates.iter().filter(|g| g.moves()).count()] += 1;
        // At least two obstacles even when some couldn't be placed.
        assert!(
            course.obstacles.iter().filter(|o| o.is_some()).count() >= 2,
            "seed {seed}: too few obstacles placed"
        );
    }
    println!("kidneys {kidneys}/{SEEDS}; obstacles used {used:?}; moving gates {movers:?}");
    assert!(
        (250..420).contains(&kidneys),
        "about one course in three is a kidney: {kidneys}"
    );
    assert!(movers[..2].iter().all(|n| *n == 0) && movers[5..].iter().all(|n| *n == 0));
}

#[test]
fn courses_differ_from_seed_to_seed() {
    let a = course::draw(&Seed::from_u64(1));
    let b = course::draw(&Seed::from_u64(2));
    assert_ne!(a.gates[3].base, b.gates[3].base);
    assert_ne!(a.straight, b.straight);
}

#[test]
fn the_start_is_identical_in_every_session() {
    let mut first: Option<Vec<[f64; 3]>> = None;
    for seed in 0..50 {
        let mut report = Report::new(&declaration(), MAX_SEATS);
        let arena = Arena::setup(&Seed::from_u64(seed), MAX_SEATS, &mut report).unwrap();
        assert_eq!(arena.course().gates[0].base, GATE_1);
        assert!(!arena.course().gates[0].moves());
        // The pads, as a set, and gate 1 relative to each.
        let mut start: Vec<[f64; 3]> = (0..MAX_SEATS).map(|s| arena.position(s)).collect();
        start.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let relative: Vec<[f64; 3]> = start
            .iter()
            .map(|p| [GATE_1[0] - p[0], GATE_1[1] - p[1], GATE_1[2] - p[2]])
            .collect();
        match &first {
            None => first = Some(relative),
            Some(expected) => assert_eq!(&relative, expected, "seed {seed}"),
        }
    }
}

#[test]
fn the_simulation_puts_gates_and_obstacles_where_the_course_says() {
    let mut report = Report::new(&declaration(), 2);
    let mut arena = Arena::setup(&Seed::from_u64(9), 2, &mut report).unwrap();
    for _ in 0..137 {
        report.next_turn();
        arena.step(&[None, None], &mut report);
    }
    let now = arena.now();
    let positions = arena.obstacle_positions();
    for (index, obstacle) in arena.course().obstacles.iter().enumerate() {
        let expected = match obstacle {
            Some(o) => o.position(now),
            None => course::parked(index),
        };
        let p = positions[index];
        let off = ((p[0] - expected[0]).powi(2)
            + (p[1] - expected[1]).powi(2)
            + (p[2] - expected[2]).powi(2))
        .sqrt();
        assert!(off < 1e-6, "obstacle {} is {off} m off", index + 1);
    }
}
