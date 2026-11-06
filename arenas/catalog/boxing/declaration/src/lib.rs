//! Humanoid Boxing's declaration: every input and output, the budgets, and the
//! stats, series and events the site draws. Test bots depend on this crate,
//! and never on the engine. The design note (`../README.md`) says what each
//! value is for.

use arena_kit::declaration::*;
use arena_kit::{EventKey, SeriesKey, StatKey};

/// The arena version: bump it, with a changelog entry, for every engine change.
pub const VERSION: &str = "0.3.0";
/// The payload schema: 2 adds the `motors` input. A new schema is a new ladder.
pub const SCHEMA: u32 = 2;

/// Turns a second, and the bout's length.
pub const CONTROL_RATE: u32 = 50;
pub const ROUND_SECONDS: u32 = 90;
pub const MAX_TURNS: u32 = CONTROL_RATE * ROUND_SECONDS;

/// The seats' names, in seat order, and the body-name prefix each uses.
pub const SEATS: [&str; 2] = ["red", "blue"];

/// Half the ring's width between the ropes, in metres.
pub const RING_HALF: f32 = 2.3;

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

/// Each motor's published peak torque, N·m, in motor order: the joint's
/// `actuatorfrcrange` in the model (a test pins it against `world.mjb`).
pub const PEAK_TORQUES: [f32; 29] = [
    88.0, 139.0, 88.0, 139.0, 50.0, 50.0, 88.0, 139.0, 88.0, 139.0, 50.0, 50.0, 88.0, 50.0, 50.0,
    25.0, 25.0, 25.0, 25.0, 25.0, 5.0, 5.0, 25.0, 25.0, 25.0, 25.0, 25.0, 5.0, 5.0,
];

/// Stamina is motor heat ([the design note](../README.md#stamina)). A motor's
/// rated (continuous) torque is its peak over this: an estimate, since
/// Unitree publishes no continuous torque per joint, and 3 is the usual
/// peak-to-continuous ratio of frameless BLDC joint actuators.
pub const PEAK_OVER_RATED: f32 = 3.0;
/// The winding's thermal time constant, seconds: an estimate of the order
/// for actuators of this size. Heat `H` follows
/// `dH/dt = ((τ / τ_rated)² − H) / τc`, so it settles at 1 at rated torque.
pub const HEAT_TIME_CONSTANT: f32 = 40.0;
/// At this heat a motor starts to lose torque…
pub const HEAT_KNEE: f32 = 1.0;
/// …and at this heat it gives only `HEAT_FLOOR` of its peak, the least it
/// ever gives. Between the two its limit falls in a straight line.
pub const HEAT_CUT: f32 = 2.0;
pub const HEAT_FLOOR: f32 = 0.25;

/// The fraction of its peak torque a motor at heat `h` can give.
pub fn torque_fraction(h: f32) -> f32 {
    ((HEAT_CUT - h) / (HEAT_CUT - HEAT_KNEE)).clamp(HEAT_FLOOR, 1.0)
}

/// The five motor groups stamina is reported over, as ranges in motor order:
/// each leg, the waist, each arm.
pub const MOTOR_GROUPS: [std::ops::Range<usize>; 5] = [0..6, 6..12, 12..15, 15..22, 22..29];

/// A glove on the head or body at or above this normal force lands, in N.
pub const LANDING_FORCE_N: f32 = 25.0;

/// Points for a landed punch to the head and to the body.
pub const HEAD_POINTS: u32 = 2;
pub const BODY_POINTS: u32 = 1;
/// A landed punch at or above these peak forces is solid (one point more)
/// or a power punch (two more), in newtons: the median and the 90th
/// percentile of the reference drill's landings (the scripted straight on a
/// fighter that only stands, 209 landings over 20 seeds), to the newton.
pub const SOLID_PUNCH_N: f32 = 101.0;
pub const POWER_PUNCH_N: f32 = 159.0;
/// The ten count, and how many knockdowns end a bout.
pub const COUNT_SECONDS: f32 = 10.0;
pub const KNOCKDOWNS_TO_LOSE: u32 = 3;

pub const POINTS: StatKey = StatKey("points");
pub const PUNCHES: StatKey = StatKey("punches");
pub const KNOCKDOWNS_SCORED: StatKey = StatKey("knockdowns_scored");
pub const KNOCKDOWNS_SUFFERED: StatKey = StatKey("knockdowns_suffered");
pub const STAMINA_LEFT: StatKey = StatKey("stamina_left");
pub const POWER_PUNCHES: StatKey = StatKey("power_punches");
pub const HARDEST_PUNCH: StatKey = StatKey("hardest_punch");

pub const POINTS_SERIES: SeriesKey = SeriesKey("points");
pub const FIST_SPEED: SeriesKey = SeriesKey("fist_speed");
pub const DISTANCE: SeriesKey = SeriesKey("distance");
pub const STAMINA: SeriesKey = SeriesKey("stamina");

pub const PUNCH: EventKey = EventKey("punch");
pub const KNOCKDOWN: EventKey = EventKey("knockdown");
pub const UP: EventKey = EventKey("up");
pub const KNOCKOUT: EventKey = EventKey("knockout");
pub const POWER_PUNCH: EventKey = EventKey("power_punch");

/// Input names, in declared order.
pub const IN_JOINTS: &str = "joints";
pub const IN_JOINT_VELOCITIES: &str = "joint_velocities";
pub const IN_IMU: &str = "imu";
pub const IN_BODY: &str = "body";
pub const IN_OPPONENT: &str = "opponent";
pub const IN_RING: &str = "ring";
pub const IN_BOUT: &str = "bout";
pub const IN_MOTORS: &str = "motors";
/// The one output.
pub const OUT_TARGETS: &str = "targets";

/// Bounds of the input values that aren't joint ranges. The engine clamps what
/// it reports into these, so a trainer's normalisation can rely on them.
pub const JOINT_VELOCITY_BOUND: f32 = 30.0;
pub const GYRO_BOUND: f32 = 20.0;
pub const VELOCITY_BOUND: f32 = 10.0;
pub const BODY_BOUND: f32 = 1.5;
pub const OPPONENT_BOUND: f32 = 10.0;
pub const RING_BOUND: f32 = 6.0;
pub const POINTS_BOUND: f32 = 500.0;

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

pub fn declaration() -> Declaration {
    Declaration {
        arena_id: "boxing".into(),
        arena_version: VERSION.into(),
        changelog: vec![
            ChangelogEntry { version: VERSION.into(), changes: "Impact scoring: a punch is judged over its whole contact at its peak force, and harder punches score more: a solid punch (101 N) one point more, a power punch (159 N) two more. A power-punch event, power-punch and hardest-punch stats, and each landing's strength in the replay frame. Inputs unchanged.".into() },
            ChangelogEntry { version: "0.2.0".into(), changes: "Boxing gloves: a padded 0.23 kg glove on each wrist, which is what lands a punch (a lower landing force for padded contact) and what the fists' positions report. Stamina: each motor heats with the torque it gives and loses torque when hot, reported in the new `motors` input, a per-turn stamina series and a stamina-left stat. Payload schema 2.".into() },
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
            ValueDecl::f32_slices(IN_BODY, "Its own fists and feet, relative to the pelvis, in the heading frame.", -BODY_BOUND, BODY_BOUND, vec![
                slice("left_fist", 3, "m", "The left fist."),
                slice("right_fist", 3, "m", "The right fist."),
                slice("left_foot", 3, "m", "The left foot."),
                slice("right_foot", 3, "m", "The right foot."),
            ]),
            ValueDecl::f32_slices(IN_OPPONENT, "The opponent, as a tracking system around the ring reports it, relative to its own pelvis in the heading frame.", -OPPONENT_BOUND, OPPONENT_BOUND, vec![
                slice("pelvis", 3, "m", "The opponent's pelvis."),
                slice("head", 3, "m", "The opponent's head."),
                slice("left_fist", 3, "m", "The opponent's left fist."),
                slice("right_fist", 3, "m", "The opponent's right fist."),
                slice("left_foot", 3, "m", "The opponent's left foot."),
                slice("right_foot", 3, "m", "The opponent's right foot."),
                slice("velocity", 3, "m/s", "The opponent's pelvis velocity."),
                slice("facing", 2, "unit", "Sine and cosine of the angle from its own heading to the opponent's. (0, -1) means squared up, face to face."),
            ]),
            ValueDecl::f32_slices(IN_RING, "How far the ropes are, along the heading frame's axes.", 0.0, RING_BOUND, vec![
                slice("ahead", 1, "m", "Free distance straight ahead to the ropes."),
                slice("behind", 1, "m", "Straight behind."),
                slice("left", 1, "m", "To the left."),
                slice("right", 1, "m", "To the right."),
            ]),
            ValueDecl::new(IN_BOUT, Dtype::F32, &[8], Bounds::PerElement(vec![
                (0.0, ROUND_SECONDS as f32), (0.0, POINTS_BOUND), (0.0, POINTS_BOUND),
                (0.0, KNOCKDOWNS_TO_LOSE as f32), (0.0, KNOCKDOWNS_TO_LOSE as f32), (0.0, 1.0), (0.0, 1.0), (0.0, COUNT_SECONDS),
            ]), "The scoreboard.")
                .with_slices(vec![
                    slice("time_left", 1, "s", "Seconds left in the round."),
                    slice("points", 1, "points", "Its own points."),
                    slice("opponent_points", 1, "points", "The opponent's points."),
                    slice("knockdowns", 1, "count", "How many times it has been down."),
                    slice("opponent_knockdowns", 1, "count", "How many times the opponent has been down."),
                    slice("down", 1, "flag", "1 while it's down and being counted."),
                    slice("opponent_down", 1, "flag", "1 while the opponent is down and being counted."),
                    slice("count_left", 1, "s", "Seconds left on the count, for whoever is down; 0 otherwise."),
                ]),
            ValueDecl::f32_slices(IN_MOTORS, "Each motor's heat, from its temperature telemetry, in motor order. 0 is cool; heat settles at 1 under the motor's rated torque held for good. Above 1 the motor starts to lose torque, giving a quarter of its peak at 2, the most reported. It heats with the square of the torque it gives and cools over about 40 s.", 0.0, HEAT_CUT,
                joint_slices("heat", "the motor's heat")),
        ],
        outputs: vec![ValueDecl::new(OUT_TARGETS, Dtype::F32, &[29], per_joint(&JOINT_RANGES),
            "Each joint's target angle, in motor order. The motors track it with the G1's published gains and torque limits.")
            .with_slices(joint_slices("rad", "the target angle"))],
        no_action: "the fighter holds its last accepted joint targets, the start stance at first".into(),
        control_rate: CONTROL_RATE,
        budgets: Budgets { turn_fuel: 2_000_000, init_fuel: 32_000_000 },
        max_turns: MAX_TURNS,
        wire: WireSizes { max_input_bytes: 1024, max_output_bytes: 256, max_frame_bytes: 8192 },
        stats: vec![
            StatDecl { key: POINTS.0.into(), label: "Points".into(), unit: "points".into(), kind: StatKind::Score, form: Aggregate::Mean, better: Better::Higher,
                doc: "Points scored: 2 for a punch to the head and 1 to the body, one more for a solid punch and two more for a power punch.".into() },
            StatDecl { key: PUNCHES.0.into(), label: "Punches landed".into(), unit: "punches".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Higher,
                doc: "Punches that landed hard enough to score.".into() },
            StatDecl { key: KNOCKDOWNS_SCORED.0.into(), label: "Knockdowns".into(), unit: "knockdowns".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Higher,
                doc: "Times the opponent went down.".into() },
            StatDecl { key: KNOCKDOWNS_SUFFERED.0.into(), label: "Times down".into(), unit: "knockdowns".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Lower,
                doc: "Times this fighter went down.".into() },
            StatDecl { key: STAMINA_LEFT.0.into(), label: "Stamina left".into(), unit: "share".into(), kind: StatKind::Ratio, form: Aggregate::Mean, better: Better::Neither,
                doc: "The fighter's stamina at the final bell: 1 fresh, 0 with its hottest limb's motors at the point where they give a quarter of their torque.".into() },
            StatDecl { key: POWER_PUNCHES.0.into(), label: "Power punches".into(), unit: "punches".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Higher,
                doc: "Punches that landed at a power punch's force or more.".into() },
            StatDecl { key: HARDEST_PUNCH.0.into(), label: "Hardest punch".into(), unit: "N".into(), kind: StatKind::Measure, form: Aggregate::Max, better: Better::Higher,
                doc: "The peak force of the fighter's hardest landed punch, 0 if none landed.".into() },
        ],
        series: vec![
            SeriesDecl { key: POINTS_SERIES.0.into(), label: "Points".into(), unit: "points".into(), scope: SeriesScope::PerSeat, doc: "Each fighter's points so far.".into() },
            SeriesDecl { key: FIST_SPEED.0.into(), label: "Fist speed".into(), unit: "m/s".into(), scope: SeriesScope::PerSeat, doc: "Each fighter's faster fist, this turn.".into() },
            SeriesDecl { key: DISTANCE.0.into(), label: "Distance".into(), unit: "m".into(), scope: SeriesScope::Session, doc: "Between the fighters' pelvises.".into() },
            SeriesDecl { key: STAMINA.0.into(), label: "Stamina".into(), unit: "share".into(), scope: SeriesScope::PerSeat, doc: "How much the hottest limb (a leg, an arm or the waist) has left: 1 fresh, 0.5 where its motors start to lose torque, 0 where they give a quarter.".into() },
        ],
        events: vec![
            EventDecl { key: PUNCH.0.into(), label: "Punch landed".into(), doc: "A punch landed and scored.".into() },
            EventDecl { key: KNOCKDOWN.0.into(), label: "Knockdown".into(), doc: "A fighter went down and the count began.".into() },
            EventDecl { key: UP.0.into(), label: "Beat the count".into(), doc: "A fighter got up before ten.".into() },
            EventDecl { key: KNOCKOUT.0.into(), label: "Knockout".into(), doc: "A fighter failed the count or went down a third time.".into() },
            EventDecl { key: POWER_PUNCH.0.into(), label: "Power punch".into(), doc: "A punch landed at a power punch's force: the hardest tenth of a committed straight.".into() },
        ],
        shaping_reward: "Each turn: +1 per point scored and -1 per point conceded; -5 on going down and +5 on putting the opponent down; +0.02 while standing; -0.002 per metre to the opponent.".into(),
        min_trainer_version: "0.1.0".into(),
    }
}
