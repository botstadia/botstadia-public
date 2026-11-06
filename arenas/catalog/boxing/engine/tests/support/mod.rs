//! Test policies. They read the declaration crate only, never the engine, as
//! the real test bots in REPOS/BOTS will.

#![allow(dead_code)]

use arena_kit::Values;
use arena_kit::declaration::Declaration;
use arena_kit::testing::Policy;
use boxing_declaration::*;

/// Motor-order indices.
pub const LHP: usize = 0;
pub const LHR: usize = 1;
pub const LK: usize = 3;
pub const LAP: usize = 4;
pub const RHP: usize = 6;
pub const RHR: usize = 7;
pub const RK: usize = 9;
pub const RAP: usize = 10;
pub const WAIST_PITCH: usize = 14;
pub const LSP: usize = 15;
pub const LSR: usize = 16;
pub const LE: usize = 18;
pub const RSP: usize = 22;
pub const RSR: usize = 23;
pub const RE: usize = 25;

/// Hold the start stance.
pub fn stand(_: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
    Some(vec![Values::F32(START_TARGETS.to_vec())])
}

/// Where a named slice of a named input starts.
fn slice_start(declaration: &Declaration, input: &str, slice: &str) -> usize {
    let value = declaration
        .inputs
        .iter()
        .find(|v| v.name == input)
        .expect("declared input");
    let mut start = 0;
    for s in &value.slices {
        if s.name == slice {
            return start;
        }
        start += s.count as usize;
    }
    panic!("no slice {slice} in {input}")
}

fn input_index(declaration: &Declaration, name: &str) -> usize {
    declaration
        .inputs
        .iter()
        .position(|v| v.name == name)
        .expect("declared input")
}

/// The scripted baseline: stand with a slight lean back, and once the opponent
/// is in reach throw slow, alternating straight punches. Classical control, no
/// learning: it doesn't step, because no fixed sequence of targets walks this
/// robot, and the lean is what keeps a punch's reaction from tipping it over.
pub struct Scripted {
    turn: u32,
    /// Whether this two-punch cycle is thrown or rested.
    throwing: bool,
    /// Throw with only this glove (0 left, 1 right), for tests.
    only: Option<usize>,
}

impl Scripted {
    pub fn new() -> Self {
        Self {
            turn: 0,
            throwing: true,
            only: None,
        }
    }

    /// Punches with one glove only, the other held in the stance.
    pub fn one_handed(side: usize) -> Self {
        Self {
            only: Some(side),
            ..Self::new()
        }
    }
}

/// Turns per punch, and the reach to start punching at.
pub const PUNCH_TURNS: u32 = 48;
/// The turns of a punch the arm is out: as few as reach the opponent, since
/// a punch held out longer rocks the puncher (the gloves' weight and the
/// reaction) until it topples over a long bout. 12 turns reach 0.6 m, 20
/// reach 0.74 m.
pub fn punch_out(ahead: f32) -> u32 {
    (12.0 + (ahead - 0.55) * 80.0).clamp(12.0, 20.0) as u32
}
pub const PUNCH_RANGE: f32 = 0.75;
/// Waist pitch while boxing: a slight lean back, which a punch's reaction then can't topple.
pub const LEAN: f32 = -0.1;
/// The punch: shoulder pitch and elbow targets, a straight at shoulder
/// height with the elbow nearly straight.
pub const PUNCH_PITCH: f32 = -1.5;
pub const PUNCH_ELBOW: f32 = 1.8;
/// Pacing: while either arm's hottest motor is this warm, it rests whole
/// two-punch cycles (left and right together, since one-handed runs tip it),
/// so its arms cool before they lose torque (heat 1, `HEAT_KNEE`).
pub const PACE_HEAT: f32 = 0.95;

impl Policy for Scripted {
    fn act(&mut self, declaration: &Declaration, _: u32, input: &[Values]) -> Option<Vec<Values>> {
        self.turn += 1;
        let opponent = input[input_index(declaration, IN_OPPONENT)]
            .as_f32()
            .unwrap();
        let ahead = opponent[slice_start(declaration, IN_OPPONENT, "pelvis")];

        let bout = input[input_index(declaration, IN_BOUT)].as_f32().unwrap();
        let opponent_down = bout[slice_start(declaration, IN_BOUT, "opponent_down")] > 0.5;
        let motors = input[input_index(declaration, IN_MOTORS)].as_f32().unwrap();
        let arms = slice_start(declaration, IN_MOTORS, "left_arm");
        let phase = self.turn % (2 * PUNCH_TURNS);
        if phase == 0 {
            self.throwing = motors[arms..arms + 14].iter().all(|h| *h < PACE_HEAT);
        }
        let mut q = START_TARGETS;
        q[WAIST_PITCH] = LEAN;
        if self.throwing
            && ahead <= PUNCH_RANGE
            && !opponent_down
            && phase % PUNCH_TURNS < punch_out(ahead)
        {
            let side = if phase < PUNCH_TURNS { 1 } else { 0 };
            let (sp, sr, el, roll) = if side == 1 {
                (RSP, RSR, RE, -0.3)
            } else {
                (LSP, LSR, LE, 0.3)
            };
            if self.only.is_none_or(|only| only == side) {
                q[sp] = PUNCH_PITCH;
                q[sr] = roll;
                q[el] = PUNCH_ELBOW;
            }
        }
        Some(vec![Values::F32(q.to_vec())])
    }
}

/// Folds its legs after a second, so something but a foot meets the canvas.
/// It never gets up: no fixed sequence of targets stands a fallen G1 up.
pub struct Faller {
    turn: u32,
}

impl Faller {
    pub fn new() -> Self {
        Self { turn: 0 }
    }
}

impl Policy for Faller {
    fn act(&mut self, _: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
        self.turn += 1;
        let mut q = START_TARGETS;
        if self.turn > 50 {
            for (hp, kn, ap) in [(LHP, LK, LAP), (RHP, RK, RAP)] {
                q[hp] = -2.2;
                q[kn] = 2.8;
                q[ap] = 0.5;
            }
        }
        Some(vec![Values::F32(q.to_vec())])
    }
}
