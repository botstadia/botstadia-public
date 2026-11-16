//! Dance-Off's world: two Unitree G1s dance one seeded routine side by side.
//! The rules are in the design note (`../README.md`): pose cards on the beat,
//! scored by how close each dancer's key bones point to the card's over a
//! window around the beat, a combo multiplier, and a fall penalty with a
//! stand-up.

use arena_kit::mujoco::MjSim;
use arena_kit::replay_wire::Frame;
use arena_kit::sim::{ContactTracker, frame};
use arena_kit::{Finish, Report, Seed, Values, World};
use dance_off_declaration::*;

/// The baked model, embedded: a new `.mjb` is a new arena version.
pub const MODEL: &[u8] = include_bytes!("../../model/world.mjb");
/// Physics steps per turn: 50 turns a second at a 4 ms timestep.
pub const SUBSTEPS: u32 = 5;
pub const TURN_SECONDS: f32 = 1.0 / CONTROL_RATE as f32;
/// Contacts weaker than this stay out of the replay frame.
pub const CONTACT_THRESHOLD: f32 = 20.0;
/// Something but a foot on the stage for this many turns (0.1 s) is a fall…
pub const FALL_TURNS: u32 = 5;
/// …as is the pelvis below this height, in metres.
pub const FALL_HEIGHT: f32 = 0.4;
/// Turns a fallen dancer is down before it's stood back up.
pub const DOWN_TURNS: u32 = (DOWN_SECONDS * CONTROL_RATE as f32) as u32;
/// A card's pose distance is measured against at least this much, radians, so
/// a pose close to the start stance can't be scored on noise.
pub const MIN_POSE_DISTANCE: f64 = 0.3;
/// Posture: quality is multiplied by `POSTURE_FLOOR` at worst, rising to 1 as
/// the pelvis's up axis rises from `POSTURE_TILT_MIN` (60° over) to vertical.
pub const POSTURE_FLOOR: f32 = 0.45;
pub const POSTURE_TILT_MIN: f64 = 0.5;
/// Cards the replay frame carries: the one before the current card (so the
/// board can scroll it off), the current one and the next four.
pub const FRAME_CARDS: usize = 6;

/// The arena section of each replay frame, 44 bytes little-endian, which the
/// player decodes:
///
/// - time left f32; beat index u16 (beats since the start); beat phase u8
///   (0..=255 across the beat), both at the session's tempo
/// - per seat, left then right: score u16, combo u8, flags u8, live quality
///   u8 (0..=255), the last resolved card's points u8. Flags: 1 down, 2 a card
///   hit this turn, 4 a card missed this turn, 8 fell this turn, 16 stood
///   back up this turn, 32 the combo multiplier rose this turn.
/// - `FRAME_CARDS` cards from the one before the current card (from the
///   first card at the start): move u8 (an index into `MOVES`, 255 when no
///   card is there), kind u8 (0 pose, 1 hold, 2 freestyle; +16 when
///   mirrored), turns to its beat i16 (negative once past).
/// - the tempo u8, beats a minute (0.2.0 on: a 0.1.0 frame ends before it,
///   at 120).
pub const ARENA_SECTION_BYTES: usize = 4 + 2 + 1 + 2 * 6 + FRAME_CARDS * 4 + 1;

pub const FLAG_DOWN: u8 = 1;
pub const FLAG_HIT: u8 = 2;
pub const FLAG_MISS: u8 = 4;
pub const FLAG_FALL: u8 = 8;
pub const FLAG_UP: u8 = 16;
pub const FLAG_COMBO: u8 = 32;
pub const KIND_MIRRORED: u8 = 16;

/// Each key bone's two ends, as body names after the seat prefix. The torso's
/// far end is between the shoulders, given as the two shoulder bodies.
const BONE_ENDS: [(&str, &str); 9] = [
    ("pelvis", "shoulders"),
    ("left_shoulder_roll_link", "left_elbow_link"),
    ("left_elbow_link", "left_wrist_yaw_link"),
    ("right_shoulder_roll_link", "right_elbow_link"),
    ("right_elbow_link", "right_wrist_yaw_link"),
    ("left_hip_roll_link", "left_knee_link"),
    ("left_knee_link", "left_ankle_roll_link"),
    ("right_hip_roll_link", "right_knee_link"),
    ("right_knee_link", "right_ankle_roll_link"),
];

/// One card of the routine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Card {
    /// Index into `MOVES`.
    pub move_id: usize,
    pub kind: CardKind,
    pub mirrored: bool,
    /// The turn its beat lands on.
    pub beat: u32,
    /// Its window's half-width at the session's tempo, in turns.
    pub window: u32,
}

impl Card {
    pub fn window(&self) -> u32 {
        self.window
    }

    /// The turn its window closes, when it resolves.
    pub fn closes(&self) -> u32 {
        self.beat + self.window()
    }

    /// The joint targets the card asks for: the move's, mirrored on a mirror
    /// card; the start stance on a freestyle bar.
    pub fn targets(&self) -> [f32; 29] {
        match self.kind {
            CardKind::Freestyle => START_TARGETS,
            _ if self.mirrored => mirror(&MOVES[self.move_id].targets),
            _ => MOVES[self.move_id].targets,
        }
    }
}

/// The tempo from its seed stream: whole beats a minute, `BPM_MIN` to
/// `BPM_MAX`.
pub fn tempo(seed: &Seed) -> u32 {
    BPM_MIN + seed.stream("tempo").below(BPM_MAX - BPM_MIN + 1)
}

/// Whether a move's mirror is a different pose.
fn asymmetric(move_id: usize) -> bool {
    mirror(&MOVES[move_id].targets) != MOVES[move_id].targets
}

/// The moves a card at `index` draws from: the basics, then the arm moves
/// too, then every move.
fn tier(index: usize) -> Vec<usize> {
    if index < RAMP_CARDS {
        BASIC_MOVES.to_vec()
    } else if index < TIER_3_CARDS {
        BASIC_MOVES.iter().chain(&ARM_MOVES).copied().collect()
    } else {
        (0..MOVES.len()).collect()
    }
}

/// A card's pose: what the dancer is asked to be in.
fn pose_of(move_id: usize, mirrored: bool) -> [f32; 29] {
    if mirrored {
        mirror(&MOVES[move_id].targets)
    } else {
        MOVES[move_id].targets
    }
}

/// One phrase of four pose cards from `index`: a pattern of moves (`ABAB`,
/// `ABAC`, `ABCB`, or a move then its mirror twice over, `A A' B B'`), with
/// the odd hold or mirror after the plain opening. No pose follows itself,
/// across the phrase before it too.
fn phrase(
    rng: &mut arena_kit::Rng,
    index: usize,
    previous: Option<[f32; 29]>,
) -> [(usize, CardKind, bool); PHRASE] {
    let pool = tier(index);
    let plain = index < PLAIN_CARDS;
    loop {
        let pattern = rng.below(if plain { 3 } else { 4 });
        let draw =
            |rng: &mut arena_kit::Rng, from: &[usize]| from[rng.below(from.len() as u32) as usize];
        let mut slots: [(usize, CardKind, bool); PHRASE] = [(0, CardKind::Pose, false); PHRASE];
        if pattern == 3 {
            let asym: Vec<usize> = pool.iter().copied().filter(|m| asymmetric(*m)).collect();
            let (a, b) = (draw(rng, &asym), draw(rng, &asym));
            slots = [
                (a, CardKind::Pose, false),
                (a, CardKind::Pose, true),
                (b, CardKind::Pose, false),
                (b, CardKind::Pose, true),
            ];
        } else {
            let (a, b, c) = (draw(rng, &pool), draw(rng, &pool), draw(rng, &pool));
            let moves = match pattern {
                0 => [a, b, a, b],
                1 => [a, b, a, c],
                _ => [a, b, c, b],
            };
            for (slot, m) in slots.iter_mut().zip(moves) {
                *slot = (m, CardKind::Pose, false);
                if !plain && rng.below(4) == 0 {
                    if rng.below(2) == 0 && asymmetric(m) {
                        slot.2 = true;
                    } else {
                        slot.1 = CardKind::Hold;
                    }
                }
            }
        }
        let mut last = previous;
        let mut ok = true;
        for (m, _, mirrored) in &slots {
            let pose = pose_of(*m, *mirrored);
            ok &= last != Some(pose);
            last = Some(pose);
        }
        if ok {
            return slots;
        }
    }
}

/// The session's tempo and routine, by the design note's rules: three plain
/// phrases, then two phrases and a freestyle bar over and over, every card
/// whose window closes before the end.
pub fn routine(seed: &Seed) -> (u32, Vec<Card>) {
    let bpm = tempo(seed);
    (bpm, routine_at(seed, bpm))
}

/// The routine from the seed, at `bpm`.
pub fn routine_at(seed: &Seed, bpm: u32) -> Vec<Card> {
    let mut rng = seed.stream("routine");
    let mut cards: Vec<Card> = Vec::with_capacity(MAX_CARDS);
    let mut previous: Option<[f32; 29]> = None;
    let mut pending: Vec<(usize, CardKind, bool)> = Vec::new();
    loop {
        let index = cards.len();
        let (move_id, kind, mirrored) = if is_freestyle(index) {
            (0, CardKind::Freestyle, false)
        } else {
            if pending.is_empty() {
                pending = phrase(&mut rng, index, previous).to_vec();
                pending.reverse();
            }
            let slot = pending.pop().expect("a phrase has cards");
            previous = Some(pose_of(slot.0, slot.2));
            slot
        };
        let card = Card {
            move_id,
            kind,
            mirrored,
            beat: card_beat_turn(index, bpm),
            window: kind.window_turns(bpm),
        };
        if card.closes() > MAX_TURNS {
            return cards;
        }
        cards.push(card);
    }
}

/// The beat curve: a raised cosine over ±`window` turns, 1 on the beat.
pub fn beat_weight(dt: i64, window: u32) -> f32 {
    let w = i64::from(window);
    if w == 0 || dt.abs() > w {
        return 0.0;
    }
    0.5 * (1.0 + (std::f32::consts::PI * dt as f32 / w as f32).cos())
}

/// The beat curve's total over a whole window: what holding a perfect pose
/// through it accumulates.
pub fn beat_total(window: u32) -> f32 {
    let w = i64::from(window);
    (-w..=w).map(|dt| beat_weight(dt, window)).sum()
}

/// A card's points at resolved quality `quality` with `combo` cards in hand.
pub fn card_points(quality: f32, combo: u32) -> u32 {
    if quality < MISS_QUALITY {
        0
    } else {
        (CARD_POINTS * quality * combo_multiplier(combo)).round() as u32
    }
}

/// A freestyle bar's quality from each counted joint's sweep: amplitude ×
/// coverage × left/right balance. A fall zeroes it.
pub fn freestyle_quality(travel: &[f32; 23], fell: bool) -> f32 {
    if fell {
        return 0.0;
    }
    let n = FREESTYLE_JOINTS.len() as f32;
    let total: f32 = travel.iter().sum();
    let amplitude = (total / n / FREESTYLE_AMPLITUDE_FULL).clamp(0.0, 1.0);
    let coverage = travel
        .iter()
        .filter(|t| **t >= FREESTYLE_JOINT_TRAVEL)
        .count() as f32
        / n;
    let side = |s: u8| -> f32 {
        FREESTYLE_JOINTS
            .iter()
            .zip(travel)
            .filter(|((_, side), _)| *side == s)
            .map(|(_, t)| *t)
            .sum()
    };
    let (left, right) = (side(0), side(1));
    let balance = if left + right <= 1e-6 {
        0.0
    } else {
        1.0 - (left - right).abs() / (left + right)
    };
    amplitude * coverage * balance
}

/// What one seat's model elements are.
struct SeatIds {
    pelvis: usize,
    /// Each key bone's ends; the torso's far end averages two bodies.
    bones: [([usize; 2], [usize; 2]); 9],
    foot_geoms: Vec<usize>,
    bodies: std::ops::Range<usize>,
    qpos: [usize; 29],
    qvel: [usize; 29],
}

impl SeatIds {
    fn resolve(sim: &MjSim, prefix: &str) -> Result<Self, String> {
        let body = |name: &str| {
            sim.body_id(&format!("{prefix}_{name}"))
                .ok_or_else(|| format!("no body {prefix}_{name}"))
        };
        let end = |name: &str| -> Result<[usize; 2], String> {
            if name == "shoulders" {
                Ok([
                    body("left_shoulder_pitch_link")?,
                    body("right_shoulder_pitch_link")?,
                ])
            } else {
                let id = body(name)?;
                Ok([id, id])
            }
        };
        let mut bones = [([0; 2], [0; 2]); 9];
        for (bone, (a, b)) in bones.iter_mut().zip(BONE_ENDS) {
            *bone = (end(a)?, end(b)?);
        }
        let mut qpos = [0; 29];
        let mut qvel = [0; 29];
        for (index, joint) in JOINTS.iter().enumerate() {
            let id = sim
                .joint_id(&format!("{prefix}_{joint}_joint"))
                .ok_or_else(|| format!("no joint {prefix}_{joint}_joint"))?;
            (qpos[index], qvel[index]) = sim.joint_addr(id);
        }
        let mut foot_geoms = Vec::new();
        for side in ["left", "right"] {
            for name in [
                "foot_box_collision",
                "foot1_collision",
                "foot2_collision",
                "foot3_collision",
            ] {
                let geom = format!("{prefix}_{side}_{name}");
                foot_geoms.push(sim.geom_id(&geom).ok_or(format!("no geom {geom}"))?);
            }
        }
        let first = body("pelvis")?;
        let last = body("right_wrist_yaw_link")?;
        Ok(Self {
            pelvis: first,
            bones,
            foot_geoms,
            bodies: first..last + 1,
            qpos,
            qvel,
        })
    }

    /// The seat's free joint and its 29 joints, as qpos and qvel ranges.
    fn state_ranges(&self) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        (
            self.qpos[0] - 7..self.qpos[28] + 1,
            self.qvel[0] - 6..self.qvel[28] + 1,
        )
    }
}

/// A target pose's key-bone directions in the heading frame, the weights
/// normalised, and how far the start stance is from it.
#[derive(Debug, Clone, Copy)]
struct Target {
    dirs: [[f64; 3]; 9],
    weights: [f64; 9],
    reach: f64,
}

impl Target {
    /// From a pose's key-bone directions and weights; `stance` gives its
    /// reach.
    fn new(dirs: &KeyDirs, weights: &[f32; 9], stance: Option<&Target>) -> Self {
        let total: f32 = weights.iter().sum();
        let mut target = Target {
            dirs: dirs.map(|d| d.map(f64::from)),
            weights: weights.map(|w| f64::from(w / total)),
            reach: 0.0,
        };
        if let Some(stance) = stance {
            target.reach = distance(&stance.dirs, &target);
        }
        target
    }
}

/// One card's running score for one dancer.
#[derive(Debug, Clone, Copy, Default)]
struct CardScore {
    sum: f32,
    travel: [f32; 23],
    fell: bool,
}

#[derive(Debug, Clone)]
struct Dancer {
    targets: [f64; 29],
    score: u32,
    combo: u32,
    best_combo: u32,
    hits: u32,
    falls: u32,
    /// Turns until it's stood back up; 0 when it's dancing.
    down: u32,
    touch_turns: u32,
    live_quality: f32,
    last_quality: f32,
    last_points: u32,
    flags: u8,
    cards: Vec<CardScore>,
    joints: [f32; 29],
}

impl Dancer {
    fn new(cards: usize) -> Self {
        Self {
            targets: START_TARGETS.map(f64::from),
            score: 0,
            combo: 0,
            best_combo: 0,
            hits: 0,
            falls: 0,
            down: 0,
            touch_turns: 0,
            live_quality: 0.0,
            last_quality: 0.0,
            last_points: 0,
            flags: 0,
            cards: vec![CardScore::default(); cards],
            joints: START_TARGETS,
        }
    }
}

pub struct Arena {
    sim: MjSim,
    turn: u32,
    seats: [SeatIds; 2],
    dancers: [Dancer; 2],
    /// The session's tempo, beats a minute, and its routine.
    bpm: u32,
    cards: Vec<Card>,
    /// Targets for each move as authored and mirrored, and for the stance.
    targets: Vec<[Target; 2]>,
    /// The next card to resolve.
    next_close: usize,
    stage: usize,
    start_key: Option<usize>,
    start_qpos: Vec<f64>,
    contacts: ContactTracker,
    last: Frame,
}

/// `[w, x, y, z]` to a rotation matrix, rows.
fn rotation(q: [f64; 4]) -> [[f64; 3]; 3] {
    let [w, x, y, z] = q;
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

fn apply_transposed(r: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        r[0][0] * v[0] + r[1][0] * v[1] + r[2][0] * v[2],
        r[0][1] * v[0] + r[1][1] * v[1] + r[2][1] * v[2],
        r[0][2] * v[0] + r[1][2] * v[1] + r[2][2] * v[2],
    ]
}

/// A seat's heading frame: the yaw of the pelvis's x axis projected flat.
struct Heading {
    cos: f64,
    sin: f64,
}

impl Heading {
    fn of(sim: &MjSim, pelvis: usize) -> Self {
        let r = rotation(sim.body_quat(pelvis));
        let (fx, fy) = (r[0][0], r[1][0]);
        let norm = (fx * fx + fy * fy).sqrt().max(1e-9);
        Self {
            cos: fx / norm,
            sin: fy / norm,
        }
    }

    /// A world vector in this frame.
    fn vector(&self, v: [f64; 3]) -> [f64; 3] {
        [
            self.cos * v[0] + self.sin * v[1],
            -self.sin * v[0] + self.cos * v[1],
            v[2],
        ]
    }
}

fn clamp(values: &mut [f32], min: f32, max: f32) {
    for value in values {
        *value = if value.is_finite() {
            value.clamp(min, max)
        } else {
            0.0
        };
    }
}

fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
        .clamp(-1.0, 1.0)
        .acos()
}

/// The weighted mean angle between two sets of bone directions.
fn distance(dirs: &[[f64; 3]; 9], target: &Target) -> f64 {
    dirs.iter()
        .zip(&target.dirs)
        .zip(&target.weights)
        .map(|((a, b), w)| w * angle(*a, *b))
        .sum()
}

impl Arena {
    fn pelvis_height(&self, seat: usize) -> f32 {
        self.sim.body_pos(self.seats[seat].pelvis)[2] as f32
    }

    fn mid(&self, ends: [usize; 2]) -> [f64; 3] {
        let (a, b) = (self.sim.body_pos(ends[0]), self.sim.body_pos(ends[1]));
        [
            0.5 * (a[0] + b[0]),
            0.5 * (a[1] + b[1]),
            0.5 * (a[2] + b[2]),
        ]
    }

    /// A seat's key-bone directions, unit vectors in its heading frame.
    fn bone_dirs(&self, seat: usize) -> [[f64; 3]; 9] {
        let ids = &self.seats[seat];
        let heading = Heading::of(&self.sim, ids.pelvis);
        ids.bones.map(|(a, b)| {
            let (pa, pb) = (self.mid(a), self.mid(b));
            let v = heading.vector([pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]]);
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
            [v[0] / n, v[1] / n, v[2] / n]
        })
    }

    /// Quality's posture factor: full upright, falling with the pelvis's tilt.
    fn posture(&self, seat: usize) -> f32 {
        let r = rotation(self.sim.body_quat(self.seats[seat].pelvis));
        let up = r[2][2];
        let upright = ((up - POSTURE_TILT_MIN) / (1.0 - POSTURE_TILT_MIN)).clamp(0.0, 1.0);
        POSTURE_FLOOR + (1.0 - POSTURE_FLOOR) * upright as f32
    }

    fn target_of(&self, card: &Card) -> &Target {
        &self.targets[card.move_id][card.mirrored as usize]
    }

    /// Pose quality against `target`, 0 to 1.
    fn quality(&self, seat: usize, dirs: &[[f64; 3]; 9], target: &Target) -> f32 {
        let d = distance(dirs, target);
        let q = (1.0 - d / target.reach.max(MIN_POSE_DISTANCE)).max(0.0) as f32;
        q * self.posture(seat)
    }

    /// Poses seat 0's model in `targets` to read its key-bone directions, and
    /// puts the state back: how the declaration's table was made.
    #[cfg(test)]
    fn measure(
        &mut self,
        targets: &[f32; 29],
        weights: &[f32; 9],
        stance: Option<&Target>,
    ) -> Target {
        let saved = self.sim.qpos().to_vec();
        let mut qpos = saved.clone();
        for (adr, value) in self.seats[0].qpos.iter().zip(targets) {
            qpos[*adr] = f64::from(*value);
        }
        self.sim.set_qpos(&qpos);
        self.sim.forward();
        let dirs = self.bone_dirs(0);
        self.sim.set_qpos(&saved);
        self.sim.forward();
        let total: f32 = weights.iter().sum();
        let mut target = Target {
            dirs,
            weights: weights.map(|w| f64::from(w / total)),
            reach: 0.0,
        };
        if let Some(stance) = stance {
            target.reach = distance(&stance.dirs, &target);
        }
        target
    }

    fn time_left(&self) -> f32 {
        (MAX_TURNS.saturating_sub(self.turn)) as f32 * TURN_SECONDS
    }

    /// The earliest card whose window hasn't closed.
    fn current_card(&self) -> usize {
        self.next_close
    }

    fn apply_targets(&mut self) {
        let mut ctrl = Vec::with_capacity(58);
        for dancer in &self.dancers {
            ctrl.extend_from_slice(&dancer.targets);
        }
        self.sim.set_ctrl(&ctrl);
    }

    /// The stagehand: back on its mark in the start stance, at rest.
    fn stand_up(&mut self, seat: usize) {
        let (qpos_range, qvel_range) = self.seats[seat].state_ranges();
        let mut qpos = self.sim.qpos().to_vec();
        qpos[qpos_range.clone()].copy_from_slice(&self.start_qpos[qpos_range]);
        let mut qvel = self.sim.qvel().to_vec();
        qvel[qvel_range].fill(0.0);
        self.sim.set_qpos(&qpos);
        self.sim.set_qvel(&qvel);
        self.dancers[seat].targets = START_TARGETS.map(f64::from);
        self.dancers[seat].touch_turns = 0;
        self.apply_targets();
        self.sim.forward();
    }

    fn joints_of(&self, seat: usize) -> [f32; 29] {
        let qpos = self.sim.qpos();
        self.seats[seat].qpos.map(|adr| qpos[adr] as f32)
    }

    fn arena_section(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(ARENA_SECTION_BYTES);
        bytes.extend_from_slice(&self.time_left().to_le_bytes());
        let (beat, phase) = self.beat();
        bytes.extend_from_slice(&(beat.min(u16::MAX as u32) as u16).to_le_bytes());
        bytes.push((phase * 256.0).floor().clamp(0.0, 255.0) as u8);
        for dancer in &self.dancers {
            bytes.extend_from_slice(&(dancer.score.min(u16::MAX as u32) as u16).to_le_bytes());
            bytes.push(dancer.combo.min(255) as u8);
            bytes.push(dancer.flags);
            bytes.push((dancer.live_quality.clamp(0.0, 1.0) * 255.0).round() as u8);
            bytes.push(dancer.last_points.min(255) as u8);
        }
        let first = self.current_card().saturating_sub(1);
        for index in first..first + FRAME_CARDS {
            match self.cards.get(index) {
                Some(card) => {
                    bytes.push(card.move_id as u8);
                    bytes.push(card.kind as u8 + if card.mirrored { KIND_MIRRORED } else { 0 });
                    let to = (i64::from(card.beat) - i64::from(self.turn))
                        .clamp(i64::from(i16::MIN), i64::from(i16::MAX));
                    bytes.extend_from_slice(&(to as i16).to_le_bytes());
                }
                None => {
                    bytes.extend_from_slice(&[255, 0]);
                    bytes.extend_from_slice(&0_i16.to_le_bytes());
                }
            }
        }
        bytes.push(self.bpm.min(255) as u8);
        bytes
    }

    /// Beats since the start, and where within the beat (0 on it, to 1).
    fn beat(&self) -> (u32, f64) {
        let beats = f64::from(self.turn) / turns_per_beat(self.bpm);
        (beats.floor() as u32, beats.fract())
    }

    fn series(&self, report: &mut Report) {
        for seat in 0..2 {
            let dancer = &self.dancers[seat];
            report.seat_series(SCORE_SERIES, seat as u32, dancer.score as f32);
            report.seat_series(COMBO_SERIES, seat as u32, dancer.combo as f32);
            report.seat_series(QUALITY_SERIES, seat as u32, dancer.live_quality);
        }
    }

    fn make_frame(&mut self) {
        let contacts = self.contacts.take(CONTACT_THRESHOLD);
        self.last = frame(
            &self.sim,
            "dance-off",
            self.turn,
            contacts,
            Vec::new(),
            self.arena_section(),
        );
    }

    fn card_input(&self, index: usize) -> [f32; CARD_VALUES] {
        let mut out = [0.0; CARD_VALUES];
        match self.cards.get(index) {
            Some(card) => {
                out[..29].copy_from_slice(&card.targets());
                out[CARD_KIND] = card.kind as u8 as f32;
                out[CARD_SECONDS] = ((card.beat as f32 - self.turn as f32) * TURN_SECONDS)
                    .clamp(SECONDS_TO_BEAT_MIN, NO_CARD_SECONDS);
                out[CARD_WINDOW] = card.window() as f32 * TURN_SECONDS;
            }
            None => {
                out[..29].copy_from_slice(&START_TARGETS);
                out[CARD_SECONDS] = NO_CARD_SECONDS;
            }
        }
        out
    }

    /// Resolves every card whose window closes this turn, for both dancers.
    fn resolve(&mut self, report: &mut Report, shaping: &mut [f32; 2]) {
        while let Some(card) = self.cards.get(self.next_close).copied() {
            if card.closes() > self.turn {
                break;
            }
            let index = self.next_close;
            self.next_close += 1;
            for (seat, reward) in shaping.iter_mut().enumerate() {
                let dancer = &mut self.dancers[seat];
                let scored = dancer.cards[index];
                let quality = match card.kind {
                    CardKind::Freestyle => freestyle_quality(&scored.travel, scored.fell),
                    _ => (scored.sum / beat_total(card.window())).clamp(0.0, 1.0),
                };
                let points = card_points(quality, dancer.combo);
                dancer.last_quality = quality;
                dancer.last_points = points;
                if points > 0 {
                    dancer.score += points;
                    dancer.hits += 1;
                    dancer.flags |= FLAG_HIT;
                    *reward += points as f32 / 10.0;
                    report.event(HIT, Some(seat as u32));
                } else {
                    dancer.flags |= FLAG_MISS;
                }
                if quality >= COMBO_QUALITY {
                    let before = combo_multiplier(dancer.combo);
                    dancer.combo += 1;
                    dancer.best_combo = dancer.best_combo.max(dancer.combo);
                    if combo_multiplier(dancer.combo) > before {
                        dancer.flags |= FLAG_COMBO;
                        report.event(COMBO, Some(seat as u32));
                    }
                } else if quality < MISS_QUALITY {
                    dancer.combo = 0;
                }
            }
        }
    }
}

impl World for Arena {
    fn declaration() -> arena_kit::declaration::Declaration {
        declaration()
    }

    fn setup(seed: &Seed, seats: u32, report: &mut Report) -> Result<Self, String> {
        if seats != 2 {
            return Err(format!("dance-off seats exactly 2 dancers, not {seats}"));
        }
        let mut sim = MjSim::load(MODEL)?;
        let start_key = sim.keyframe_id("start");
        sim.reset(start_key);
        sim.forward();
        let stage = sim.geom_id("stage").ok_or("no stage geom")?;
        let seat_ids = [
            SeatIds::resolve(&sim, SEATS[0])?,
            SeatIds::resolve(&sim, SEATS[1])?,
        ];
        let start_qpos = sim.qpos().to_vec();
        let (bpm, cards) = routine(seed);
        let mut arena = Self {
            sim,
            turn: 0,
            seats: seat_ids,
            dancers: [Dancer::new(cards.len()), Dancer::new(cards.len())],
            bpm,
            cards,
            targets: Vec::new(),
            next_close: 0,
            stage,
            start_key,
            start_qpos,
            contacts: ContactTracker::default(),
            last: Frame::default(),
        };
        // Every move's target bone directions, as authored and mirrored,
        // from the declaration's table; and how far the start stance is
        // from each.
        let stance = Target::new(&STANCE_DIRS, &[1.0; 9], None);
        for (m, dirs) in MOVES.iter().zip(&MOVE_DIRS) {
            let plain = Target::new(&dirs[0], &m.weights, Some(&stance));
            let mirrored = Target::new(&dirs[1], &mirror_weights(&m.weights), Some(&stance));
            arena.targets.push([plain, mirrored]);
        }
        arena.apply_targets();
        arena.series(report);
        arena.make_frame();
        Ok(arena)
    }

    fn input(&self, seat: u32) -> Vec<Values> {
        let me = seat as usize;
        let them = 1 - me;
        let ids = &self.seats[me];
        let qvel = self.sim.qvel();

        let mut joints: Vec<f32> = self.joints_of(me).to_vec();
        for (value, (min, max)) in joints.iter_mut().zip(JOINT_RANGES) {
            *value = if value.is_finite() {
                value.clamp(min, max)
            } else {
                0.0
            };
        }
        let mut velocities: Vec<f32> = ids.qvel.iter().map(|&adr| qvel[adr] as f32).collect();
        clamp(&mut velocities, -JOINT_VELOCITY_BOUND, JOINT_VELOCITY_BOUND);

        let heading = Heading::of(&self.sim, ids.pelvis);
        let r = rotation(self.sim.body_quat(ids.pelvis));
        let gravity = apply_transposed(&r, [0.0, 0.0, -1.0]);
        let gyro = apply_transposed(&r, self.sim.body_angvel(ids.pelvis));
        let velocity = heading.vector(self.sim.body_linvel(ids.pelvis));
        let mut imu: Vec<f32> = Vec::with_capacity(9);
        imu.extend(gravity.map(|v| v as f32));
        let mut gyro: Vec<f32> = gyro.map(|v| v as f32).to_vec();
        clamp(&mut gyro, -GYRO_BOUND, GYRO_BOUND);
        imu.extend(gyro);
        let mut velocity: Vec<f32> = velocity.map(|v| v as f32).to_vec();
        clamp(&mut velocity, -VELOCITY_BOUND, VELOCITY_BOUND);
        imu.extend(velocity);
        clamp(&mut imu[..3], -1.0, 1.0);

        let current = self.current_card();
        let mut cards = Vec::with_capacity(2 * CARD_VALUES);
        cards.extend(self.card_input(current));
        cards.extend(self.card_input(current + 1));

        let (mine, yours) = (&self.dancers[me], &self.dancers[them]);
        let dance = vec![
            self.time_left(),
            (mine.score as f32).min(SCORE_BOUND),
            (yours.score as f32).min(SCORE_BOUND),
            (mine.combo as f32).min(MAX_CARDS as f32),
            (mine.falls as f32).min(FALLS_BOUND),
            (mine.down > 0) as u8 as f32,
            (mine.down as f32 * TURN_SECONDS).min(DOWN_SECONDS),
            mine.last_quality.clamp(0.0, 1.0),
            self.beat().1 as f32,
            beat_period(self.bpm),
        ];

        vec![
            Values::F32(joints),
            Values::F32(velocities),
            Values::F32(imu),
            Values::F32(cards),
            Values::F32(dance),
        ]
    }

    fn step(&mut self, outputs: &[Option<Vec<Values>>], report: &mut Report) {
        self.turn += 1;
        for (seat, output) in outputs.iter().enumerate().take(2) {
            self.dancers[seat].flags = 0;
            if self.dancers[seat].down > 0 {
                continue;
            }
            let values = output
                .as_ref()
                .and_then(|values| values.first())
                .and_then(Values::as_f32)
                .filter(|values| values.len() == 29);
            if let Some(values) = values {
                for ((target, value), (min, max)) in self.dancers[seat]
                    .targets
                    .iter_mut()
                    .zip(values)
                    .zip(JOINT_RANGES)
                {
                    if value.is_finite() {
                        *target = f64::from(value.clamp(min, max));
                    }
                }
            }
        }
        self.apply_targets();

        // Physics, noting anything but a foot on the stage.
        let mut touched = [false; 2];
        for _ in 0..SUBSTEPS {
            self.sim.step();
            self.contacts.observe(&self.sim);
            for contact in self.sim.contacts() {
                let geoms = [contact.geom1, contact.geom2];
                for (index, geom) in geoms.iter().enumerate() {
                    if *geom != self.stage {
                        continue;
                    }
                    let other = geoms[1 - index];
                    let body = self.sim.geom_body(other);
                    for (seat, ids) in self.seats.iter().enumerate() {
                        if ids.bodies.contains(&body) && !ids.foot_geoms.contains(&other) {
                            touched[seat] = true;
                        }
                    }
                }
            }
        }
        if self.sim.diverged() {
            // A rule of the game, never a trap: both dancers back on their
            // marks, nothing counted against either.
            self.sim.reset(self.start_key);
            for dancer in &mut self.dancers {
                dancer.targets = START_TARGETS.map(f64::from);
                dancer.touch_turns = 0;
                dancer.down = 0;
                dancer.joints = START_TARGETS;
            }
            self.apply_targets();
            self.sim.forward();
            touched = [false; 2];
        }

        let mut shaping = [0.0_f32; 2];

        // Falls and stand-ups.
        for (seat, reward) in shaping.iter_mut().enumerate() {
            let low = self.pelvis_height(seat) < FALL_HEIGHT;
            let dancer = &mut self.dancers[seat];
            if dancer.down > 0 {
                dancer.down -= 1;
                dancer.flags |= FLAG_DOWN;
                if dancer.down == 0 {
                    self.stand_up(seat);
                    let dancer = &mut self.dancers[seat];
                    dancer.flags = (dancer.flags & !FLAG_DOWN) | FLAG_UP;
                    dancer.joints = START_TARGETS;
                    report.event(UP, Some(seat as u32));
                }
                continue;
            }
            if touched[seat] || low {
                dancer.touch_turns += 1;
            } else {
                dancer.touch_turns = 0;
            }
            if dancer.touch_turns >= FALL_TURNS || low {
                dancer.down = DOWN_TURNS;
                dancer.touch_turns = 0;
                dancer.falls += 1;
                dancer.score = dancer.score.saturating_sub(FALL_PENALTY);
                dancer.combo = 0;
                dancer.flags |= FLAG_FALL | FLAG_DOWN;
                *reward -= 5.0;
                report.event(FALL, Some(seat as u32));
            }
        }

        // Every live card's running score, for each dancer.
        let first = self.next_close;
        for (seat, reward) in shaping.iter_mut().enumerate() {
            let down = self.dancers[seat].down > 0;
            let fell = self.dancers[seat].flags & FLAG_FALL != 0;
            let dirs = self.bone_dirs(seat);
            let joints = self.joints_of(seat);
            let previous = self.dancers[seat].joints;
            let mut live = 0.0_f32;
            let mut weighted = 0.0_f32;
            for index in first..self.cards.len() {
                let card = self.cards[index];
                let dt = i64::from(self.turn) - i64::from(card.beat);
                if dt < -i64::from(card.window()) {
                    break;
                }
                if dt > i64::from(card.window()) {
                    continue;
                }
                let weight = beat_weight(dt, card.window());
                match card.kind {
                    CardKind::Freestyle => {
                        let scored = &mut self.dancers[seat].cards[index];
                        scored.fell |= fell || down;
                        if !down {
                            for (travel, (joint, _)) in
                                scored.travel.iter_mut().zip(FREESTYLE_JOINTS)
                            {
                                *travel += (joints[joint] - previous[joint]).abs();
                            }
                        }
                    }
                    _ => {
                        let quality = if down {
                            0.0
                        } else {
                            self.quality(seat, &dirs, self.target_of(&card))
                        };
                        self.dancers[seat].cards[index].sum += weight * quality;
                        if weight >= weighted {
                            weighted = weight;
                            live = quality;
                        }
                    }
                }
            }
            let dancer = &mut self.dancers[seat];
            dancer.joints = joints;
            dancer.live_quality = live;
            if !down {
                *reward += 0.05 * weighted * live + 0.01;
            }
        }

        self.resolve(report, &mut shaping);

        for (seat, reward) in shaping.iter().enumerate() {
            report.shaping(seat as u32, *reward);
        }
        self.series(report);
        self.make_frame();
    }

    fn frame(&self) -> Vec<u8> {
        self.last.encode()
    }

    fn finish(&self, out_of_turns: bool) -> Option<Finish> {
        if !out_of_turns && self.turn < MAX_TURNS {
            return None;
        }
        let scores = [self.dancers[0].score, self.dancers[1].score];
        let (hi, lo) = (scores[0].max(scores[1]), scores[0].min(scores[1]));
        let finish = if scores[0] == scores[1] {
            Finish::new(vec![vec![0, 1]], format!("A draw, {hi} to {lo}"))
        } else {
            let winner = if scores[0] > scores[1] { 0 } else { 1 };
            Finish::new(
                vec![vec![winner], vec![1 - winner]],
                format!("Score {hi} to {lo}"),
            )
        };
        let each = |f: fn(&Dancer) -> u32| -> Vec<f64> {
            self.dancers.iter().map(|d| f64::from(f(d))).collect()
        };
        Some(
            finish
                .stat(SCORE, each(|d| d.score))
                .stat(HITS, each(|d| d.hits))
                .stat(FALLS, each(|d| d.falls))
                .stat(BEST_COMBO, each(|d| d.best_combo))
                .stat(
                    SCORE_SHARE,
                    self.dancers
                        .iter()
                        .map(|d| f64::from(d.score as f32 / max_score(self.cards.len())))
                        .collect(),
                ),
        )
    }
}

impl Arena {
    /// For tests and tools: the routine.
    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// For tests and tools: the session's tempo, beats a minute.
    pub fn bpm(&self) -> u32 {
        self.bpm
    }

    /// For tests: each dancer's score, combo, falls and whether it's down.
    pub fn scoreboard(&self) -> [(u32, u32, u32, bool); 2] {
        [0, 1].map(|seat| {
            let d = &self.dancers[seat];
            (d.score, d.combo, d.falls, d.down > 0)
        })
    }

    /// For tests: the seats' pelvis heights.
    pub fn pelvis_heights(&self) -> [f32; 2] {
        [self.pelvis_height(0), self.pelvis_height(1)]
    }
}

#[cfg(test)]
mod tests {
    //! The scoring rules, driven directly.

    use super::*;

    fn arena(seed: u64) -> (Arena, Report) {
        let mut report = Report::new(&declaration(), 2);
        let arena = Arena::setup(&Seed::from_u64(seed), 2, &mut report).unwrap();
        (arena, report)
    }

    #[test]
    fn the_routine_is_seed_stable() {
        let a = routine(&Seed::from_u64(7));
        assert_eq!(a, routine(&Seed::from_u64(7)));
        assert_ne!(a, routine(&Seed::from_u64(8)));
    }

    #[test]
    fn holding_through_the_beat_scores_fully_and_a_snap_almost_nothing() {
        let w = CardKind::Pose.window_turns(DEFAULT_BPM);
        let held: f32 = (-(w as i64)..=w as i64).map(|dt| beat_weight(dt, w)).sum();
        assert!((held / beat_total(w) - 1.0).abs() < 1e-5);
        let snap = beat_weight(0, w) / beat_total(w);
        assert!(snap < MISS_QUALITY, "a one-turn snap scores {snap}");
        let late: f32 = (w as i64 - 5..=w as i64).map(|dt| beat_weight(dt, w)).sum();
        let centred: f32 = (-2..=3).map(|dt| beat_weight(dt, w)).sum();
        assert!(centred > late);
    }

    #[test]
    fn points_and_the_combo_multiplier() {
        assert_eq!(card_points(0.29, 0), 0);
        assert_eq!(card_points(1.0, 0), 100);
        assert_eq!(card_points(1.0, 4), 125);
        assert_eq!(card_points(1.0, 40), 200);
        assert_eq!(card_points(0.5, 8), 75);
    }

    #[test]
    fn mirrored_moves_are_the_other_side() {
        for (l, r) in [(2, 3), (4, 5), (7, 8)] {
            assert_eq!(
                mirror(&MOVES[l].targets),
                MOVES[r].targets,
                "{}",
                MOVES[l].name
            );
            assert_eq!(mirror_weights(&MOVES[l].weights), MOVES[r].weights);
        }
        for m in &MOVES {
            assert_eq!(mirror(&mirror(&m.targets)), m.targets);
        }
    }

    #[test]
    fn every_move_is_within_the_joint_ranges() {
        for m in &MOVES {
            for (value, (min, max)) in m.targets.iter().zip(JOINT_RANGES) {
                assert!((min..=max).contains(value), "{}: {value}", m.name);
            }
        }
    }

    #[test]
    fn the_start_stance_scores_nothing_and_each_pose_scores_fully() {
        let (mut arena, _) = arena(1);
        let stance = arena.bone_dirs(0);
        for (index, m) in MOVES.iter().enumerate() {
            for mirrored in [false, true] {
                let target = arena.targets[index][mirrored as usize];
                assert!(
                    target.reach >= MIN_POSE_DISTANCE * 0.5,
                    "{} reach {}",
                    m.name,
                    target.reach
                );
                // Nothing, to the table's six decimals.
                let still = arena.quality(0, &stance, &target);
                assert!(still < 1e-3, "{}: the stance scores {still}", m.name);
                let posed = if mirrored {
                    mirror(&m.targets)
                } else {
                    m.targets
                };
                let measured = arena.measure(&posed, &[1.0; 9], None);
                let q = arena.quality(0, &measured.dirs, &target);
                assert!(q > 0.99, "{} mirrored {mirrored}: {q}", m.name);
            }
        }
    }

    #[test]
    fn a_mirror_card_scores_the_mirrored_pose() {
        let (mut arena, _) = arena(1);
        let wave = arena.measure(&MOVES[2].targets, &[1.0; 9], None);
        let plain = arena.targets[2][0];
        let mirrored = arena.targets[2][1];
        assert!(arena.quality(0, &wave.dirs, &plain) > 0.99);
        assert!(arena.quality(0, &wave.dirs, &mirrored) < 0.3);
    }

    #[test]
    fn freestyle_rewards_wide_even_movement_and_a_fall_zeroes_it() {
        assert_eq!(freestyle_quality(&[0.0; 23], false), 0.0);
        let full = freestyle_quality(&[FREESTYLE_AMPLITUDE_FULL; 23], false);
        assert!(full > 0.99, "{full}");
        assert_eq!(
            freestyle_quality(&[FREESTYLE_AMPLITUDE_FULL; 23], true),
            0.0
        );
        let mut one_arm = [0.0; 23];
        one_arm[15] = 23.0;
        assert!(freestyle_quality(&one_arm, false) < 0.1);
    }

    #[test]
    fn a_fall_costs_points_and_the_combo_and_the_dancer_is_stood_back_up() {
        let (mut arena, mut report) = arena(1);
        let fall = declaration().event_index(FALL.0).unwrap() as u32;
        let up = declaration().event_index(UP.0).unwrap() as u32;
        arena.dancers[0].score = 120;
        arena.dancers[0].combo = 5;
        // Fold the legs so something but a foot meets the stage.
        let mut targets = START_TARGETS.map(f64::from);
        for (hip, knee) in [(0, 3), (6, 9)] {
            targets[hip] = -2.2;
            targets[knee] = 2.8;
        }
        arena.dancers[0].targets = targets;
        let mut events = Vec::new();
        let mut fell_at = None;
        for turn in 0..400 {
            report.next_turn();
            arena.step(&[None, None], &mut report);
            events.extend(report.events().iter().copied());
            if fell_at.is_none() && arena.dancers[0].falls == 1 {
                fell_at = Some(turn);
                assert_eq!(arena.dancers[0].score, 70);
                assert_eq!(arena.dancers[0].combo, 0);
            }
            if events.contains(&(up, Some(0))) {
                break;
            }
        }
        assert!(events.contains(&(fall, Some(0))), "no fall in {events:?}");
        assert!(
            events.contains(&(up, Some(0))),
            "never stood up: {events:?}"
        );
        assert_eq!(arena.dancers[0].falls, 1);
        assert!(arena.pelvis_heights()[0] > 0.7);
        assert_eq!(arena.dancers[0].targets, START_TARGETS.map(f64::from));
        // The other dancer, holding still, never fell.
        assert_eq!(arena.dancers[1].falls, 0);
    }

    #[test]
    fn the_score_never_goes_below_zero() {
        let (mut arena, mut report) = arena(1);
        arena.dancers[1].score = 20;
        arena.dancers[1].targets[0] = -2.2;
        arena.dancers[1].targets[3] = 2.8;
        arena.dancers[1].targets[6] = -2.2;
        arena.dancers[1].targets[9] = 2.8;
        for _ in 0..200 {
            report.next_turn();
            arena.step(&[None, None], &mut report);
            if arena.dancers[1].falls > 0 {
                break;
            }
        }
        assert_eq!(arena.dancers[1].score, 0);
    }

    #[test]
    fn the_key_bone_table_is_the_models() {
        let (mut arena, _) = arena(1);
        let near = |a: &[[f64; 3]; 9], b: &KeyDirs, what: &str| {
            for (x, y) in a.iter().zip(b) {
                for (u, v) in x.iter().zip(y) {
                    assert!((u - f64::from(*v)).abs() < 1e-5, "{what}: {a:?} vs {b:?}");
                }
            }
        };
        near(
            &arena.measure(&START_TARGETS, &[1.0; 9], None).dirs,
            &STANCE_DIRS,
            "stance",
        );
        for (m, dirs) in MOVES.iter().zip(&MOVE_DIRS) {
            near(
                &arena.measure(&m.targets, &[1.0; 9], None).dirs,
                &dirs[0],
                m.name,
            );
            near(
                &arena.measure(&mirror(&m.targets), &[1.0; 9], None).dirs,
                &dirs[1],
                m.name,
            );
        }
    }

    #[test]
    #[ignore = "prints the key-bone table for the declaration"]
    fn print_key_directions() {
        let (mut arena, _) = arena(1);
        let fmt = |dirs: &[[f64; 3]; 9]| {
            let bones: Vec<String> = dirs
                .iter()
                .map(|d| format!("[{:.6}, {:.6}, {:.6}]", d[0], d[1], d[2]))
                .collect();
            format!("[{}]", bones.join(", "))
        };
        let stance = arena.measure(&START_TARGETS, &[1.0; 9], None);
        println!("pub const STANCE_DIRS: KeyDirs = {};", fmt(&stance.dirs));
        println!("pub const MOVE_DIRS: [[KeyDirs; 2]; MOVE_COUNT] = [");
        for m in &MOVES {
            let plain = arena.measure(&m.targets, &[1.0; 9], None);
            let mirrored = arena.measure(&mirror(&m.targets), &[1.0; 9], None);
            println!(
                "    // {}\n    [{}, {}],",
                m.name,
                fmt(&plain.dirs),
                fmt(&mirrored.dirs)
            );
        }
        println!("];");
    }

    #[test]
    fn the_arena_section_is_the_documented_size() {
        let (arena, _) = arena(1);
        assert_eq!(arena.arena_section().len(), ARENA_SECTION_BYTES);
    }
}
