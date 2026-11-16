//! Humanoid Boxing's world: two Unitree G1s box one round in a square ring.
//! The rules are in the design note (`../README.md`): points for landed
//! punches, a ten count for a knockdown, a knockout on a failed count or a
//! third knockdown, and points when time runs out.

use arena_kit::mujoco::MjSim;
use arena_kit::replay_wire::Frame;
use arena_kit::sim::{ContactTracker, frame};
use arena_kit::{Finish, Report, Seed, Values, World};
use boxing_declaration::*;

/// The baked model, embedded: a new `.mjb` is a new arena version.
pub const MODEL: &[u8] = include_bytes!("../../model/world.mjb");
/// Physics steps per turn: 50 turns a second at a 4 ms timestep.
pub const SUBSTEPS: u32 = 5;
pub const TURN_SECONDS: f32 = 1.0 / CONTROL_RATE as f32;
/// A punch whose peak normal force on the head or body reaches this lands,
/// in newtons. Measured on the padded glove (the design note has the
/// figures): a glove resting on a chest presses under 5 N, and no passive
/// contact reaches 15 N; harder punches score more (`punch_strength`).
pub const LANDING_FORCE: f32 = LANDING_FORCE_N;
/// Contacts weaker than this stay out of the replay frame.
pub const CONTACT_THRESHOLD: f32 = 20.0;
/// After landing, a fist can't score again for this many turns (0.3 s).
pub const PUNCH_COOLDOWN: u32 = 15;
/// A punch is judged on its whole contact: from the glove's fresh touch until
/// it leaves, or for at most this many turns (0.06 s; a committed punch's
/// contact lasts 10 to 20 ms), at the peak normal force over all of it.
pub const LANDING_WINDOW: u32 = 3;
/// Something but a foot on the canvas for this many turns (0.1 s) is a knockdown…
pub const DOWN_TURNS: u32 = 5;
/// …as is the pelvis below this height, in metres.
pub const DOWN_HEIGHT: f32 = 0.3;
/// Up again: pelvis at least this high with only feet down, for this many turns (0.5 s).
pub const UP_HEIGHT: f32 = 0.55;
pub const UP_TURNS: u32 = 25;
/// The most the seed turns the starting pair about the ring's centre, rad.
pub const START_TURN: f64 = std::f64::consts::FRAC_PI_6;
/// The head above the torso link, and the sole's centre on the ankle link.
const HEAD_OFFSET: [f64; 3] = [0.0, 0.0, 0.43];
const FOOT_OFFSET: [f64; 3] = [0.04, 0.0, -0.03];

/// The arena section of each replay frame, 20 bytes little-endian, which the
/// player decodes: time left f32, count left f32, points u16 × 2, knockdowns
/// u8 × 2, down bits, knockout bits, punch landed this turn u8 × 2 (0 none,
/// 1 body, 2 head), and its strength u8 × 2 (0 a landing, 1 solid, 2 power;
/// added in 0.3.0, so 0.2.0's frames are 18 bytes).
pub const ARENA_SECTION_BYTES: usize = 20;

/// What one seat's model elements are.
struct SeatIds {
    pelvis: usize,
    torso: usize,
    /// The glove bodies: the fists, where punches land from.
    gloves: [usize; 2],
    feet: [usize; 2],
    glove_geoms: [usize; 2],
    head_geom: usize,
    torso_geom: usize,
    foot_geoms: Vec<usize>,
    bodies: std::ops::Range<usize>,
    qpos: [usize; 29],
    qvel: [usize; 29],
    /// Each motor's joint, for its torque limit.
    joints: [usize; 29],
}

impl SeatIds {
    fn resolve(sim: &MjSim, prefix: &str) -> Result<Self, String> {
        let body = |name: &str| {
            sim.body_id(&format!("{prefix}_{name}"))
                .ok_or_else(|| format!("no body {prefix}_{name}"))
        };
        let geom = |name: &str| {
            sim.geom_id(&format!("{prefix}_{name}"))
                .ok_or_else(|| format!("no geom {prefix}_{name}"))
        };
        let mut qpos = [0; 29];
        let mut qvel = [0; 29];
        let mut joints = [0; 29];
        for (index, joint) in JOINTS.iter().enumerate() {
            let id = sim
                .joint_id(&format!("{prefix}_{joint}_joint"))
                .ok_or_else(|| format!("no joint {prefix}_{joint}_joint"))?;
            joints[index] = id;
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
                foot_geoms.push(geom(&format!("{side}_{name}"))?);
            }
        }
        let first = body("pelvis")?;
        // Body ids run pelvis first, right glove last, for each fighter.
        let last = body("right_glove")?;
        Ok(Self {
            pelvis: first,
            torso: body("torso_link")?,
            gloves: [body("left_glove")?, body("right_glove")?],
            feet: [
                body("left_ankle_roll_link")?,
                body("right_ankle_roll_link")?,
            ],
            glove_geoms: [
                geom("left_glove_collision")?,
                geom("right_glove_collision")?,
            ],
            head_geom: geom("head_collision")?,
            torso_geom: geom("torso_collision")?,
            foot_geoms,
            bodies: first..last + 1,
            qpos,
            qvel,
            joints,
        })
    }
}

#[derive(Debug, Clone)]
struct Fighter {
    targets: [f64; 29],
    points: u32,
    punches: u32,
    knockdowns: u32,
    down: bool,
    count_left: f32,
    touch_turns: u32,
    up_turns: u32,
    ko: bool,
    fist_cooldown: [u32; 2],
    fist_touching: [bool; 2],
    /// Each fist's punch being judged: peak force, what it hit (1 body,
    /// 2 head), the turns it has been open, and the turn it opened.
    open: [Option<(f32, u8, u32, u32)>; 2],
    fist_speed: f32,
    /// This turn: 0 none, 1 body, 2 head.
    landed: u8,
    /// This turn's landing's strength: 0 a landing, 1 solid, 2 power.
    strength: u8,
    power_punches: u32,
    /// The hardest landed punch's peak force, N.
    hardest: f32,
    /// Each motor's heat, in motor order ([`Motors`]).
    heat: [f64; 29],
}

impl Fighter {
    fn new() -> Self {
        Self {
            targets: START_TARGETS.map(f64::from),
            points: 0,
            punches: 0,
            knockdowns: 0,
            down: false,
            count_left: 0.0,
            touch_turns: 0,
            up_turns: 0,
            ko: false,
            fist_cooldown: [0; 2],
            fist_touching: [false; 2],
            open: [None; 2],
            fist_speed: 0.0,
            landed: 0,
            strength: 0,
            power_punches: 0,
            hardest: 0.0,
            heat: [0.0; 29],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// `loser` failed the count or went down a third time.
    Knockout {
        loser: u32,
        third: bool,
    },
    BothCountedOut,
    Time,
}

pub struct Arena {
    sim: MjSim,
    turn: u32,
    seats: [SeatIds; 2],
    fighters: [Fighter; 2],
    canvas: usize,
    start_key: Option<usize>,
    contacts: ContactTracker,
    ending: Option<Ending>,
    last: Frame,
    /// Every punch that landed, in order.
    landings: Vec<Landing>,
}

/// One landed punch, as judged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landing {
    pub seat: u32,
    /// The fist: 0 left, 1 right.
    pub side: u8,
    /// The turn its glove first touched.
    pub opened: u32,
    /// The turn it was judged on: its contact's end.
    pub turn: u32,
    /// Its peak normal force over the contact, N.
    pub force: f32,
    pub head: bool,
    pub points: u32,
}

/// A landed punch's strength band: 0 a landing, 1 solid, 2 power.
pub fn punch_strength(force: f32) -> u8 {
    if force >= POWER_PUNCH_N {
        2
    } else if force >= SOLID_PUNCH_N {
        1
    } else {
        0
    }
}

/// Points for a landed punch: the target's base (head or body) plus its
/// strength band.
pub fn punch_points(struck: u8, strength: u8) -> u32 {
    let base = if struck == 2 {
        HEAD_POINTS
    } else {
        BODY_POINTS
    };
    base + u32::from(strength)
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

fn apply(r: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        r[0][0] * v[0] + r[0][1] * v[1] + r[0][2] * v[2],
        r[1][0] * v[0] + r[1][1] * v[1] + r[1][2] * v[2],
        r[2][0] * v[0] + r[2][1] * v[1] + r[2][2] * v[2],
    ]
}

fn apply_transposed(r: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        r[0][0] * v[0] + r[1][0] * v[1] + r[2][0] * v[2],
        r[0][1] * v[0] + r[1][1] * v[1] + r[2][1] * v[2],
        r[0][2] * v[0] + r[1][2] * v[1] + r[2][2] * v[2],
    ]
}

/// A seat's heading frame: its pelvis position, and the yaw of the pelvis's
/// x axis projected flat.
struct Heading {
    origin: [f64; 3],
    cos: f64,
    sin: f64,
}

impl Heading {
    fn of(sim: &MjSim, pelvis: usize) -> Self {
        let r = rotation(sim.body_quat(pelvis));
        let (fx, fy) = (r[0][0], r[1][0]);
        let norm = (fx * fx + fy * fy).sqrt().max(1e-9);
        Self {
            origin: sim.body_pos(pelvis),
            cos: fx / norm,
            sin: fy / norm,
        }
    }

    /// A world point in this frame.
    fn point(&self, p: [f64; 3]) -> [f32; 3] {
        let (dx, dy, dz) = (
            p[0] - self.origin[0],
            p[1] - self.origin[1],
            p[2] - self.origin[2],
        );
        [
            (self.cos * dx + self.sin * dy) as f32,
            (-self.sin * dx + self.cos * dy) as f32,
            dz as f32,
        ]
    }

    /// A world vector in this frame.
    fn vector(&self, v: [f64; 3]) -> [f32; 3] {
        [
            (self.cos * v[0] + self.sin * v[1]) as f32,
            (-self.sin * v[0] + self.cos * v[1]) as f32,
            v[2] as f32,
        ]
    }

    /// Free distance from the origin along a flat direction to the ropes.
    fn to_ropes(&self, dir: (f64, f64)) -> f32 {
        let half = f64::from(RING_HALF);
        let mut best = f64::from(RING_BOUND);
        for (p, d) in [(self.origin[0], dir.0), (self.origin[1], dir.1)] {
            if d.abs() > 1e-9 {
                let t = (if d > 0.0 { half - p } else { -half - p }) / d;
                if t >= 0.0 {
                    best = best.min(t);
                }
            }
        }
        best as f32
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

impl Arena {
    fn point_on(&self, body: usize, local: [f64; 3]) -> [f64; 3] {
        let p = self.sim.body_pos(body);
        let o = apply(&rotation(self.sim.body_quat(body)), local);
        [p[0] + o[0], p[1] + o[1], p[2] + o[2]]
    }

    /// A fist: its glove's centre.
    fn fist(&self, seat: usize, side: usize) -> [f64; 3] {
        self.sim.body_pos(self.seats[seat].gloves[side])
    }

    fn head(&self, seat: usize) -> [f64; 3] {
        self.point_on(self.seats[seat].torso, HEAD_OFFSET)
    }

    fn foot(&self, seat: usize, side: usize) -> [f64; 3] {
        self.point_on(self.seats[seat].feet[side], FOOT_OFFSET)
    }

    fn pelvis_height(&self, seat: usize) -> f32 {
        self.sim.body_pos(self.seats[seat].pelvis)[2] as f32
    }

    fn distance(&self) -> f32 {
        let (a, b) = (
            self.sim.body_pos(self.seats[0].pelvis),
            self.sim.body_pos(self.seats[1].pelvis),
        );
        (((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()) as f32
    }

    fn time_left(&self) -> f32 {
        (MAX_TURNS.saturating_sub(self.turn)) as f32 * TURN_SECONDS
    }

    /// The count that's running, for the scoreboard: a seat's own, else the opponent's.
    fn count_shown(&self, seat: usize) -> f32 {
        if self.fighters[seat].down {
            self.fighters[seat].count_left
        } else if self.fighters[1 - seat].down {
            self.fighters[1 - seat].count_left
        } else {
            0.0
        }
    }

    fn apply_targets(&mut self) {
        let mut ctrl = Vec::with_capacity(58);
        for fighter in &self.fighters {
            ctrl.extend_from_slice(&fighter.targets);
        }
        self.sim.set_ctrl(&ctrl);
    }

    fn arena_section(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(ARENA_SECTION_BYTES);
        bytes.extend_from_slice(&self.time_left().to_le_bytes());
        let count = self
            .fighters
            .iter()
            .filter(|f| f.down)
            .map(|f| f.count_left)
            .fold(0.0_f32, f32::max);
        bytes.extend_from_slice(&count.to_le_bytes());
        for fighter in &self.fighters {
            bytes.extend_from_slice(&(fighter.points.min(u16::MAX as u32) as u16).to_le_bytes());
        }
        for fighter in &self.fighters {
            bytes.push(fighter.knockdowns.min(255) as u8);
        }
        bytes.push(
            self.fighters
                .iter()
                .enumerate()
                .map(|(i, f)| (f.down as u8) << i)
                .sum(),
        );
        bytes.push(
            self.fighters
                .iter()
                .enumerate()
                .map(|(i, f)| (f.ko as u8) << i)
                .sum(),
        );
        for fighter in &self.fighters {
            bytes.push(fighter.landed);
        }
        for fighter in &self.fighters {
            bytes.push(fighter.strength);
        }
        bytes
    }

    fn series(&self, report: &mut Report) {
        for seat in 0..2 {
            report.seat_series(
                POINTS_SERIES,
                seat as u32,
                self.fighters[seat].points as f32,
            );
            report.seat_series(FIST_SPEED, seat as u32, self.fighters[seat].fist_speed);
            report.seat_series(STAMINA, seat as u32, self.stamina(seat));
        }
        report.session_series(DISTANCE, self.distance());
    }

    /// Heat each motor by the torque it gave over one physics step:
    /// `dH/dt = ((τ / τ_rated)² − H) / τc`, with `τ` the joint's actuator
    /// torque after MuJoCo's limit clamp.
    fn heat_motors(&mut self, dt: f64) {
        let qfrc = self.sim.qfrc_actuator();
        let k = dt / f64::from(HEAT_TIME_CONSTANT);
        for (seat, ids) in self.seats.iter().enumerate() {
            let fighter = &mut self.fighters[seat];
            for motor in 0..29 {
                let rated = f64::from(PEAK_TORQUES[motor] / PEAK_OVER_RATED);
                let load = qfrc[ids.qvel[motor]] / rated;
                let load = if load.is_finite() { load * load } else { 0.0 };
                let heat = &mut fighter.heat[motor];
                *heat += k * (load - *heat);
            }
        }
    }

    /// Derate each motor's torque limit from its heat, for the next turn.
    fn derate_motors(&mut self) {
        for (seat, ids) in self.seats.iter().enumerate() {
            let heats = &self.fighters[seat].heat;
            for ((joint, peak), heat) in ids.joints.iter().zip(PEAK_TORQUES).zip(heats) {
                let limit = f64::from(peak) * f64::from(torque_fraction(*heat as f32));
                self.sim.set_joint_actfrcrange(*joint, (-limit, limit));
            }
        }
    }

    /// What the seat's hottest limb has left: 1 − its rated-torque-weighted
    /// mean heat over the cut-off, so 0.5 at the knee and 0 at the cut-off.
    fn stamina(&self, seat: usize) -> f32 {
        let heat = &self.fighters[seat].heat;
        let hottest = MOTOR_GROUPS
            .iter()
            .map(|group| {
                let (mut sum, mut weight) = (0.0, 0.0);
                for motor in group.clone() {
                    let rated = f64::from(PEAK_TORQUES[motor]);
                    sum += rated * heat[motor];
                    weight += rated;
                }
                sum / weight
            })
            .fold(0.0_f64, f64::max);
        (1.0 - (hottest / f64::from(HEAT_CUT)).clamp(0.0, 1.0)) as f32
    }

    fn make_frame(&mut self) {
        let contacts = self.contacts.take(CONTACT_THRESHOLD);
        self.last = frame(
            &self.sim,
            "boxing",
            self.turn,
            contacts,
            Vec::new(),
            self.arena_section(),
        );
    }
}

impl World for Arena {
    fn declaration() -> arena_kit::declaration::Declaration {
        declaration()
    }

    fn setup(seed: &Seed, seats: u32, report: &mut Report) -> Result<Self, String> {
        if seats != 2 {
            return Err(format!("boxing seats exactly 2 fighters, not {seats}"));
        }
        let mut sim = MjSim::load(MODEL)?;
        let start_key = sim.keyframe_id("start");
        sim.reset(start_key);
        let canvas = sim.geom_id("canvas").ok_or("no canvas geom")?;
        let seat_ids = [
            SeatIds::resolve(&sim, SEATS[0])?,
            SeatIds::resolve(&sim, SEATS[1])?,
        ];
        // The only randomness: the pair is turned about the ring's centre by
        // up to 30° either way, drawn from the seed, so no two bouts begin
        // alike and the ropes sit differently each time, while the
        // broadcast still looks across the fighters. They stay 0.6 m apart, touching
        // gloves, so a punch reaches without a step. A turn about the centre
        // keeps the start symmetric under a half turn, so each seat sees the
        // same first input.
        let mut start = seed.stream("start");
        let angle = start.range(-START_TURN, START_TURN);
        let (sin, cos) = angle.sin_cos();
        let (half_sin, half_cos) = (angle / 2.0).sin_cos();
        let mut qpos = sim.qpos().to_vec();
        for ids in &seat_ids {
            let adr = ids.qpos[0] - 7;
            let (x, y) = (qpos[adr], qpos[adr + 1]);
            qpos[adr] = cos * x - sin * y;
            qpos[adr + 1] = sin * x + cos * y;
            // The pelvis turns with it: a turn about z, then the stance's own.
            let [w, qx, qy, qz] = [qpos[adr + 3], qpos[adr + 4], qpos[adr + 5], qpos[adr + 6]];
            let (tw, tz) = (half_cos, half_sin);
            qpos[adr + 3] = tw * w - tz * qz;
            qpos[adr + 4] = tw * qx - tz * qy;
            qpos[adr + 5] = tw * qy + tz * qx;
            qpos[adr + 6] = tw * qz + tz * w;
        }
        sim.set_qpos(&qpos);
        sim.forward();
        let mut arena = Self {
            sim,
            turn: 0,
            seats: seat_ids,
            fighters: [Fighter::new(), Fighter::new()],
            canvas,
            start_key,
            contacts: ContactTracker::default(),
            ending: None,
            last: Frame::default(),
            landings: Vec::new(),
        };
        arena.apply_targets();
        arena.series(report);
        arena.make_frame();
        Ok(arena)
    }

    fn input(&self, seat: u32) -> Vec<Values> {
        let me = seat as usize;
        let them = 1 - me;
        let ids = &self.seats[me];
        let qpos = self.sim.qpos();
        let qvel = self.sim.qvel();

        let mut joints: Vec<f32> = ids.qpos.iter().map(|&adr| qpos[adr] as f32).collect();
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
        let mut velocity = velocity.to_vec();
        clamp(&mut velocity, -VELOCITY_BOUND, VELOCITY_BOUND);
        imu.extend(velocity);
        clamp(&mut imu[..3], -1.0, 1.0);

        let mut body: Vec<f32> = Vec::with_capacity(12);
        for side in 0..2 {
            body.extend(heading.point(self.fist(me, side)));
        }
        for side in 0..2 {
            body.extend(heading.point(self.foot(me, side)));
        }
        clamp(&mut body, -BODY_BOUND, BODY_BOUND);

        let mut opponent: Vec<f32> = Vec::with_capacity(23);
        opponent.extend(heading.point(self.sim.body_pos(self.seats[them].pelvis)));
        opponent.extend(heading.point(self.head(them)));
        for side in 0..2 {
            opponent.extend(heading.point(self.fist(them, side)));
        }
        for side in 0..2 {
            opponent.extend(heading.point(self.foot(them, side)));
        }
        opponent.extend(heading.vector(self.sim.body_linvel(self.seats[them].pelvis)));
        let theirs = Heading::of(&self.sim, self.seats[them].pelvis);
        // The angle from my heading to theirs: sin and cos of (their yaw - my yaw).
        let sin = (theirs.sin * heading.cos - theirs.cos * heading.sin) as f32;
        let cos = (theirs.cos * heading.cos + theirs.sin * heading.sin) as f32;
        opponent.extend([sin, cos]);
        clamp(&mut opponent, -OPPONENT_BOUND, OPPONENT_BOUND);

        let mut ring = vec![
            heading.to_ropes((heading.cos, heading.sin)),
            heading.to_ropes((-heading.cos, -heading.sin)),
            heading.to_ropes((-heading.sin, heading.cos)),
            heading.to_ropes((heading.sin, -heading.cos)),
        ];
        clamp(&mut ring, 0.0, RING_BOUND);

        let (mine, yours) = (&self.fighters[me], &self.fighters[them]);
        let bout = vec![
            self.time_left(),
            (mine.points as f32).min(POINTS_BOUND),
            (yours.points as f32).min(POINTS_BOUND),
            mine.knockdowns.min(KNOCKDOWNS_TO_LOSE) as f32,
            yours.knockdowns.min(KNOCKDOWNS_TO_LOSE) as f32,
            mine.down as u8 as f32,
            yours.down as u8 as f32,
            self.count_shown(me).clamp(0.0, COUNT_SECONDS),
        ];

        let mut motors: Vec<f32> = mine.heat.iter().map(|&h| h as f32).collect();
        clamp(&mut motors, 0.0, HEAT_CUT);

        vec![
            Values::F32(joints),
            Values::F32(velocities),
            Values::F32(imu),
            Values::F32(body),
            Values::F32(opponent),
            Values::F32(ring),
            Values::F32(bout),
            Values::F32(motors),
        ]
    }

    fn step(&mut self, outputs: &[Option<Vec<Values>>], report: &mut Report) {
        self.turn += 1;
        for (seat, output) in outputs.iter().enumerate().take(2) {
            let values = output
                .as_ref()
                .and_then(|values| values.first())
                .and_then(Values::as_f32)
                .filter(|values| values.len() == 29);
            if let Some(values) = values {
                for (target, value) in self.fighters[seat].targets.iter_mut().zip(values) {
                    *target = f64::from(*value);
                }
            }
            self.fighters[seat].landed = 0;
            self.fighters[seat].strength = 0;
        }
        self.apply_targets();

        // Physics, noting what touched what.
        let mut touched_canvas = [false; 2];
        let mut hit: [[(f32, u8); 2]; 2] = [[(0.0, 0); 2]; 2];
        let mut fist_touching = [[false; 2]; 2];
        let dt = self.sim.timestep();
        for _ in 0..SUBSTEPS {
            self.sim.step();
            self.heat_motors(dt);
            self.contacts.observe(&self.sim);
            for contact in self.sim.contacts() {
                let geoms = [contact.geom1, contact.geom2];
                for seat in 0..2 {
                    let ids = &self.seats[seat];
                    // Anything but a foot on the canvas.
                    for (index, geom) in geoms.iter().enumerate() {
                        let other = geoms[1 - index];
                        if *geom == self.canvas
                            && ids.bodies.contains(&self.sim.geom_body(other))
                            && !ids.foot_geoms.contains(&other)
                        {
                            touched_canvas[seat] = true;
                        }
                    }
                    // A fist on the opponent's head or body.
                    let target = &self.seats[1 - seat];
                    for side in 0..2 {
                        let fist = ids.glove_geoms[side];
                        let Some(index) = geoms.iter().position(|g| *g == fist) else {
                            continue;
                        };
                        let other = geoms[1 - index];
                        let kind = if other == target.head_geom {
                            2
                        } else if other == target.torso_geom {
                            1
                        } else {
                            0
                        };
                        if kind > 0 {
                            fist_touching[seat][side] = true;
                            let force = contact.normal_force as f32;
                            if force > hit[seat][side].0 {
                                hit[seat][side] = (force, kind);
                            }
                        }
                    }
                }
            }
        }
        if self.sim.diverged() {
            // A rule of the game, never a trap: both fighters go back to their corners.
            self.sim.reset(self.start_key);
            self.apply_targets();
            self.sim.forward();
            // What touched before the reset was the blow-up's, not a punch.
            touched_canvas = [false; 2];
            hit = [[(0.0, 0); 2]; 2];
            fist_touching = [[false; 2]; 2];
            for fighter in &mut self.fighters {
                fighter.touch_turns = 0;
                fighter.open = [None; 2];
            }
        }

        // Knockdowns, counts and knockouts.
        let mut shaping = [0.0_f32; 2];
        for seat in 0..2 {
            let height = self.pelvis_height(seat);
            let low = height < DOWN_HEIGHT;
            let standing = !touched_canvas[seat] && height >= UP_HEIGHT;
            let ending = self.ending;
            let fighter = &mut self.fighters[seat];
            if touched_canvas[seat] || low {
                fighter.touch_turns += 1;
                fighter.up_turns = 0;
            } else {
                fighter.touch_turns = 0;
            }
            if fighter.down {
                fighter.count_left = (fighter.count_left - TURN_SECONDS).max(0.0);
                if standing {
                    fighter.up_turns += 1;
                }
                if fighter.up_turns >= UP_TURNS {
                    fighter.down = false;
                    fighter.up_turns = 0;
                    fighter.count_left = 0.0;
                    report.event(UP, Some(seat as u32));
                } else if fighter.count_left <= 0.0 && !fighter.ko {
                    fighter.ko = true;
                    report.event(KNOCKOUT, Some(seat as u32));
                }
            } else if fighter.touch_turns >= DOWN_TURNS && ending.is_none() {
                fighter.down = true;
                fighter.count_left = COUNT_SECONDS;
                fighter.knockdowns += 1;
                fighter.up_turns = 0;
                report.event(KNOCKDOWN, Some(seat as u32));
                shaping[seat] -= 5.0;
                shaping[1 - seat] += 5.0;
                if fighter.knockdowns >= KNOCKDOWNS_TO_LOSE {
                    fighter.ko = true;
                    report.event(KNOCKOUT, Some(seat as u32));
                }
            }
        }

        // Punches: only while both are up and the bout is on. The bell
        // (this turn, if it's the last) judges any punch still open.
        let both_up = !self.fighters[0].down && !self.fighters[1].down && self.ending.is_none();
        let last_turn = self.turn >= MAX_TURNS;
        for seat in 0..2 {
            let mut speed: f32 = 0.0;
            for side in 0..2 {
                let v = self.sim.body_linvel(self.seats[seat].gloves[side]);
                speed = speed.max(((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()) as f32);
            }
            let fighter = &mut self.fighters[seat];
            fighter.fist_speed = speed.min(VELOCITY_BOUND);
            // The frame shows this turn's best-scoring landing: what it hit
            // and how hard, from the same punch.
            let mut best = 0;
            for side in 0..2 {
                fighter.fist_cooldown[side] = fighter.fist_cooldown[side].saturating_sub(1);
                let (force, kind) = hit[seat][side];
                let fresh = !fighter.fist_touching[side];
                fighter.fist_touching[side] = fist_touching[seat][side];
                // A fresh touch opens a punch; the cooldown starts with it,
                // so a fist judges one punch at a time.
                if both_up && fresh && kind > 0 && fighter.fist_cooldown[side] == 0 {
                    fighter.open[side] = Some((0.0, kind, 0, self.turn));
                    fighter.fist_cooldown[side] = PUNCH_COOLDOWN;
                }
                let Some((peak, struck, turns, opened)) = fighter.open[side] else {
                    continue;
                };
                // Force only counts while both are up: a glove on a fighter
                // already falling adds nothing.
                let (peak, struck) = if both_up && force > peak {
                    (force, kind)
                } else {
                    (peak, struck)
                };
                let turns = turns + 1;
                if fist_touching[seat][side] && turns < LANDING_WINDOW && both_up && !last_turn {
                    fighter.open[side] = Some((peak, struck, turns, opened));
                    continue;
                }
                // The contact is over, or the window, or a fighter went down,
                // or the bell: judge it on its peak while both were up.
                fighter.open[side] = None;
                if peak < LANDING_FORCE {
                    continue;
                }
                let strength = punch_strength(peak);
                let points = punch_points(struck, strength);
                fighter.points += points;
                fighter.punches += 1;
                if points > best {
                    best = points;
                    fighter.landed = struck;
                    fighter.strength = strength;
                }
                fighter.hardest = fighter.hardest.max(peak);
                if strength == 2 {
                    fighter.power_punches += 1;
                }
                shaping[seat] += points as f32;
                shaping[1 - seat] -= points as f32;
                self.landings.push(Landing {
                    seat: seat as u32,
                    side: side as u8,
                    opened,
                    turn: self.turn,
                    force: peak,
                    head: struck == 2,
                    points,
                });
                report.event(PUNCH, Some(seat as u32));
                if strength == 2 {
                    report.event(POWER_PUNCH, Some(seat as u32));
                }
            }
        }

        // How it ends.
        if self.ending.is_none() {
            match (self.fighters[0].ko, self.fighters[1].ko) {
                (true, true) => self.ending = Some(Ending::BothCountedOut),
                (true, false) => {
                    self.ending = Some(Ending::Knockout {
                        loser: 0,
                        third: self.fighters[0].knockdowns >= KNOCKDOWNS_TO_LOSE,
                    })
                }
                (false, true) => {
                    self.ending = Some(Ending::Knockout {
                        loser: 1,
                        third: self.fighters[1].knockdowns >= KNOCKDOWNS_TO_LOSE,
                    })
                }
                (false, false) if self.turn >= MAX_TURNS => self.ending = Some(Ending::Time),
                _ => {}
            }
        }

        let distance = self.distance();
        for (seat, reward) in shaping.iter_mut().enumerate() {
            if !self.fighters[seat].down && self.pelvis_height(seat) >= UP_HEIGHT {
                *reward += 0.02;
            }
            *reward -= 0.002 * distance;
            report.shaping(seat as u32, *reward);
        }
        // Heat persists through a reset: the motors don't cool by magic.
        self.derate_motors();
        self.series(report);
        self.make_frame();
    }

    fn frame(&self) -> Vec<u8> {
        self.last.encode()
    }

    fn finish(&self, out_of_turns: bool) -> Option<Finish> {
        let ending = match self.ending {
            Some(ending) => ending,
            None if out_of_turns => Ending::Time,
            None => return None,
        };
        let points = [self.fighters[0].points, self.fighters[1].points];
        let on_points = |prefix: &str| {
            let (hi, lo) = (points[0].max(points[1]), points[0].min(points[1]));
            if points[0] == points[1] {
                Finish::new(vec![vec![0, 1]], format!("{prefix}a draw, {hi} to {lo}"))
            } else {
                let winner = if points[0] > points[1] { 0 } else { 1 };
                Finish::new(
                    vec![vec![winner], vec![1 - winner]],
                    format!("{prefix}{hi} to {lo}"),
                )
            }
        };
        let finish = match ending {
            Ending::Knockout { loser, third } => Finish::new(
                vec![vec![1 - loser], vec![loser]],
                if third {
                    "Knockout: third knockdown"
                } else {
                    "Knockout: didn't beat the count"
                },
            ),
            Ending::BothCountedOut => on_points("Both counted out: on points, "),
            Ending::Time => {
                if points[0] == points[1] {
                    Finish::new(
                        vec![vec![0, 1]],
                        format!("A draw on points, {} to {}", points[0], points[1]),
                    )
                } else {
                    on_points("On points, ")
                }
            }
        };
        Some(
            finish
                .stat(POINTS, points.map(f64::from).to_vec())
                .stat(
                    PUNCHES,
                    self.fighters.iter().map(|f| f64::from(f.punches)).collect(),
                )
                .stat(
                    KNOCKDOWNS_SCORED,
                    [self.fighters[1].knockdowns, self.fighters[0].knockdowns]
                        .map(f64::from)
                        .to_vec(),
                )
                .stat(
                    KNOCKDOWNS_SUFFERED,
                    self.fighters
                        .iter()
                        .map(|f| f64::from(f.knockdowns))
                        .collect(),
                )
                .stat(
                    STAMINA_LEFT,
                    [0, 1].map(|seat| f64::from(self.stamina(seat))).to_vec(),
                )
                .stat(
                    POWER_PUNCHES,
                    self.fighters
                        .iter()
                        .map(|f| f64::from(f.power_punches))
                        .collect(),
                )
                .stat(
                    HARDEST_PUNCH,
                    self.fighters.iter().map(|f| f64::from(f.hardest)).collect(),
                ),
        )
    }
}

impl Arena {
    /// For tests: the seats' pelvis heights.
    pub fn pelvis_heights(&self) -> [f32; 2] {
        [self.pelvis_height(0), self.pelvis_height(1)]
    }

    /// For tests: a seat's motor heats, in motor order.
    pub fn heats(&self, seat: usize) -> [f64; 29] {
        self.fighters[seat].heat
    }

    /// Every punch that landed so far, in order.
    pub fn landings(&self) -> &[Landing] {
        &self.landings
    }

    /// For tests: a seat's stamina, as the series reports it.
    pub fn stamina_of(&self, seat: usize) -> f32 {
        self.stamina(seat)
    }

    /// For tests: each motor's current torque limit and the torque it gave,
    /// N·m, in motor order.
    pub fn torques(&self, seat: usize) -> [(f64, f64); 29] {
        let ids = &self.seats[seat];
        let qfrc = self.sim.qfrc_actuator();
        std::array::from_fn(|motor| {
            let limit = self
                .sim
                .joint_actfrcrange(ids.joints[motor])
                .map_or(f64::INFINITY, |(_, hi)| hi);
            (limit, qfrc[ids.qvel[motor]])
        })
    }

    /// For tests: set a seat's motor heats.
    pub fn set_heats(&mut self, seat: usize, heat: [f64; 29]) {
        self.fighters[seat].heat = heat;
        self.derate_motors();
    }

    /// For tests: set a seat's joint targets.
    pub fn set_targets(&mut self, seat: usize, targets: [f64; 29]) {
        self.fighters[seat].targets = targets;
    }

    /// For tests: the bodies of a seat, by id, pelvis to right glove.
    pub fn seat_bodies(&self, seat: usize) -> std::ops::Range<usize> {
        self.seats[seat].bodies.clone()
    }

    /// For tests: the simulation.
    pub fn sim(&self) -> &MjSim {
        &self.sim
    }

    /// For tests: each fighter's points, knockdowns and whether it's down.
    pub fn scoreboard(&self) -> [(u32, u32, bool); 2] {
        [0, 1].map(|seat| {
            (
                self.fighters[seat].points,
                self.fighters[seat].knockdowns,
                self.fighters[seat].down,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    //! The count and the knockout, driven by setting the count state directly:
    //! no fixed sequence of joint targets gets a fallen G1 up again, so these
    //! paths can't be reached by a test policy (see the design note).

    use super::*;

    fn arena() -> (Arena, Report) {
        let mut report = Report::new(&declaration(), 2);
        let arena = Arena::setup(&Seed::from_u64(1), 2, &mut report).unwrap();
        (arena, report)
    }

    fn step_holding(arena: &mut Arena, report: &mut Report, turns: u32) -> Vec<(u32, Option<u32>)> {
        let mut events = Vec::new();
        for _ in 0..turns {
            report.next_turn();
            arena.step(&[None, None], report);
            events.extend(report.events().iter().copied());
        }
        events
    }

    #[test]
    fn a_standing_fighter_beats_the_count() {
        let (mut arena, mut report) = arena();
        let up = declaration().event_index(UP.0).unwrap() as u32;
        arena.fighters[0].down = true;
        arena.fighters[0].count_left = COUNT_SECONDS;
        arena.fighters[0].knockdowns = 1;
        let events = step_holding(&mut arena, &mut report, UP_TURNS + 2);
        assert!(events.contains(&(up, Some(0))), "no `up` in {events:?}");
        assert!(!arena.fighters[0].down);
        assert!(arena.ending.is_none());
    }

    #[test]
    fn a_fighter_down_at_ten_is_knocked_out_on_the_count() {
        let (mut arena, mut report) = arena();
        let knockout = declaration().event_index(KNOCKOUT.0).unwrap() as u32;
        arena.fighters[1].down = true;
        // The count runs out before it has stood for UP_TURNS.
        arena.fighters[1].count_left = 0.3;
        let events = step_holding(&mut arena, &mut report, 20);
        assert!(
            events.contains(&(knockout, Some(1))),
            "no knockout in {events:?}"
        );
        let finish = arena.finish(false).unwrap();
        assert_eq!(finish.places, vec![vec![0], vec![1]]);
        assert_eq!(finish.decided_by, "Knockout: didn't beat the count");
    }

    #[test]
    fn a_third_knockdown_is_a_knockout() {
        let (mut arena, mut report) = arena();
        arena.fighters[0].knockdowns = KNOCKDOWNS_TO_LOSE - 1;
        // Fold the legs so something but a foot meets the canvas.
        let mut targets = START_TARGETS.map(f64::from);
        for (hip, knee) in [(0, 3), (6, 9)] {
            targets[hip] = -2.2;
            targets[knee] = 2.8;
        }
        arena.fighters[0].targets = targets;
        let mut ended = None;
        for turn in 0..400 {
            report.next_turn();
            arena.step(&[None, None], &mut report);
            if arena.ending.is_some() {
                ended = Some(turn);
                break;
            }
        }
        assert!(ended.is_some(), "never ended");
        let finish = arena.finish(false).unwrap();
        assert_eq!(finish.places, vec![vec![1], vec![0]]);
        assert_eq!(finish.decided_by, "Knockout: third knockdown");
        assert_eq!(arena.fighters[0].knockdowns, KNOCKDOWNS_TO_LOSE);
    }

    #[test]
    fn the_arena_section_is_the_documented_size() {
        let (arena, _) = arena();
        assert_eq!(arena.arena_section().len(), ARENA_SECTION_BYTES);
    }
}
