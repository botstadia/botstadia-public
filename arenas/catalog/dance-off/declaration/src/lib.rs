//! Dance-Off's declaration: every input and output, the budgets, the stats,
//! series and events the site draws, and the game's own numbers (the moves and
//! the routine's timing) that bots may read. Test bots depend on this crate,
//! and never on the engine. The design note (`../README.md`) says what each
//! value is for.

use arena_kit::declaration::*;
use arena_kit::{EventKey, SeriesKey, StatKey};

/// The arena version: bump it, with a changelog entry, for every engine change.
pub const VERSION: &str = "0.2.0";
/// The payload schema: 2 adds the beat's period to `dance`.
pub const SCHEMA: u32 = 2;

/// Turns a second, and the session's length.
pub const CONTROL_RATE: u32 = 50;
pub const SESSION_SECONDS: u32 = 90;
pub const MAX_TURNS: u32 = CONTROL_RATE * SESSION_SECONDS;

/// The seats' names, in seat order, and the body-name prefix each uses.
/// Left and right as the audience sees the stage.
pub const SEATS: [&str; 2] = ["left", "right"];

// ── The routine's timing ─────────────────────────────────────────────

/// The seed draws each session's tempo, in whole beats a minute, from this
/// range; everything musical follows it.
pub const BPM_MIN: u32 = 84;
pub const BPM_MAX: u32 = 150;
/// 0.1.0's one tempo, which a replay from before the seeded tempo plays at.
pub const DEFAULT_BPM: u32 = 120;
/// Beats before the first card's beat.
pub const INTRO_BEATS: u32 = 8;
/// Beats between consecutive cards.
pub const BEATS_PER_CARD: u32 = 2;
/// The most cards a routine can have: the fastest tempo's (a test pins it).
pub const MAX_CARDS: usize = 109;
/// The routine's shape: phrases of four cards. The first `PLAIN_CARDS` are
/// three plain phrases; from there the routine repeats two phrases and a
/// freestyle bar.
pub const PHRASE: usize = 4;
pub const PLAIN_CARDS: usize = 12;
pub const BLOCK: usize = 2 * PHRASE + 1;
/// Difficulty tiers: the first `RAMP_CARDS` draw from the six basic moves,
/// the cards before `TIER_3_CARDS` add the arm moves, and from there on
/// every move.
pub const RAMP_CARDS: usize = 8;
pub const TIER_3_CARDS: usize = 24;
/// A card scores over this many beats either side of its beat (0.4 s at 120
/// BPM)…
pub const WINDOW_BEATS: f32 = 0.8;
/// …and a hold card or a freestyle bar over twice that.
pub const LONG_WINDOW_BEATS: f32 = 2.0 * WINDOW_BEATS;

/// Turns a beat at `bpm` (25 at 120 BPM and 50 turns a second).
pub fn turns_per_beat(bpm: u32) -> f64 {
    f64::from(CONTROL_RATE) * 60.0 / f64::from(bpm)
}

/// Seconds a beat at `bpm`.
pub fn beat_period(bpm: u32) -> f32 {
    60.0 / bpm as f32
}

/// The turn on which card `index` lands at `bpm`: beats fall between turns
/// at most tempos, so each lands on the nearest.
pub fn card_beat_turn(index: usize, bpm: u32) -> u32 {
    (f64::from(INTRO_BEATS + BEATS_PER_CARD * index as u32) * turns_per_beat(bpm)).round() as u32
}

/// Whether card `index` of the routine is a freestyle bar.
pub fn is_freestyle(index: usize) -> bool {
    index >= PLAIN_CARDS && (index - PLAIN_CARDS) % BLOCK == BLOCK - 1
}

// ── Scoring ──────────────────────────────────────────────────────────

/// A card's base points, before quality and the combo multiplier.
pub const CARD_POINTS: f32 = 100.0;
/// A card resolved below this quality is a miss and breaks the combo…
pub const MISS_QUALITY: f32 = 0.3;
/// …and one at or above this extends it.
pub const COMBO_QUALITY: f32 = 0.6;
/// The multiplier rises by `COMBO_STEP_BONUS` every `COMBO_STEP` combo cards…
pub const COMBO_STEP: u32 = 4;
pub const COMBO_STEP_BONUS: f32 = 0.25;
/// …to at most this.
pub const COMBO_MAX_MULTIPLIER: f32 = 2.0;
/// Points lost for a fall (the score never goes below zero).
pub const FALL_PENALTY: u32 = 50;
/// How long a fallen dancer is down before it's stood back up, in seconds.
pub const DOWN_SECONDS: f32 = 2.0;

/// The most a routine of `cards` cards can score: every card perfect, the
/// combo unbroken from the first.
pub fn max_score(cards: usize) -> f32 {
    (0..cards as u32)
        .map(|combo| CARD_POINTS * combo_multiplier(combo))
        .sum()
}

/// The combo multiplier for a combo of `combo` cards.
pub fn combo_multiplier(combo: u32) -> f32 {
    (1.0 + COMBO_STEP_BONUS * (combo / COMBO_STEP) as f32).min(COMBO_MAX_MULTIPLIER)
}

/// What a card asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    /// Be in the move's pose on the beat.
    Pose = 0,
    /// The same, held over a window twice as long.
    Hold = 1,
    /// No pose: move as much, as widely and as evenly as you can.
    Freestyle = 2,
}

impl CardKind {
    /// The card's window half-width, in beats.
    pub fn window_beats(self) -> f32 {
        match self {
            CardKind::Pose => WINDOW_BEATS,
            CardKind::Hold | CardKind::Freestyle => LONG_WINDOW_BEATS,
        }
    }

    /// The card's window half-width at `bpm`, in turns.
    pub fn window_turns(self, bpm: u32) -> u32 {
        (f64::from(self.window_beats()) * turns_per_beat(bpm)).round() as u32
    }
}

// ── The robot ────────────────────────────────────────────────────────

/// The G1's 29 joints in motor order: left leg, right leg, waist, left arm,
/// right arm. Inputs and outputs follow this order; never reorder.
pub const JOINTS: [&str; 29] = [
    "left_hip_pitch",
    "left_hip_roll",
    "left_hip_yaw",
    "left_knee",
    "left_ankle_pitch",
    "left_ankle_roll",
    "right_hip_pitch",
    "right_hip_roll",
    "right_hip_yaw",
    "right_knee",
    "right_ankle_pitch",
    "right_ankle_roll",
    "waist_yaw",
    "waist_roll",
    "waist_pitch",
    "left_shoulder_pitch",
    "left_shoulder_roll",
    "left_shoulder_yaw",
    "left_elbow",
    "left_wrist_roll",
    "left_wrist_pitch",
    "left_wrist_yaw",
    "right_shoulder_pitch",
    "right_shoulder_roll",
    "right_shoulder_yaw",
    "right_elbow",
    "right_wrist_roll",
    "right_wrist_pitch",
    "right_wrist_yaw",
];

/// Each joint's range, radians, as the model has it: the motors' `ctrlrange`
/// (a test pins it against `world.mjb`), and the bounds of `targets` and `joints`.
#[allow(clippy::approx_constant)] // the model's own limits, some of which happen to be fractions of pi
pub const JOINT_RANGES: [(f32, f32); 29] = [
    (-2.5307, 2.8798),
    (-0.5236, 2.9671),
    (-2.7576, 2.7576),
    (-0.087267, 2.8798),
    (-0.87267, 0.5236),
    (-0.2618, 0.2618),
    (-2.5307, 2.8798),
    (-2.9671, 0.5236),
    (-2.7576, 2.7576),
    (-0.087267, 2.8798),
    (-0.87267, 0.5236),
    (-0.2618, 0.2618),
    (-2.618, 2.618),
    (-0.52, 0.52),
    (-0.52, 0.52),
    (-3.0892, 2.6704),
    (-1.5882, 2.2515),
    (-2.618, 2.618),
    (-1.0472, 2.0944),
    (-1.97222, 1.97222),
    (-1.61443, 1.61443),
    (-1.61443, 1.61443),
    (-3.0892, 2.6704),
    (-2.2515, 1.5882),
    (-2.618, 2.618),
    (-1.0472, 2.0944),
    (-1.97222, 1.97222),
    (-1.61443, 1.61443),
    (-1.61443, 1.61443),
];

/// The start stance's joint targets (the G1's "home"), in motor order.
pub const START_TARGETS: [f32; 29] = [
    -0.1, 0.0, 0.0, 0.3, -0.2, 0.0, -0.1, 0.0, 0.0, 0.3, -0.2, 0.0, 0.0, 0.0, 0.0, 0.2, 0.2, 0.0,
    1.28, 0.0, 0.0, 0.0, 0.2, -0.2, 0.0, 1.28, 0.0, 0.0, 0.0,
];

/// Joints whose sign flips when left and right swap (rolls and yaws), by motor
/// index. Pitches, knees and elbows keep theirs.
const MIRROR_NEGATE: [usize; 16] = [1, 2, 5, 7, 8, 11, 12, 13, 16, 17, 19, 21, 23, 24, 26, 28];

/// A pose with left and right swapped: the legs and arms trade places, and
/// rolls and yaws change sign. Mirroring twice gives the pose back.
pub fn mirror(targets: &[f32; 29]) -> [f32; 29] {
    let mut out = *targets;
    out[..6].copy_from_slice(&targets[6..12]);
    out[6..12].copy_from_slice(&targets[..6]);
    out[15..22].copy_from_slice(&targets[22..29]);
    out[22..29].copy_from_slice(&targets[15..22]);
    for &i in &MIRROR_NEGATE {
        out[i] = -out[i];
    }
    out
}

// ── The moves ────────────────────────────────────────────────────────

/// The key bones a pose is scored on, in weight order. Each is a direction:
/// the torso from the pelvis to between the shoulders, each upper arm from
/// shoulder to elbow, each forearm from elbow to wrist, each thigh from hip to
/// knee, each shin from knee to ankle.
pub const KEY_BONES: [&str; 9] = [
    "torso",
    "left_upper_arm",
    "left_forearm",
    "right_upper_arm",
    "right_forearm",
    "left_thigh",
    "left_shin",
    "right_thigh",
    "right_shin",
];

/// Key-bone weights with left and right swapped, for a mirror card.
pub fn mirror_weights(weights: &[f32; 9]) -> [f32; 9] {
    [
        weights[0], weights[3], weights[4], weights[1], weights[2], weights[7], weights[8],
        weights[5], weights[6],
    ]
}

/// One move of the vocabulary: its joint targets and how much each key bone
/// counts in scoring it.
#[derive(Debug, Clone, Copy)]
pub struct Move {
    pub name: &'static str,
    /// 29 joint targets, motor order. Every one holds standing.
    pub targets: [f32; 29],
    /// Weights over `KEY_BONES`; the engine normalises them.
    pub weights: [f32; 9],
}

/// The start stance with some joints set: `(motor index, angle)`.
const fn pose(set: &[(usize, f32)]) -> [f32; 29] {
    let mut out = START_TARGETS;
    let mut i = 0;
    while i < set.len() {
        out[set[i].0] = set[i].1;
        i += 1;
    }
    out
}

// Motor indices, for authoring.
const L_HIP_PITCH: usize = 0;
const L_HIP_ROLL: usize = 1;
const L_KNEE: usize = 3;
const L_ANKLE_PITCH: usize = 4;
const L_ANKLE_ROLL: usize = 5;
const R_HIP_PITCH: usize = 6;
const R_HIP_ROLL: usize = 7;
const R_KNEE: usize = 9;
const R_ANKLE_PITCH: usize = 10;
const R_ANKLE_ROLL: usize = 11;
const WAIST_YAW: usize = 12;
const WAIST_ROLL: usize = 13;
const L_SHOULDER_PITCH: usize = 15;
const L_SHOULDER_ROLL: usize = 16;
const L_SHOULDER_YAW: usize = 17;
const L_ELBOW: usize = 18;
const R_SHOULDER_PITCH: usize = 22;
const R_SHOULDER_ROLL: usize = 23;
const R_SHOULDER_YAW: usize = 24;
const R_ELBOW: usize = 25;

/// The G1's elbow is bent a right angle at zero: this is a straight arm.
const STRAIGHT: f32 = 1.5;

/// How many moves the vocabulary has.
pub const MOVE_COUNT: usize = 20;

/// The vocabulary. The player's card glyphs follow this order; ids 0 to 9
/// are 0.1.0's and keep their numbers, and new moves append.
pub const MOVES: [Move; MOVE_COUNT] = [
    Move {
        name: "t-pose",
        targets: pose(&[
            (L_SHOULDER_ROLL, 1.5),
            (R_SHOULDER_ROLL, -1.5),
            (L_SHOULDER_PITCH, 0.0),
            (R_SHOULDER_PITCH, 0.0),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [1.0, 2.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0],
    },
    Move {
        name: "arms-up",
        targets: pose(&[
            (L_SHOULDER_ROLL, 2.2),
            (R_SHOULDER_ROLL, -2.2),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [1.0, 2.0, 2.0, 2.0, 2.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "wave-l",
        targets: pose(&[
            (WAIST_ROLL, -0.15),
            (L_SHOULDER_ROLL, 1.5),
            (L_SHOULDER_YAW, 1.5),
            (L_ELBOW, 0.0),
            (R_SHOULDER_ROLL, -0.4),
        ]),
        weights: [1.0, 3.0, 3.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "wave-r",
        targets: pose(&[
            (WAIST_ROLL, 0.15),
            (R_SHOULDER_ROLL, -1.5),
            (R_SHOULDER_YAW, -1.5),
            (R_ELBOW, 0.0),
            (L_SHOULDER_ROLL, 0.4),
        ]),
        weights: [1.0, 1.0, 1.0, 3.0, 3.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "step-touch-l",
        targets: pose(&[
            (L_HIP_ROLL, 0.2),
            (L_ANKLE_ROLL, -0.2),
            (R_HIP_ROLL, -0.05),
            (R_ANKLE_ROLL, 0.05),
            (WAIST_ROLL, 0.12),
            (L_SHOULDER_ROLL, 1.5),
            (L_ELBOW, STRAIGHT),
            (R_SHOULDER_ROLL, -0.3),
        ]),
        weights: [1.5, 2.0, 2.0, 1.0, 1.0, 3.0, 2.0, 1.0, 1.0],
    },
    Move {
        name: "step-touch-r",
        targets: pose(&[
            (R_HIP_ROLL, -0.2),
            (R_ANKLE_ROLL, 0.2),
            (L_HIP_ROLL, 0.05),
            (L_ANKLE_ROLL, -0.05),
            (WAIST_ROLL, -0.12),
            (R_SHOULDER_ROLL, -1.5),
            (R_ELBOW, STRAIGHT),
            (L_SHOULDER_ROLL, 0.3),
        ]),
        weights: [1.5, 1.0, 1.0, 2.0, 2.0, 1.0, 1.0, 3.0, 2.0],
    },
    Move {
        name: "squat",
        targets: pose(&[
            (L_HIP_PITCH, -0.6),
            (R_HIP_PITCH, -0.6),
            (L_KNEE, 1.0),
            (R_KNEE, 1.0),
            (L_ANKLE_PITCH, -0.4),
            (R_ANKLE_PITCH, -0.4),
            (L_SHOULDER_PITCH, -1.4),
            (R_SHOULDER_PITCH, -1.4),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [2.0, 1.0, 1.0, 1.0, 1.0, 3.0, 3.0, 3.0, 3.0],
    },
    Move {
        name: "lunge-l",
        targets: pose(&[
            (L_HIP_PITCH, -0.5),
            (R_HIP_PITCH, -0.5),
            (L_KNEE, 0.8),
            (R_KNEE, 0.8),
            (L_ANKLE_PITCH, -0.35),
            (R_ANKLE_PITCH, -0.35),
            (L_HIP_ROLL, 0.18),
            (L_ANKLE_ROLL, -0.18),
            (R_HIP_ROLL, -0.18),
            (R_ANKLE_ROLL, 0.18),
            (L_SHOULDER_ROLL, 1.4),
            (R_SHOULDER_PITCH, -1.4),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [1.5, 1.5, 1.0, 1.5, 1.0, 3.0, 2.5, 2.0, 1.0],
    },
    Move {
        name: "lunge-r",
        targets: pose(&[
            (L_HIP_PITCH, -0.5),
            (R_HIP_PITCH, -0.5),
            (L_KNEE, 0.8),
            (R_KNEE, 0.8),
            (L_ANKLE_PITCH, -0.35),
            (R_ANKLE_PITCH, -0.35),
            (L_HIP_ROLL, 0.18),
            (L_ANKLE_ROLL, -0.18),
            (R_HIP_ROLL, -0.18),
            (R_ANKLE_ROLL, 0.18),
            (R_SHOULDER_ROLL, -1.4),
            (L_SHOULDER_PITCH, -1.4),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [1.5, 1.5, 1.0, 1.5, 1.0, 2.0, 1.0, 3.0, 2.5],
    },
    Move {
        name: "spin",
        targets: pose(&[
            (WAIST_YAW, 1.0),
            (L_SHOULDER_ROLL, 1.3),
            (R_SHOULDER_ROLL, -1.3),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [3.0, 2.0, 1.0, 2.0, 1.0, 1.5, 0.5, 1.5, 0.5],
    },
    Move {
        name: "arms-cross",
        targets: pose(&[
            (L_HIP_PITCH, -0.3),
            (R_HIP_PITCH, -0.3),
            (L_KNEE, 0.55),
            (R_KNEE, 0.55),
            (L_ANKLE_PITCH, -0.25),
            (R_ANKLE_PITCH, -0.25),
            (L_SHOULDER_PITCH, -1.3),
            (R_SHOULDER_PITCH, -0.9),
            (L_SHOULDER_ROLL, -0.2),
            (R_SHOULDER_ROLL, 0.2),
            (L_ELBOW, 0.7),
            (R_ELBOW, 0.7),
        ]),
        weights: [1.0, 2.5, 3.0, 2.5, 3.0, 1.0, 1.0, 1.0, 1.0],
    },
    Move {
        name: "one-arm-up-l",
        targets: pose(&[
            (L_SHOULDER_ROLL, 2.2),
            (L_ELBOW, STRAIGHT),
            (R_SHOULDER_ROLL, -0.4),
        ]),
        weights: [1.0, 3.0, 3.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "one-arm-up-r",
        targets: pose(&[
            (R_SHOULDER_ROLL, -2.2),
            (R_ELBOW, STRAIGHT),
            (L_SHOULDER_ROLL, 0.4),
        ]),
        weights: [1.0, 1.0, 1.0, 3.0, 3.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "point-l",
        targets: pose(&[
            (L_HIP_PITCH, -0.3),
            (R_HIP_PITCH, -0.3),
            (L_KNEE, 0.55),
            (R_KNEE, 0.55),
            (L_ANKLE_PITCH, -0.25),
            (R_ANKLE_PITCH, -0.25),
            (WAIST_YAW, 0.3),
            (L_SHOULDER_PITCH, -1.5),
            (L_SHOULDER_ROLL, 0.4),
            (L_ELBOW, STRAIGHT),
            (R_SHOULDER_PITCH, 0.6),
        ]),
        weights: [1.5, 3.0, 3.0, 1.5, 1.0, 1.0, 1.0, 1.0, 1.0],
    },
    Move {
        name: "point-r",
        targets: pose(&[
            (L_HIP_PITCH, -0.3),
            (R_HIP_PITCH, -0.3),
            (L_KNEE, 0.55),
            (R_KNEE, 0.55),
            (L_ANKLE_PITCH, -0.25),
            (R_ANKLE_PITCH, -0.25),
            (WAIST_YAW, -0.3),
            (R_SHOULDER_PITCH, -1.5),
            (R_SHOULDER_ROLL, -0.4),
            (R_ELBOW, STRAIGHT),
            (L_SHOULDER_PITCH, 0.6),
        ]),
        weights: [1.5, 1.5, 1.0, 3.0, 3.0, 1.0, 1.0, 1.0, 1.0],
    },
    Move {
        name: "hands-on-hips",
        targets: pose(&[
            (L_SHOULDER_PITCH, 0.4),
            (R_SHOULDER_PITCH, 0.4),
            (L_SHOULDER_ROLL, 0.7),
            (R_SHOULDER_ROLL, -0.7),
            (L_SHOULDER_YAW, -0.6),
            (R_SHOULDER_YAW, 0.6),
            (L_ELBOW, 0.0),
            (R_ELBOW, 0.0),
        ]),
        weights: [1.0, 2.5, 3.0, 2.5, 3.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "lean-l",
        targets: pose(&[
            (WAIST_ROLL, -0.4),
            (R_SHOULDER_ROLL, -2.2),
            (R_ELBOW, STRAIGHT),
            (L_SHOULDER_ROLL, 0.35),
        ]),
        weights: [3.0, 1.0, 1.0, 2.5, 2.5, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "lean-r",
        targets: pose(&[
            (WAIST_ROLL, 0.4),
            (L_SHOULDER_ROLL, 2.2),
            (L_ELBOW, STRAIGHT),
            (R_SHOULDER_ROLL, -0.35),
        ]),
        weights: [3.0, 2.5, 2.5, 1.0, 1.0, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "reach-up-twist-l",
        targets: pose(&[
            (WAIST_YAW, 0.8),
            (L_SHOULDER_ROLL, 2.2),
            (R_SHOULDER_ROLL, -2.2),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [3.0, 2.0, 1.5, 2.0, 1.5, 0.5, 0.5, 0.5, 0.5],
    },
    Move {
        name: "reach-up-twist-r",
        targets: pose(&[
            (WAIST_YAW, -0.8),
            (L_SHOULDER_ROLL, 2.2),
            (R_SHOULDER_ROLL, -2.2),
            (L_ELBOW, STRAIGHT),
            (R_ELBOW, STRAIGHT),
        ]),
        weights: [3.0, 2.0, 1.5, 2.0, 1.5, 0.5, 0.5, 0.5, 0.5],
    },
];

/// Each key bone's direction in a pose, a unit vector in the dancer's
/// heading frame (x forward, y left, z up), in `KEY_BONES` order.
pub type KeyDirs = [[f32; 3]; 9];

/// The key bones' directions in the start stance and in every move, as
/// authored and mirrored: what scoring compares a dancer against. Measured
/// once from the baked model posed in each (the engine's tests pin them to
/// it), so a session's setup poses nothing.
pub const STANCE_DIRS: KeyDirs = [
    [-0.000025, 0.000017, 1.000000],
    [-0.113908, 0.230268, -0.966438],
    [0.036924, 0.210721, -0.976849],
    [-0.113908, -0.230268, -0.966438],
    [0.036924, -0.210721, -0.976849],
    [0.099823, 0.007019, -0.994980],
    [-0.187881, -0.000298, -0.982192],
    [0.099823, -0.007019, -0.994980],
    [-0.187881, 0.000298, -0.982192],
];
pub const MOVE_DIRS: [[KeyDirs; 2]; MOVE_COUNT] = [
    // t-pose
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.085733, 0.995644, -0.036647],
            [0.016688, 0.998032, -0.060447],
            [0.085733, -0.995644, -0.036647],
            [0.016688, -0.998032, -0.060447],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.085733, 0.995644, -0.036647],
            [0.016688, 0.998032, -0.060447],
            [0.085733, -0.995644, -0.036647],
            [0.016688, -0.998032, -0.060447],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // arms-up
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.158161, 0.791835, 0.589901],
            [0.086372, 0.805104, 0.586812],
            [0.158161, -0.791835, 0.589901],
            [0.086372, -0.805104, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.158161, 0.791835, 0.589901],
            [0.086372, 0.805104, 0.586812],
            [0.158161, -0.791835, 0.589901],
            [0.086372, -0.805104, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // wave-l
    [
        [
            [-0.000025, 0.127103, 0.991890],
            [-0.039368, 0.994394, -0.098131],
            [0.241592, 0.275033, 0.930586],
            [-0.111707, -0.548279, -0.828801],
            [0.038169, -0.528264, -0.848222],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, -0.127069, 0.991894],
            [-0.111707, 0.548279, -0.828801],
            [0.038169, 0.528264, -0.848222],
            [-0.039368, -0.994394, -0.098131],
            [0.241592, -0.275033, 0.930586],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // wave-r
    [
        [
            [-0.000025, -0.127069, 0.991894],
            [-0.111707, 0.548279, -0.828801],
            [0.038169, 0.528264, -0.848222],
            [-0.039368, -0.994394, -0.098131],
            [0.241592, -0.275033, 0.930586],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.127103, 0.991890],
            [-0.039368, 0.994394, -0.098131],
            [0.241592, 0.275033, 0.930586],
            [-0.111707, -0.548279, -0.828801],
            [0.038169, -0.528264, -0.848222],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // step-touch-l
    [
        [
            [-0.000025, -0.101736, 0.994811],
            [0.022497, 0.997090, 0.072842],
            [-0.049848, 0.996795, 0.062569],
            [-0.113795, -0.211175, -0.970802],
            [0.036582, -0.188476, -0.981396],
            [0.094117, 0.202513, -0.974746],
            [-0.192704, 0.177381, -0.965091],
            [0.099394, -0.056226, -0.993458],
            [-0.188180, -0.044400, -0.981130],
        ],
        [
            [-0.000025, 0.101771, 0.994808],
            [-0.113795, 0.211175, -0.970802],
            [0.036582, 0.188476, -0.981396],
            [0.022497, -0.997090, 0.072842],
            [-0.049848, -0.996795, 0.062569],
            [0.099394, 0.056226, -0.993458],
            [-0.188180, 0.044400, -0.981130],
            [0.094117, -0.202513, -0.974746],
            [-0.192704, -0.177381, -0.965091],
        ],
    ],
    // step-touch-r
    [
        [
            [-0.000025, 0.101771, 0.994808],
            [-0.113795, 0.211175, -0.970802],
            [0.036582, 0.188476, -0.981396],
            [0.022497, -0.997090, 0.072842],
            [-0.049848, -0.996795, 0.062569],
            [0.099394, 0.056226, -0.993458],
            [-0.188180, 0.044400, -0.981130],
            [0.094117, -0.202513, -0.974746],
            [-0.192704, -0.177381, -0.965091],
        ],
        [
            [-0.000025, -0.101736, 0.994811],
            [0.022497, 0.997090, 0.072842],
            [-0.049848, 0.996795, 0.062569],
            [-0.113795, -0.211175, -0.970802],
            [0.036582, -0.188476, -0.981396],
            [0.094117, 0.202513, -0.974746],
            [-0.192704, 0.177381, -0.965091],
            [0.099394, -0.056226, -0.993458],
            [-0.188180, -0.044400, -0.981130],
        ],
    ],
    // squat
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.995361, -0.020161, -0.094075],
            [0.985761, -0.024199, -0.166400],
            [0.995361, 0.020161, -0.094075],
            [0.985761, 0.024199, -0.166400],
            [0.564622, 0.007019, -0.825320],
            [-0.369414, -0.000299, -0.929265],
            [0.564622, -0.007019, -0.825320],
            [-0.369414, 0.000299, -0.929265],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.995361, -0.020161, -0.094075],
            [0.985761, -0.024199, -0.166400],
            [0.995361, 0.020161, -0.094075],
            [0.985761, 0.024199, -0.166400],
            [0.564622, 0.007019, -0.825320],
            [-0.369414, -0.000299, -0.929265],
            [0.564622, -0.007019, -0.825320],
            [-0.369414, 0.000299, -0.929265],
        ],
    ],
    // lunge-l
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.004015, 0.989500, -0.144480],
            [-0.068222, 0.985592, -0.154772],
            [0.995361, 0.020161, -0.094075],
            [0.985761, 0.024199, -0.166400],
            [0.468681, 0.183200, -0.864162],
            [-0.283088, 0.102987, -0.953549],
            [0.468681, -0.183200, -0.864162],
            [-0.283088, -0.102987, -0.953549],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.995361, -0.020161, -0.094075],
            [0.985761, -0.024199, -0.166400],
            [0.004015, -0.989500, -0.144480],
            [-0.068222, -0.985592, -0.154772],
            [0.468681, 0.183200, -0.864162],
            [-0.283088, 0.102987, -0.953549],
            [0.468681, -0.183200, -0.864162],
            [-0.283088, -0.102987, -0.953549],
        ],
    ],
    // lunge-r
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.995361, -0.020161, -0.094075],
            [0.985761, -0.024199, -0.166400],
            [0.004015, -0.989500, -0.144480],
            [-0.068222, -0.985592, -0.154772],
            [0.468681, 0.183200, -0.864162],
            [-0.283088, 0.102987, -0.953549],
            [0.468681, -0.183200, -0.864162],
            [-0.283088, -0.102987, -0.953549],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.004015, 0.989500, -0.144480],
            [-0.068222, 0.985592, -0.154772],
            [0.995361, 0.020161, -0.094075],
            [0.985761, 0.024199, -0.166400],
            [0.468681, 0.183200, -0.864162],
            [-0.283088, 0.102987, -0.953549],
            [0.468681, -0.183200, -0.864162],
            [-0.283088, -0.102987, -0.953549],
        ],
    ],
    // spin
    [
        [
            [-0.000028, -0.000012, 1.000000],
            [-0.824050, 0.512872, -0.240634],
            [-0.857702, 0.448816, -0.250820],
            [0.809279, -0.535876, -0.240634],
            [0.765037, -0.593133, -0.250820],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [0.000001, 0.000030, 1.000000],
            [0.809279, 0.535876, -0.240634],
            [0.765037, 0.593133, -0.250820],
            [-0.824050, -0.512872, -0.240634],
            [-0.857702, -0.448816, -0.250820],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // arms-cross
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.889204, -0.369101, -0.270335],
            [0.784876, -0.443342, 0.432918],
            [0.757437, 0.277042, -0.591216],
            [0.932564, 0.347383, 0.098235],
            [0.295506, 0.007019, -0.955315],
            [-0.234106, -0.000298, -0.972211],
            [0.295506, -0.007019, -0.955315],
            [-0.234106, 0.000298, -0.972211],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.757437, -0.277042, -0.591216],
            [0.932564, -0.347383, 0.098235],
            [0.889204, 0.369101, -0.270335],
            [0.784876, 0.443342, 0.432918],
            [0.295506, 0.007019, -0.955315],
            [-0.234106, -0.000298, -0.972211],
            [0.295506, -0.007019, -0.955315],
            [-0.234106, 0.000298, -0.972211],
        ],
    ],
    // one-arm-up-l
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [0.158161, 0.791835, 0.589901],
            [0.086372, 0.805104, 0.586812],
            [-0.111707, -0.418268, -0.901428],
            [0.038169, -0.395575, -0.917640],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [-0.111707, 0.418268, -0.901428],
            [0.038169, 0.395575, -0.917640],
            [0.158161, -0.791835, 0.589901],
            [0.086372, -0.805104, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // one-arm-up-r
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [-0.111707, 0.418268, -0.901428],
            [0.038169, 0.395575, -0.917640],
            [0.158161, -0.791835, 0.589901],
            [0.086372, -0.805104, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [0.158161, 0.791835, 0.589901],
            [0.086372, 0.805104, 0.586812],
            [-0.111707, -0.418268, -0.901428],
            [0.038169, -0.395575, -0.917640],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // point-l
    [
        [
            [-0.000029, 0.000009, 1.000000],
            [0.901646, 0.428655, 0.057356],
            [0.904374, 0.426455, -0.015631],
            [-0.411380, -0.332956, -0.848473],
            [-0.281399, -0.289134, -0.914995],
            [0.295506, 0.007019, -0.955315],
            [-0.234106, -0.000298, -0.972211],
            [0.295506, -0.007019, -0.955315],
            [-0.234106, 0.000298, -0.972211],
        ],
        [
            [-0.000019, 0.000024, 1.000000],
            [-0.411380, 0.332956, -0.848473],
            [-0.281399, 0.289134, -0.914995],
            [0.901646, -0.428655, 0.057356],
            [0.904374, -0.426455, -0.015631],
            [0.295506, 0.007019, -0.955315],
            [-0.234106, -0.000298, -0.972211],
            [0.295506, -0.007019, -0.955315],
            [-0.234106, 0.000298, -0.972211],
        ],
    ],
    // point-r
    [
        [
            [-0.000019, 0.000024, 1.000000],
            [-0.411380, 0.332956, -0.848473],
            [-0.281399, 0.289134, -0.914995],
            [0.901646, -0.428655, 0.057356],
            [0.904374, -0.426455, -0.015631],
            [0.295506, 0.007019, -0.955315],
            [-0.234106, -0.000298, -0.972211],
            [0.295506, -0.007019, -0.955315],
            [-0.234106, 0.000298, -0.972211],
        ],
        [
            [-0.000029, 0.000009, 1.000000],
            [0.901646, 0.428655, 0.057356],
            [0.904374, 0.426455, -0.015631],
            [-0.411380, -0.332956, -0.848473],
            [-0.281399, -0.289134, -0.914995],
            [0.295506, 0.007019, -0.955315],
            [-0.234106, -0.000298, -0.972211],
            [0.295506, -0.007019, -0.955315],
            [-0.234106, 0.000298, -0.972211],
        ],
    ],
    // hands-on-hips
    [
        [
            [-0.000025, 0.000017, 1.000000],
            [-0.291619, 0.619195, -0.729079],
            [0.656639, -0.306801, -0.688983],
            [-0.291619, -0.619195, -0.729079],
            [0.656639, 0.306801, -0.688983],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.000017, 1.000000],
            [-0.291619, 0.619195, -0.729079],
            [0.656639, -0.306801, -0.688983],
            [-0.291619, -0.619195, -0.729079],
            [0.656639, 0.306801, -0.688983],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // lean-l
    [
        [
            [-0.000025, 0.334105, 0.942536],
            [-0.112997, -0.015576, -0.993473],
            [0.037135, -0.041583, -0.998445],
            [0.158161, -0.499610, 0.851690],
            [0.086372, -0.513035, 0.854011],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, -0.334073, 0.942547],
            [0.158161, 0.499610, 0.851690],
            [0.086372, 0.513035, 0.854011],
            [-0.112997, 0.015576, -0.993473],
            [0.037135, 0.041583, -0.998445],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // lean-r
    [
        [
            [-0.000025, -0.334073, 0.942547],
            [0.158161, 0.499610, 0.851690],
            [0.086372, 0.513035, 0.854011],
            [-0.112997, 0.015576, -0.993473],
            [0.037135, 0.041583, -0.998445],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000025, 0.334105, 0.942536],
            [-0.112997, -0.015576, -0.993473],
            [0.037135, -0.041583, -0.998445],
            [0.158161, -0.499610, 0.851690],
            [0.086372, -0.513035, 0.854011],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // reach-up-twist-l
    [
        [
            [-0.000029, -0.000006, 1.000000],
            [-0.457835, 0.665134, 0.589901],
            [-0.517370, 0.622880, 0.586812],
            [0.678219, -0.438219, 0.589901],
            [0.637722, -0.498962, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000005, 0.000030, 1.000000],
            [0.678219, 0.438219, 0.589901],
            [0.637722, 0.498962, 0.586812],
            [-0.457835, -0.665134, 0.589901],
            [-0.517370, -0.622880, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
    // reach-up-twist-r
    [
        [
            [-0.000005, 0.000030, 1.000000],
            [0.678219, 0.438219, 0.589901],
            [0.637722, 0.498962, 0.586812],
            [-0.457835, -0.665134, 0.589901],
            [-0.517370, -0.622880, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
        [
            [-0.000029, -0.000006, 1.000000],
            [-0.457835, 0.665134, 0.589901],
            [-0.517370, 0.622880, 0.586812],
            [0.678219, -0.438219, 0.589901],
            [0.637722, -0.498962, 0.586812],
            [0.099823, 0.007019, -0.994980],
            [-0.187881, -0.000298, -0.982192],
            [0.099823, -0.007019, -0.994980],
            [-0.187881, 0.000298, -0.982192],
        ],
    ],
];

/// The moves the routine's opening draws from: indices into `MOVES`.
pub const BASIC_MOVES: [usize; 6] = [0, 1, 2, 3, 4, 5];
/// The arm moves the second tier adds: arms-cross, one-arm-up, hands on hips.
pub const ARM_MOVES: [usize; 4] = [10, 11, 12, 15];

// ── Freestyle ────────────────────────────────────────────────────────

/// The joints a freestyle bar counts (legs, waist, shoulders and elbows; not
/// the wrists), by motor index, and which side each is on (0 left, 1 right,
/// 2 the waist).
pub const FREESTYLE_JOINTS: [(usize, u8); 23] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 1),
    (7, 1),
    (8, 1),
    (9, 1),
    (10, 1),
    (11, 1),
    (12, 2),
    (13, 2),
    (14, 2),
    (15, 0),
    (16, 0),
    (17, 0),
    (18, 0),
    (22, 1),
    (23, 1),
    (24, 1),
    (25, 1),
];
/// A joint takes part in a freestyle bar once it has swept this far a beat
/// of the bar's window, radians: 0.6 over the bar's 3.2 beats…
pub const FREESTYLE_JOINT_TRAVEL_PER_BEAT: f32 = 0.1875;
/// …and the bar's amplitude is full at a mean sweep per joint of this a
/// beat: 1.0 over the bar. The window is in beats, so the bar asks the same
/// at every tempo; a slow one gives more time to do it.
pub const FREESTYLE_AMPLITUDE_PER_BEAT: f32 = 0.3125;
/// Over a whole freestyle bar: its window, both sides, in beats.
pub const FREESTYLE_BEATS: f32 = 2.0 * LONG_WINDOW_BEATS;
pub const FREESTYLE_JOINT_TRAVEL: f32 = FREESTYLE_JOINT_TRAVEL_PER_BEAT * FREESTYLE_BEATS;
pub const FREESTYLE_AMPLITUDE_FULL: f32 = FREESTYLE_AMPLITUDE_PER_BEAT * FREESTYLE_BEATS;

// ── Keys and names ───────────────────────────────────────────────────

pub const SCORE: StatKey = StatKey("score");
pub const HITS: StatKey = StatKey("hits");
pub const FALLS: StatKey = StatKey("falls");
pub const BEST_COMBO: StatKey = StatKey("best_combo");
pub const SCORE_SHARE: StatKey = StatKey("score_share");

pub const SCORE_SERIES: SeriesKey = SeriesKey("score");
pub const COMBO_SERIES: SeriesKey = SeriesKey("combo");
pub const QUALITY_SERIES: SeriesKey = SeriesKey("quality");

pub const HIT: EventKey = EventKey("hit");
pub const COMBO: EventKey = EventKey("combo");
pub const FALL: EventKey = EventKey("fall");
pub const UP: EventKey = EventKey("up");

/// Input names, in declared order.
pub const IN_JOINTS: &str = "joints";
pub const IN_JOINT_VELOCITIES: &str = "joint_velocities";
pub const IN_IMU: &str = "imu";
pub const IN_CARDS: &str = "cards";
pub const IN_DANCE: &str = "dance";
/// The one output.
pub const OUT_TARGETS: &str = "targets";

/// Values per card in `cards`: 29 targets, kind, seconds to its beat, window.
pub const CARD_VALUES: usize = 32;
/// Where each part of a card sits within its 32 values.
pub const CARD_KIND: usize = 29;
pub const CARD_SECONDS: usize = 30;
pub const CARD_WINDOW: usize = 31;
/// What `cards` shows when no card is left: the start stance, a pose, this
/// many seconds away, with no window.
pub const NO_CARD_SECONDS: f32 = 10.0;

/// The `dance` input's values, in order.
pub const DANCE_TIME_LEFT: usize = 0;
pub const DANCE_SCORE: usize = 1;
pub const DANCE_OPPONENT_SCORE: usize = 2;
pub const DANCE_COMBO: usize = 3;
pub const DANCE_FALLS: usize = 4;
pub const DANCE_DOWN: usize = 5;
pub const DANCE_UP_IN: usize = 6;
pub const DANCE_LAST_QUALITY: usize = 7;
pub const DANCE_BEAT_PHASE: usize = 8;
pub const DANCE_BEAT_PERIOD: usize = 9;

/// Bounds of the input values that aren't joint ranges. The engine clamps what
/// it reports into these, so a trainer's normalisation can rely on them.
pub const JOINT_VELOCITY_BOUND: f32 = 30.0;
pub const GYRO_BOUND: f32 = 20.0;
pub const VELOCITY_BOUND: f32 = 10.0;
pub const SCORE_BOUND: f32 = 20_000.0;
pub const FALLS_BOUND: f32 = 50.0;
pub const SECONDS_TO_BEAT_MIN: f32 = -1.0;

fn per_joint(ranges: &[(f32, f32); 29]) -> Bounds {
    Bounds::PerElement(ranges.to_vec())
}

fn joint_slices(unit: &str, what: &str) -> Vec<SliceDecl> {
    vec![
        slice(
            "left_leg",
            6,
            unit,
            &format!("Left hip pitch, roll and yaw, knee, ankle pitch and roll: {what}."),
        ),
        slice(
            "right_leg",
            6,
            unit,
            &format!("Right hip pitch, roll and yaw, knee, ankle pitch and roll: {what}."),
        ),
        slice(
            "waist",
            3,
            unit,
            &format!("Waist yaw, roll and pitch: {what}."),
        ),
        slice(
            "left_arm",
            7,
            unit,
            &format!(
                "Left shoulder pitch, roll and yaw, elbow, wrist roll, pitch and yaw: {what}."
            ),
        ),
        slice(
            "right_arm",
            7,
            unit,
            &format!(
                "Right shoulder pitch, roll and yaw, elbow, wrist roll, pitch and yaw: {what}."
            ),
        ),
    ]
}

fn card_bounds() -> Vec<(f32, f32)> {
    let mut one: Vec<(f32, f32)> = JOINT_RANGES.to_vec();
    one.push((0.0, 2.0));
    one.push((SECONDS_TO_BEAT_MIN, NO_CARD_SECONDS));
    one.push((0.0, LONG_WINDOW_BEATS * beat_period(BPM_MIN)));
    let mut both = one.clone();
    both.extend(one);
    both
}

fn card_slices(which: &str) -> Vec<SliceDecl> {
    vec![
        slice(
            &format!("{which}_targets"),
            29,
            "rad",
            &format!(
                "The {which} card's pose as joint targets in motor order: mirrored already on a mirror card, the start stance on a freestyle bar."
            ),
        ),
        slice(
            &format!("{which}_kind"),
            1,
            "kind",
            &format!(
                "The {which} card's kind: 0 a pose, 1 a hold (a long window), 2 a freestyle bar (no pose)."
            ),
        ),
        slice(
            &format!("{which}_seconds"),
            1,
            "s",
            &format!(
                "Seconds until the {which} card's beat; negative once it has passed, while its window is still open."
            ),
        ),
        slice(
            &format!("{which}_window"),
            1,
            "s",
            &format!("The {which} card's window: it scores this long either side of its beat."),
        ),
    ]
}

pub fn declaration() -> Declaration {
    Declaration {
        arena_id: "dance-off".into(),
        arena_version: VERSION.into(),
        changelog: vec![
            ChangelogEntry { version: VERSION.into(), changes: "The seed draws the tempo, 84 to 150 beats a minute, and the routine is built from phrases over twenty moves, ten of them new, rising in difficulty; windows are in beats. `dance` gains the beat's period (payload schema 2); form is the score as a share of the routine's most.".into() },
            ChangelogEntry { version: "0.1.0".into(), changes: "First version.".into() },
        ],
        payload_schema_version: SCHEMA,
        entrants: Entrants::Exact(2),
        inputs: vec![
            ValueDecl::new(IN_JOINTS, Dtype::F32, &[29], per_joint(&JOINT_RANGES),
                "Each joint's angle, from its encoder, in motor order: left leg, right leg, waist, left arm, right arm.")
                .with_slices(joint_slices("rad", "the angle")),
            ValueDecl::f32_slices(IN_JOINT_VELOCITIES, "Each joint's angular velocity, in motor order.", -JOINT_VELOCITY_BOUND, JOINT_VELOCITY_BOUND,
                joint_slices("rad/s", "the angular velocity")),
            ValueDecl::new(IN_IMU, Dtype::F32, &[9], Bounds::PerElement(
                [(-1.0, 1.0); 3].into_iter().chain([(-GYRO_BOUND, GYRO_BOUND); 3]).chain([(-VELOCITY_BOUND, VELOCITY_BOUND); 3]).collect()),
                "What the pelvis IMU and the robot's own state estimate give.")
                .with_slices(vec![
                    slice("gravity", 3, "unit", "Which way is down, in the pelvis frame. Upright is (0, 0, -1)."),
                    slice("gyro", 3, "rad/s", "The pelvis's angular velocity, in the pelvis frame."),
                    slice("velocity", 3, "m/s", "The pelvis's velocity in the heading frame: x forward, y left, z up."),
                ]),
            ValueDecl::new(IN_CARDS, Dtype::F32, &[2 * CARD_VALUES as u32], Bounds::PerElement(card_bounds()),
                "The board: the current card (the earliest whose window is still open) and the next. When no card is left, the start stance, a pose, ten seconds away, with no window.")
                .with_slices(card_slices("current").into_iter().chain(card_slices("next")).collect()),
            ValueDecl::new(IN_DANCE, Dtype::F32, &[10], Bounds::PerElement(vec![
                (0.0, SESSION_SECONDS as f32), (0.0, SCORE_BOUND), (0.0, SCORE_BOUND), (0.0, MAX_CARDS as f32),
                (0.0, FALLS_BOUND), (0.0, 1.0), (0.0, DOWN_SECONDS), (0.0, 1.0), (0.0, 1.0),
                (beat_period(BPM_MAX), beat_period(BPM_MIN)),
            ]), "The scoreboard and the music.")
                .with_slices(vec![
                    slice("time_left", 1, "s", "Seconds left in the session."),
                    slice("score", 1, "points", "Its own score."),
                    slice("opponent_score", 1, "points", "The opponent's score."),
                    slice("combo", 1, "cards", "Its combo: cards in a row at 0.6 quality or better."),
                    slice("falls", 1, "count", "How many times it has fallen."),
                    slice("down", 1, "flag", "1 while it's down after a fall."),
                    slice("up_in", 1, "s", "Seconds until it's stood back up; 0 when it isn't down."),
                    slice("last_quality", 1, "0 to 1", "The quality of its last resolved card."),
                    slice("beat_phase", 1, "0 to 1", "Where the music is within the current beat: 0 on the beat."),
                    slice("beat_period", 1, "s", "Seconds a beat: the session's tempo, drawn from 84 to 150 beats a minute (0.4 to 0.714 s)."),
                ]),
        ],
        outputs: vec![ValueDecl::new(OUT_TARGETS, Dtype::F32, &[29], per_joint(&JOINT_RANGES),
            "Each joint's target angle, in motor order. The motors track it with the G1's published gains and torque limits.")
            .with_slices(joint_slices("rad", "the target angle"))],
        no_action: "the dancer holds its last accepted joint targets, the start stance at first".into(),
        control_rate: CONTROL_RATE,
        budgets: Budgets { turn_fuel: 2_000_000, init_fuel: 32_000_000 },
        max_turns: MAX_TURNS,
        wire: WireSizes { max_input_bytes: 1024, max_output_bytes: 256, max_frame_bytes: 8192 },
        stats: vec![
            StatDecl { key: SCORE.0.into(), label: "Score".into(), unit: "points".into(), kind: StatKind::Score, form: Aggregate::Mean, better: Better::Higher,
                doc: "Points from cards, less 50 for each fall.".into() },
            StatDecl { key: HITS.0.into(), label: "Cards hit".into(), unit: "cards".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Higher,
                doc: "Cards that scored: quality 0.3 or better.".into() },
            StatDecl { key: FALLS.0.into(), label: "Falls".into(), unit: "falls".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Lower,
                doc: "Times this dancer fell.".into() },
            StatDecl { key: BEST_COMBO.0.into(), label: "Best combo".into(), unit: "cards".into(), kind: StatKind::Measure, form: Aggregate::Mean, better: Better::Higher,
                doc: "The longest run of cards at 0.6 quality or better.".into() },
            StatDecl { key: SCORE_SHARE.0.into(), label: "Score share".into(), unit: "share".into(), kind: StatKind::Ratio, form: Aggregate::Mean, better: Better::Higher,
                doc: "The score as a share of the most the routine could score: every card perfect with the combo unbroken. Routines differ in length with their tempo, so this compares dancers better than a score.".into() },
        ],
        series: vec![
            SeriesDecl { key: SCORE_SERIES.0.into(), label: "Score".into(), unit: "points".into(), scope: SeriesScope::PerSeat, doc: "Each dancer's score so far.".into() },
            SeriesDecl { key: COMBO_SERIES.0.into(), label: "Combo".into(), unit: "cards".into(), scope: SeriesScope::PerSeat, doc: "Each dancer's current combo.".into() },
            SeriesDecl { key: QUALITY_SERIES.0.into(), label: "Pose quality".into(), unit: "0 to 1".into(), scope: SeriesScope::PerSeat,
                doc: "How close each dancer is to the live card's pose this turn; 0 between windows.".into() },
        ],
        events: vec![
            EventDecl { key: HIT.0.into(), label: "Card hit".into(), doc: "A card scored.".into() },
            EventDecl { key: COMBO.0.into(), label: "Combo up".into(), doc: "A dancer's combo multiplier rose.".into() },
            EventDecl { key: FALL.0.into(), label: "Fall".into(), doc: "A dancer fell.".into() },
            EventDecl { key: UP.0.into(), label: "Back up".into(), doc: "A fallen dancer was stood back on its mark.".into() },
        ],
        shaping_reward: "Each turn: a tenth of each point scored; +0.05 x the live card's quality x the beat curve; -5 on a fall; +0.01 while standing.".into(),
        min_trainer_version: "0.1.0".into(),
    }
}
