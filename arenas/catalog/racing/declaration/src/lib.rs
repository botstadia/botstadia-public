//! Quadcopter Racing's declaration: every input and output, the budgets, and
//! the stats, series and events the site draws, plus the course's numbers the
//! engine and the test bots share. Test bots depend on this crate, and never
//! on the engine. The design note (`../README.md`) says what each value is for.

use arena_kit::declaration::*;
use arena_kit::{EventKey, SeriesKey, StatKey};

/// The arena version: bump it, with a changelog entry, for every engine change.
pub const VERSION: &str = "0.2.0";
/// The payload schema: 2 adds gate velocities and the obstacles.
pub const SCHEMA: u32 = 2;

/// Turns a second, and the race's time limit.
pub const CONTROL_RATE: u32 = 100;
pub const RACE_SECONDS: u32 = 90;
pub const MAX_TURNS: u32 = CONTROL_RATE * RACE_SECONDS;

/// How many drones may race: the model carries the most.
pub const MIN_SEATS: u32 = 2;
pub const MAX_SEATS: u32 = 6;

/// Gates a lap, laps a race.
pub const GATES: usize = 8;
pub const LAPS: u32 = 3;
pub const GATE_PASSES: u32 = GATES as u32 * LAPS;
/// Half a gate's opening, sideways and up, in metres: the frame's inner edge.
pub const GATE_HALF: f32 = 1.0;
/// The obstacle spheres the model carries; a session uses some of them.
pub const OBSTACLES: usize = 8;

/// The course's bounds: leaving them is a crash.
pub const BOUND_X: f32 = 32.0;
pub const BOUND_Y: f32 = 20.0;
pub const BOUND_UP: f32 = 15.0;

/// A crash: a contact at least this hard, or upside down this long.
pub const CRASH_FORCE: f32 = 40.0;
pub const FLIPPED_SECONDS: f32 = 0.5;
/// After a crash, motors off for this long, then back in the air this far
/// above the last gate passed (or its own pad, before the first gate).
pub const PENALTY_SECONDS: f32 = 3.0;
pub const RESPAWN_HEIGHT: f32 = 2.0;
/// Each seat's respawn spot is offset sideways along the gate by this much
/// per seat, so two drones respawning together never overlap.
pub const RESPAWN_SPACING: f32 = 0.9;

/// The Skydio X2's mass, and each rotor's thrust range, as the model has it
/// (a test pins the range against `world.mjb`).
pub const DRONE_MASS: f32 = 1.325;
pub const THRUST_MAX: f32 = 13.0;
/// Each rotor's thrust to hold a hover: the drone's weight over four.
pub const HOVER_THRUST: f32 = DRONE_MASS * 9.81 / 4.0;
/// Where the rotors are, in the body frame (x forward, y left), in motor
/// order, and the yaw torque each makes per newton (its sign is its spin).
pub const ROTORS: [[f32; 2]; 4] = [[-0.14, -0.18], [-0.14, 0.18], [0.14, 0.18], [0.14, -0.18]];
pub const YAW_TORQUE: [f32; 4] = [-0.0201, 0.0201, -0.0201, 0.0201];

/// How many gates ahead, and how many rivals, a bot is told about.
pub const GATES_AHEAD: usize = 3;
pub const RIVALS: usize = 5;

pub const FINISH_TIME: StatKey = StatKey("finish_time");
pub const LAPS_DONE: StatKey = StatKey("laps");
pub const GATES_PASSED: StatKey = StatKey("gates");
pub const CRASHES: StatKey = StatKey("crashes");
pub const BEST_LAP: StatKey = StatKey("best_lap");
pub const AVG_SPEED: StatKey = StatKey("avg_speed");

pub const PROGRESS: SeriesKey = SeriesKey("progress");
pub const GAP: SeriesKey = SeriesKey("gap");
pub const SPEED: SeriesKey = SeriesKey("speed");

pub const GATE: EventKey = EventKey("gate");
pub const LAP: EventKey = EventKey("lap");
pub const CRASH: EventKey = EventKey("crash");
pub const RESPAWN: EventKey = EventKey("respawn");
pub const FINISH: EventKey = EventKey("finish");
pub const OVERTAKE: EventKey = EventKey("overtake");

/// Input names, in declared order.
pub const IN_IMU: &str = "imu";
pub const IN_ROTORS: &str = "rotors";
pub const IN_COURSE: &str = "course";
pub const IN_RIVALS: &str = "rivals";
pub const IN_BOUNDS: &str = "bounds";
pub const IN_RACE: &str = "race";
pub const IN_OBSTACLES: &str = "obstacles";
/// The one output.
pub const OUT_THRUST: &str = "thrust";

/// Bounds of the input values. The engine clamps what it reports into these,
/// so a trainer's normalisation can rely on them.
pub const GYRO_BOUND: f32 = 30.0;
pub const VELOCITY_BOUND: f32 = 30.0;
pub const HEIGHT_BOUND: f32 = 20.0;
pub const POSITION_BOUND: f32 = 80.0;
pub const BOUNDS_BOUND: f32 = 80.0;

pub fn declaration() -> Declaration {
    let seconds = RACE_SECONDS as f32;
    Declaration {
        arena_id: "racing".into(),
        arena_version: VERSION.into(),
        changelog: vec![
            ChangelogEntry { version: VERSION.into(), changes: "The seed lays the course out every session: a stadium or a kidney of drawn size, gates spread, raised, offset and turned, two to four of them swaying or bobbing, and three to eight obstacle spheres drifting or orbiting beside the line. The start is the same every time. Inputs gain each gate's velocity and the obstacles (payload schema 2); form is average speed.".into() },
            ChangelogEntry { version: "0.1.0".into(), changes: "First version.".into() },
        ],
        payload_schema_version: SCHEMA,
        entrants: Entrants::Range { min: MIN_SEATS, max: MAX_SEATS },
        inputs: vec![
            ValueDecl::new(IN_IMU, Dtype::F32, &[10], Bounds::PerElement(
                [(-1.0, 1.0); 3].into_iter()
                    .chain([(-GYRO_BOUND, GYRO_BOUND); 3])
                    .chain([(-VELOCITY_BOUND, VELOCITY_BOUND); 3])
                    .chain([(0.0, HEIGHT_BOUND)])
                    .collect()),
                "What the drone's IMU, its own state estimate and a downward rangefinder give.")
                .with_slices(vec![
                    slice("gravity", 3, "unit", "Which way is down, in the body frame (x forward, y left, z up). Level is (0, 0, -1)."),
                    slice("gyro", 3, "rad/s", "The body's angular velocity, in the body frame."),
                    slice("velocity", 3, "m/s", "The body's velocity in the heading frame: x along its heading, level, y left, z up."),
                    slice("height", 1, "m", "Height of the body above the ground."),
                ]),
            ValueDecl::new(IN_ROTORS, Dtype::F32, &[4], Bounds::Uniform { min: 0.0, max: THRUST_MAX },
                "The thrust each rotor is making now, in motor order: rear right, rear left, front left, front right. Zero while the motors are cut after a crash.")
                .with_slices(vec![slice("thrust", 4, "N", "Each rotor's thrust.")]),
            ValueDecl::f32_slices(IN_COURSE, "The next three gates to fly through, in order, as a tracking system around the course reports them, relative to the body in the heading frame. Some gates sway sideways or bob up and down. After the finish, the gates of a lap that isn't flown.", -POSITION_BOUND, POSITION_BOUND, vec![
                slice("next", 3, "m", "The next gate's centre."),
                slice("next_way", 2, "unit", "Sine and cosine of the angle from its heading to the way through the next gate. (0, 1) means straight ahead."),
                slice("next_velocity", 3, "m/s", "How fast the next gate is moving: zero for one that stands still."),
                slice("second", 3, "m", "The gate after that."),
                slice("second_way", 2, "unit", "The way through it."),
                slice("second_velocity", 3, "m/s", "Its velocity."),
                slice("third", 3, "m", "The third gate ahead."),
                slice("third_way", 2, "unit", "The way through it."),
                slice("third_velocity", 3, "m/s", "Its velocity."),
            ]),
            ValueDecl::f32_slices(IN_RIVALS, "The five nearest other drones in the race, nearest first, relative to the body in the heading frame. A slot with no drone is all zeros.", -POSITION_BOUND, POSITION_BOUND,
                (1..=RIVALS).flat_map(|n| [
                    slice(&format!("rival_{n}"), 3, "m", "Its position."),
                    slice(&format!("rival_{n}_velocity"), 3, "m/s", "Its velocity."),
                    slice(&format!("rival_{n}_flying"), 1, "flag", "1 while it's flying, 0 while its motors are cut after a crash or the slot is empty."),
                ]).collect()),
            ValueDecl::f32_slices(IN_BOUNDS, "Free distance to the edge of the course's airspace, along the heading frame's axes. Leaving it is a crash.", 0.0, BOUNDS_BOUND, vec![
                slice("ahead", 1, "m", "Straight ahead, level."),
                slice("behind", 1, "m", "Straight behind."),
                slice("left", 1, "m", "To the left."),
                slice("right", 1, "m", "To the right."),
                slice("up", 1, "m", "Straight up to the ceiling."),
            ]),
            ValueDecl::new(IN_RACE, Dtype::F32, &[8], Bounds::PerElement(vec![
                (0.0, seconds), (0.0, LAPS as f32), (0.0, GATES as f32), (0.0, MAX_SEATS as f32),
                (0.0, 1.0), (0.0, PENALTY_SECONDS), (0.0, 1.0), (0.0, VELOCITY_BOUND),
            ]), "The race so far.")
                .with_slices(vec![
                    slice("time_left", 1, "s", "Seconds left before the time limit."),
                    slice("laps", 1, "count", "Laps finished."),
                    slice("gates", 1, "count", "Gates passed on this lap."),
                    slice("place", 1, "place", "Its place in the race now: 1 is leading."),
                    slice("down", 1, "flag", "1 while its motors are cut after a crash."),
                    slice("penalty_left", 1, "s", "Seconds until it's put back in the air; 0 when flying."),
                    slice("finished", 1, "flag", "1 once it has finished."),
                    slice("speed", 1, "m/s", "Its speed."),
                ]),
            ValueDecl::f32_slices(IN_OBSTACLES, "The obstacles floating over the course, nearest first, as the tracking system reports them, relative to the body in the heading frame. Hitting one is a crash like any other. A session has three to eight; the rest of the slots are empty, all zeros.", -POSITION_BOUND, POSITION_BOUND,
                (1..=OBSTACLES).flat_map(|n| [
                    slice(&format!("obstacle_{n}"), 3, "m", "Its centre."),
                    slice(&format!("obstacle_{n}_velocity"), 3, "m/s", "How fast it's moving."),
                    slice(&format!("obstacle_{n}_radius"), 1, "m", "Its radius: 0.4, 0.6 or 0.8, or 0 for an empty slot."),
                ]).collect()),
        ],
        outputs: vec![ValueDecl::new(OUT_THRUST, Dtype::F32, &[4], Bounds::Uniform { min: 0.0, max: THRUST_MAX },
            "Each rotor's thrust, in motor order: rear right, rear left, front left, front right. Each rotor's spin also turns the body about its vertical axis.")
            .with_slices(vec![slice("thrust", 4, "N", "Each rotor's commanded thrust.")])],
        no_action: "the rotors hold their last accepted thrusts: off before the first, a hover just after being put back in the air".into(),
        control_rate: CONTROL_RATE,
        budgets: Budgets { turn_fuel: 1_000_000, init_fuel: 16_000_000 },
        max_turns: MAX_TURNS,
        wire: WireSizes { max_input_bytes: 1024, max_output_bytes: 128, max_frame_bytes: 4096 },
        stats: vec![
            StatDecl { key: FINISH_TIME.0.into(), label: "Finish time".into(), unit: "s".into(), kind: StatKind::Duration, form: Aggregate::Mean, better: Better::Lower,
                doc: "Seconds from the start to crossing the last gate of the last lap. A drone that didn't finish counts the full 90.".into() },
            StatDecl { key: LAPS_DONE.0.into(), label: "Laps".into(), unit: "laps".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Higher,
                doc: "Laps finished, out of three.".into() },
            StatDecl { key: GATES_PASSED.0.into(), label: "Gates".into(), unit: "gates".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Higher,
                doc: "Gates passed in order, out of 24.".into() },
            StatDecl { key: CRASHES.0.into(), label: "Crashes".into(), unit: "crashes".into(), kind: StatKind::Count, form: Aggregate::Mean, better: Better::Lower,
                doc: "Hard hits, flips and flights out of bounds before finishing.".into() },
            StatDecl { key: BEST_LAP.0.into(), label: "Best lap".into(), unit: "s".into(), kind: StatKind::Duration, form: Aggregate::Min, better: Better::Lower,
                doc: "The fastest lap. A drone that finished no lap counts the full 90.".into() },
            StatDecl { key: AVG_SPEED.0.into(), label: "Average speed".into(), unit: "m/s".into(), kind: StatKind::Measure, form: Aggregate::Mean, better: Better::Higher,
                doc: "Metres flown before finishing, over the time it took: time spent down after a crash counts, the distance it fell doesn't. Courses differ every session, so this compares drones better than a finish time.".into() },
        ],
        series: vec![
            SeriesDecl { key: PROGRESS.0.into(), label: "Gates passed".into(), unit: "gates".into(), scope: SeriesScope::PerSeat, doc: "Each drone's gates passed so far, out of 24.".into() },
            SeriesDecl { key: GAP.0.into(), label: "To the next gate".into(), unit: "m".into(), scope: SeriesScope::PerSeat, doc: "Each drone's distance to its next gate; 0 once finished.".into() },
            SeriesDecl { key: SPEED.0.into(), label: "Speed".into(), unit: "m/s".into(), scope: SeriesScope::PerSeat, doc: "Each drone's speed.".into() },
        ],
        events: vec![
            EventDecl { key: GATE.0.into(), label: "Gate".into(), doc: "A drone flew through its next gate.".into() },
            EventDecl { key: LAP.0.into(), label: "Lap".into(), doc: "A drone finished a lap.".into() },
            EventDecl { key: CRASH.0.into(), label: "Crash".into(), doc: "A drone hit something hard, flipped over or left the course, and its motors were cut.".into() },
            EventDecl { key: RESPAWN.0.into(), label: "Back in the air".into(), doc: "A crashed drone was put back above its last gate.".into() },
            EventDecl { key: FINISH.0.into(), label: "Finish".into(), doc: "A drone finished its third lap.".into() },
            EventDecl { key: OVERTAKE.0.into(), label: "Overtake".into(), doc: "A drone moved up a place.".into() },
        ],
        shaping_reward: "Each turn: +0.1 per metre closer to the next gate; +10 per gate and +50 per lap; -20 on crashing; -0.01 per radian of tilt and -0.005 per rad/s of spin; on finishing, +100 less 10 per place behind first.".into(),
        min_trainer_version: "0.1.0".into(),
    }
}
