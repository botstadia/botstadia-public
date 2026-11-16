//! The vocabulary is whatever a G1 can do: every move, as authored and
//! mirrored, is held standing after ramping in from the stance; every change
//! from one pose to another over two beats at the fastest tempo stands; and
//! no pose brings any two of a dancer's own colliders together that are
//! apart in the stance, on bodies more than two links apart.

use arena_kit::mujoco::MjSim;
use dance_off_declaration::*;
use dance_off_engine::MODEL;

const SEAT: &str = "left";
/// The left dancer's free joint, then its joints, and what else it touches.
struct Dancer {
    qpos: [usize; 29],
    pelvis: usize,
    bodies: std::ops::Range<usize>,
    feet: Vec<usize>,
    stage: usize,
    geoms: Vec<usize>,
}

fn dancer(sim: &MjSim) -> Dancer {
    let mut qpos = [0; 29];
    for (index, joint) in JOINTS.iter().enumerate() {
        let id = sim.joint_id(&format!("{SEAT}_{joint}_joint")).unwrap();
        qpos[index] = sim.joint_addr(id).0;
    }
    let pelvis = sim.body_id(&format!("{SEAT}_pelvis")).unwrap();
    let last = sim
        .body_id(&format!("{SEAT}_right_wrist_yaw_link"))
        .unwrap();
    let mut feet = Vec::new();
    for side in ["left", "right"] {
        for name in [
            "foot_box_collision",
            "foot1_collision",
            "foot2_collision",
            "foot3_collision",
        ] {
            feet.push(sim.geom_id(&format!("{SEAT}_{side}_{name}")).unwrap());
        }
    }
    let geoms = (0..sim.ngeom())
        .filter(|g| (pelvis..last + 1).contains(&sim.geom_body(*g)) && sim.geom_collides(*g))
        .collect();
    Dancer {
        qpos,
        pelvis,
        bodies: pelvis..last + 1,
        feet,
        stage: sim.geom_id("stage").unwrap(),
        geoms,
    }
}

/// Every distinct pose: each move as authored, and mirrored where that's a
/// different pose.
fn poses() -> Vec<(String, [f32; 29])> {
    let mut out: Vec<(String, [f32; 29])> = Vec::new();
    for m in &MOVES {
        for (mirrored, targets) in [(false, m.targets), (true, mirror(&m.targets))] {
            if !out.iter().any(|(_, t)| *t == targets) {
                let name = if mirrored {
                    format!("{} (mirrored)", m.name)
                } else {
                    m.name.into()
                };
                out.push((name, targets));
            }
        }
    }
    out
}

/// Drive the left dancer through `schedule` (targets per turn), five
/// sub-steps a turn, from the start keyframe; the right dancer holds the
/// stance. The first turn it's down, if it falls.
fn run(sim: &mut MjSim, d: &Dancer, schedule: impl Iterator<Item = [f32; 29]>) -> Option<usize> {
    sim.reset(sim.keyframe_id("start"));
    let mut ctrl = sim.ctrl().to_vec();
    let mut touching = 0;
    for (turn, targets) in schedule.enumerate() {
        for (i, t) in targets.iter().enumerate() {
            ctrl[i] = f64::from(*t);
        }
        sim.set_ctrl(&ctrl);
        let mut touched = false;
        for _ in 0..5 {
            sim.step();
            for c in sim.contacts() {
                for (a, b) in [(c.geom1, c.geom2), (c.geom2, c.geom1)] {
                    if a == d.stage && d.bodies.contains(&sim.geom_body(b)) && !d.feet.contains(&b)
                    {
                        touched = true;
                    }
                }
            }
        }
        touching = if touched { touching + 1 } else { 0 };
        if sim.body_pos(d.pelvis)[2] < 0.4 || touching >= 5 {
            return Some(turn);
        }
    }
    None
}

fn lerp(a: &[f32; 29], b: &[f32; 29], t: f32) -> [f32; 29] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// Turns: 0.4 s to ramp in from the stance, 2 s to hold.
const RAMP: usize = 20;
const HOLD: usize = 100;

#[test]
fn every_pose_holds_standing() {
    let mut sim = MjSim::load(MODEL).unwrap();
    let d = dancer(&sim);
    let mut fallen = Vec::new();
    for (name, targets) in poses() {
        let schedule = (0..RAMP + HOLD).map(|turn| {
            lerp(
                &START_TARGETS,
                &targets,
                (turn as f32 / RAMP as f32).min(1.0),
            )
        });
        if let Some(turn) = run(&mut sim, &d, schedule) {
            fallen.push(format!("{name} falls at turn {turn}"));
        }
    }
    assert!(fallen.is_empty(), "{}", fallen.join("\n"));
}

#[test]
fn every_change_of_pose_at_the_fastest_tempo_stands() {
    let mut sim = MjSim::load(MODEL).unwrap();
    let d = dancer(&sim);
    // Two beats at the fastest tempo, in turns.
    let change = (2.0 * 60.0 / BPM_MAX as f32 * CONTROL_RATE as f32).round() as usize;
    let poses = poses();
    let mut fallen = Vec::new();
    for (from_name, from) in &poses {
        for (to_name, to) in &poses {
            if from == to {
                continue;
            }
            let schedule = (0..RAMP + 40 + change + 50).map(|turn| {
                if turn < RAMP {
                    lerp(&START_TARGETS, from, turn as f32 / RAMP as f32)
                } else if turn < RAMP + 40 {
                    *from
                } else {
                    lerp(
                        from,
                        to,
                        ((turn - RAMP - 40) as f32 / change as f32).min(1.0),
                    )
                }
            });
            if let Some(turn) = run(&mut sim, &d, schedule) {
                fallen.push(format!("{from_name} → {to_name} falls at turn {turn}"));
            }
        }
    }
    assert!(
        fallen.is_empty(),
        "{} changes fall:\n{}",
        fallen.len(),
        fallen.join("\n")
    );
}

/// Bodies within two links of each other: the same, parent and child, or
/// grandparent and grandchild (the knee's linkage brace reaches the foot).
fn near(sim: &MjSim, a: usize, b: usize) -> bool {
    let up = |x: usize| sim.body_parent(x);
    a == b || up(a) == b || up(b) == a || up(up(a)) == b || up(up(b)) == a
}

#[test]
fn no_pose_brings_a_dancers_own_colliders_together() {
    let mut sim = MjSim::load(MODEL).unwrap();
    let d = dancer(&sim);
    let pose_at = |sim: &mut MjSim, targets: &[f32; 29]| {
        sim.reset(sim.keyframe_id("start"));
        let mut qpos = sim.qpos().to_vec();
        for (adr, t) in d.qpos.iter().zip(targets) {
            qpos[*adr] = f64::from(*t);
        }
        sim.set_qpos(&qpos);
        sim.forward();
    };
    pose_at(&mut sim, &START_TARGETS);
    let mut apart = Vec::new();
    for (i, a) in d.geoms.iter().enumerate() {
        for b in &d.geoms[i + 1..] {
            if !near(&sim, sim.geom_body(*a), sim.geom_body(*b))
                && sim.geom_distance(*a, *b, 1.0) > 0.0
            {
                apart.push((*a, *b));
            }
        }
    }
    let mut touching = Vec::new();
    for (name, targets) in poses() {
        pose_at(&mut sim, &targets);
        for (a, b) in &apart {
            let distance = sim.geom_distance(*a, *b, 1.0);
            if distance <= 0.0 {
                touching.push(format!("{name}: geoms {a} and {b} at {distance:.3} m"));
            }
        }
    }
    assert!(touching.is_empty(), "{}", touching.join("\n"));
}
