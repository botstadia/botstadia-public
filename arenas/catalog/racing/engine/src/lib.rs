//! Quadcopter Racing's world: two to six Skydio X2s race three laps of an
//! eight-gate course. The rules are in the design note (`../README.md`): gates
//! in order, a crash cuts the motors for three seconds and puts the drone
//! back above its last gate, and the finish order decides, or at the time
//! limit gates passed and then distance to the next gate.

pub mod course;

use arena_kit::mujoco::MjSim;
use arena_kit::replay_wire::Frame;
use arena_kit::sim::{ContactTracker, frame};
use arena_kit::{Finish, Report, Seed, Values, World};
use course::{Course, Gate, OBSTACLES};
use racing_declaration::*;

/// The baked model, embedded: a new `.mjb` is a new arena version.
pub const MODEL: &[u8] = include_bytes!("../../model/world.mjb");
/// Physics steps per turn: 100 turns a second at a 2 ms timestep.
pub const SUBSTEPS: u32 = 5;
pub const TURN_SECONDS: f32 = 1.0 / CONTROL_RATE as f32;
/// Contacts weaker than this stay out of the replay frame.
pub const CONTACT_THRESHOLD: f32 = 10.0;
/// Upside down for this many turns is a crash.
pub const FLIPPED_TURNS: u32 = (FLIPPED_SECONDS * CONTROL_RATE as f32) as u32;
/// A crash's penalty, in turns.
pub const PENALTY_TURNS: u32 = (PENALTY_SECONDS * CONTROL_RATE as f32) as u32;
/// Where the drones of seats beyond the field wait: on the ground past the
/// north edge of the airspace, `PARK_SPACING` apart.
pub const PARK_Y: f64 = 26.0;
pub const PARK_SPACING: f64 = 2.5;
/// How many drones the model carries, and its qpos and qvel per drone.
const DRONES: usize = MAX_SEATS as usize;
const QVEL_PER_DRONE: usize = 6;

/// The arena section of each replay frame, little-endian, which the player
/// decodes: time left f32, the seat count u8, then per seat 4 bytes: gates
/// passed u8, place u8 (1 is leading), mean rotor thrust u8 (0 to 255 over
/// 0 to 13 N), and flags u8: bit 0 down, 1 finished, 2 gate this turn, 3 lap
/// this turn, 4 crash this turn, 5 back in the air this turn, 6 finished
/// this turn, 7 out of the race for good (crashed after finishing). Then
/// (0.2.0 on) one byte: bit n set when `obstacle_{n+1}` is on the course,
/// so the player hides the parked ones. A 0.1.0 frame ends before it.
pub const ARENA_SECTION_HEADER: usize = 5;
pub const ARENA_SECTION_PER_SEAT: usize = 4;
pub const ARENA_SECTION_TRAILER: usize = 1;

pub mod flags {
    pub const DOWN: u8 = 1;
    pub const FINISHED: u8 = 1 << 1;
    pub const GATE: u8 = 1 << 2;
    pub const LAP: u8 = 1 << 3;
    pub const CRASH: u8 = 1 << 4;
    pub const RESPAWN: u8 = 1 << 5;
    pub const FINISH: u8 = 1 << 6;
    pub const RETIRED: u8 = 1 << 7;
}

/// One drone in the race.
#[derive(Debug, Clone)]
struct Drone {
    body: usize,
    qvel: usize,
    /// Metres flown while racing (flying, before finishing), and turns
    /// raced (before finishing), for the average speed.
    flown: f64,
    raced: u32,
    ctrl: usize,
    pad: [f64; 3],
    /// The thrusts the rotors hold.
    thrust: [f64; 4],
    passes: u32,
    /// The turn of the latest gate, for the running order.
    last_pass_turn: u32,
    lap_start: u32,
    best_lap: Option<u32>,
    crashes: u32,
    /// Turns of penalty left while down.
    penalty: u32,
    down: bool,
    /// Crashed after finishing: out of the race for good, motors off.
    retired: bool,
    flipped_turns: u32,
    finished: Option<u32>,
    previous: [f64; 3],
    gap: f32,
    place: u32,
    flags: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    AllFinished,
    Time,
}

pub struct Arena {
    sim: MjSim,
    turn: u32,
    seats: u32,
    course: Course,
    /// Each gate's slide joints, sideways and up: (qpos, qvel) addresses.
    /// Gate 1 has none.
    gate_joints: [Option<[(usize, usize); 2]>; GATES],
    /// Each obstacle's x, y and z slides.
    obstacle_joints: [[(usize, usize); 3]; OBSTACLES],
    drones: Vec<Drone>,
    /// Bodies of every drone in the race, for contacts.
    drone_bodies: Vec<usize>,
    /// Bodies of every drone the model carries, raced or parked.
    drone_bodies_all: Vec<usize>,
    start_key: Option<usize>,
    contacts: ContactTracker,
    ending: Option<Ending>,
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

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// A level quaternion facing `yaw` radians anticlockwise from +x.
fn yaw_quat(yaw: f64) -> [f64; 4] {
    [(yaw / 2.0).cos(), 0.0, 0.0, (yaw / 2.0).sin()]
}

/// The body-centred, level frame along a drone's heading.
struct Heading {
    origin: [f64; 3],
    cos: f64,
    sin: f64,
}

impl Heading {
    fn of(sim: &MjSim, body: usize) -> Self {
        let r = rotation(sim.body_quat(body));
        let (fx, fy) = (r[0][0], r[1][0]);
        let norm = (fx * fx + fy * fy).sqrt();
        let (cos, sin) = if norm > 1e-9 {
            (fx / norm, fy / norm)
        } else {
            (1.0, 0.0)
        };
        Self {
            origin: sim.body_pos(body),
            cos,
            sin,
        }
    }

    fn point(&self, p: [f64; 3]) -> [f32; 3] {
        self.vector(sub(p, self.origin))
    }

    fn vector(&self, v: [f64; 3]) -> [f32; 3] {
        [
            (self.cos * v[0] + self.sin * v[1]) as f32,
            (-self.sin * v[0] + self.cos * v[1]) as f32,
            v[2] as f32,
        ]
    }

    /// Sine and cosine of the angle from the heading to a level direction.
    fn way(&self, d: [f64; 3]) -> [f32; 2] {
        let [x, y, _] = self.vector(d);
        [y, x]
    }

    /// Free level distance to the airspace's edge along a direction.
    fn to_bounds(&self, dir: (f64, f64)) -> f32 {
        let mut best = f64::from(BOUNDS_BOUND);
        for (p, d, half) in [
            (self.origin[0], dir.0, f64::from(BOUND_X)),
            (self.origin[1], dir.1, f64::from(BOUND_Y)),
        ] {
            if d.abs() > 1e-9 {
                let t = (if d > 0.0 { half - p } else { -half - p }) / d;
                best = best.min(t.max(0.0));
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

/// Seconds as the site reads a race time: 1:02.4.
pub fn race_time(turns: u32) -> String {
    let tenths = (turns as f32 * TURN_SECONDS * 10.0).round() as u32;
    format!("{}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

impl Arena {
    fn time_left(&self) -> f32 {
        (MAX_TURNS.saturating_sub(self.turn)) as f32 * TURN_SECONDS
    }

    fn next_gate(&self, drone: &Drone) -> &Gate {
        &self.course.gates[drone.passes as usize % GATES]
    }

    /// Seconds into the race at the end of `turn`'s physics, or at `step`
    /// sub-steps into it.
    fn time(&self, turn: u32, step: u32) -> f64 {
        (f64::from(turn) * f64::from(SUBSTEPS) + f64::from(step)) * self.sim.timestep()
    }

    /// Drive every gate and obstacle to where it is at `t`: position and
    /// velocity, so contacts see how fast it moves. Gates that don't move
    /// and parked obstacles are held still, since a hit would push them.
    fn drive(&mut self, t: f64) {
        let mut qpos = self.sim.qpos().to_vec();
        let mut qvel = self.sim.qvel().to_vec();
        for (gate, joints) in self.course.gates.iter().zip(&self.gate_joints) {
            if let Some([(sq, sv), (uq, uv)]) = joints {
                qpos[*sq] = gate.sway.at(t);
                qvel[*sv] = gate.sway.rate(t);
                qpos[*uq] = gate.bob.at(t);
                qvel[*uv] = gate.bob.rate(t);
            }
        }
        for (index, joints) in self.obstacle_joints.iter().enumerate() {
            let (p, v) = match &self.course.obstacles[index] {
                Some(obstacle) => (obstacle.position(t), obstacle.velocity(t)),
                None => (course::parked(index), [0.0; 3]),
            };
            for (axis, (q, d)) in joints.iter().enumerate() {
                qpos[*q] = p[axis];
                qvel[*d] = v[axis];
            }
        }
        self.sim.set_qpos(&qpos);
        self.sim.set_qvel(&qvel);
    }

    fn gap(&self, seat: usize) -> f32 {
        let drone = &self.drones[seat];
        if drone.finished.is_some() {
            return 0.0;
        }
        let now = self.time(self.turn, 0);
        length(sub(
            self.next_gate(drone).centre(now),
            self.sim.body_pos(drone.body),
        )) as f32
    }

    fn speed(&self, seat: usize) -> f32 {
        let speed = length(self.sim.body_linvel(self.drones[seat].body)) as f32;
        if speed.is_finite() {
            speed.min(VELOCITY_BOUND)
        } else {
            0.0
        }
    }

    fn flying(&self, seat: usize) -> bool {
        let drone = &self.drones[seat];
        !drone.down && !drone.retired
    }

    /// Where a seat goes back into the air: above its last gate's resting
    /// place and above the highest it bobs, offset sideways by seat so that
    /// drones respawning together never overlap, or above its own pad
    /// before the first gate. Facing the next gate's resting place.
    fn respawn_pose(&self, seat: usize) -> ([f64; 3], [f64; 4]) {
        let drone = &self.drones[seat];
        let height = f64::from(RESPAWN_HEIGHT);
        let pos = if drone.passes == 0 {
            [drone.pad[0], drone.pad[1], drone.pad[2] + height]
        } else {
            let gate = &self.course.gates[(drone.passes as usize - 1) % GATES];
            let offset =
                (seat as f64 - (self.seats as f64 - 1.0) / 2.0) * f64::from(RESPAWN_SPACING);
            [
                gate.base[0] + gate.side[0] * offset,
                gate.base[1] + gate.side[1] * offset,
                gate.base[2] + gate.bob.amplitude + height,
            ]
        };
        let to = sub(self.next_gate(drone).base, pos);
        (pos, yaw_quat(to[1].atan2(to[0])))
    }

    fn place_drone(&mut self, seat: usize, pos: [f64; 3], quat: [f64; 4]) {
        let (body, qvel) = (self.drones[seat].body, self.drones[seat].qvel);
        self.sim.set_free_body_pose(body, pos, quat);
        let mut velocities = self.sim.qvel().to_vec();
        velocities[qvel..qvel + QVEL_PER_DRONE].fill(0.0);
        self.sim.set_qvel(&velocities);
        self.sim.forward();
        self.drones[seat].previous = pos;
        self.drones[seat].flipped_turns = 0;
    }

    fn respawn(&mut self, seat: usize) {
        let (pos, quat) = self.respawn_pose(seat);
        self.place_drone(seat, pos, quat);
        let drone = &mut self.drones[seat];
        drone.down = false;
        drone.penalty = 0;
        drone.thrust = [f64::from(HOVER_THRUST); 4];
    }

    fn apply_thrust(&mut self) {
        let mut ctrl = self.sim.ctrl().to_vec();
        for drone in &self.drones {
            let thrust = if drone.down || drone.retired {
                [0.0; 4]
            } else {
                drone.thrust
            };
            ctrl[drone.ctrl..drone.ctrl + 4].copy_from_slice(&thrust);
        }
        self.sim.set_ctrl(&ctrl);
    }

    /// The running order: finished drones by finish turn, then the rest by
    /// gates passed, earlier to the latest gate first. Seats compare equal
    /// only when they did the same thing on the same turn.
    fn running_key(&self, seat: usize) -> (u32, u32, u32) {
        let drone = &self.drones[seat];
        match drone.finished {
            Some(turn) => (0, turn, 0),
            None => (1, u32::MAX - drone.passes, drone.last_pass_turn),
        }
    }

    fn places(&self) -> Vec<u32> {
        let keys: Vec<_> = (0..self.seats as usize)
            .map(|seat| self.running_key(seat))
            .collect();
        keys.iter()
            .map(|key| 1 + keys.iter().filter(|other| *other < key).count() as u32)
            .collect()
    }

    fn arena_section(&self) -> Vec<u8> {
        let mut bytes =
            Vec::with_capacity(ARENA_SECTION_HEADER + ARENA_SECTION_PER_SEAT * self.seats as usize);
        bytes.extend_from_slice(&self.time_left().to_le_bytes());
        bytes.push(self.seats as u8);
        for drone in &self.drones {
            bytes.push(drone.passes.min(255) as u8);
            bytes.push(drone.place.min(255) as u8);
            let thrust = if drone.down || drone.retired {
                0.0
            } else {
                drone.thrust.iter().sum::<f64>() / 4.0
            };
            bytes.push(
                (thrust / f64::from(THRUST_MAX) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8,
            );
            let mut flags = drone.flags;
            if drone.down {
                flags |= flags::DOWN;
            }
            if drone.finished.is_some() {
                flags |= flags::FINISHED;
            }
            if drone.retired {
                flags |= flags::RETIRED;
            }
            bytes.push(flags);
        }
        bytes.push(
            self.course
                .obstacles
                .iter()
                .enumerate()
                .map(|(i, o)| u8::from(o.is_some()) << i)
                .sum(),
        );
        bytes
    }

    fn series(&self, report: &mut Report) {
        for seat in 0..self.seats as usize {
            report.seat_series(PROGRESS, seat as u32, self.drones[seat].passes as f32);
            report.seat_series(GAP, seat as u32, self.drones[seat].gap);
            report.seat_series(SPEED, seat as u32, self.speed(seat));
        }
    }

    fn make_frame(&mut self) {
        let contacts = self.contacts.take(CONTACT_THRESHOLD);
        self.last = frame(
            &self.sim,
            "racing",
            self.turn,
            contacts,
            Vec::new(),
            self.arena_section(),
        );
    }

    /// A numerical blow-up: back to the start keyframe, then every drone
    /// where it belongs, nothing counted. A flying drone is put back in the
    /// air where it would respawn; one serving a penalty keeps serving it.
    fn recover(&mut self) {
        self.sim.reset(self.start_key);
        self.drive(self.time(self.turn, 0));
        self.park();
        for seat in 0..self.seats as usize {
            let (pos, quat) = self.respawn_pose(seat);
            self.place_drone(seat, pos, quat);
            if !self.drones[seat].down {
                self.drones[seat].thrust = [f64::from(HOVER_THRUST); 4];
            }
        }
        self.apply_thrust();
        self.sim.forward();
    }

    /// Seats beyond the field: on the ground past the airspace, motors off.
    fn park(&mut self) {
        for index in self.seats as usize..DRONES {
            let body = self.drone_bodies_all[index];
            let pos = [-12.5 + PARK_SPACING * index as f64, PARK_Y, 0.02];
            self.sim.set_free_body_pose(body, pos, [1.0, 0.0, 0.0, 0.0]);
        }
    }
}

impl World for Arena {
    fn declaration() -> arena_kit::declaration::Declaration {
        declaration()
    }

    fn setup(seed: &Seed, seats: u32, report: &mut Report) -> Result<Self, String> {
        if !(MIN_SEATS..=MAX_SEATS).contains(&seats) {
            return Err(format!(
                "racing seats {MIN_SEATS} to {MAX_SEATS} drones, not {seats}"
            ));
        }
        let mut sim = MjSim::load(MODEL)?;
        if sim.nu() != DRONES * 4 {
            return Err("the model isn't six drones of four rotors".into());
        }
        let start_key = sim.keyframe_id("start");
        sim.reset(start_key);

        // The course, laid out from the seed: gates 2 to 8 placed and
        // turned, and the joints that move them and the obstacles.
        let course = course::draw(seed);
        let joint = |sim: &MjSim, name: &str| {
            sim.joint_id(name)
                .map(|id| sim.joint_addr(id))
                .ok_or_else(|| format!("no joint {name}"))
        };
        let mut gate_joints = [None; GATES];
        for (index, gate) in course.gates.iter().enumerate() {
            let name = format!("gate_{}", index + 1);
            let body = sim
                .body_id(&name)
                .ok_or_else(|| format!("no body {name}"))?;
            if index == 0 {
                continue;
            }
            sim.set_body_pos(body, gate.base);
            let half = gate.yaw() / 2.0;
            sim.set_body_quat(body, [half.cos(), 0.0, 0.0, half.sin()]);
            gate_joints[index] = Some([
                joint(&sim, &format!("{name}_side"))?,
                joint(&sim, &format!("{name}_up"))?,
            ]);
        }
        let mut obstacle_joints = [[(0, 0); 3]; OBSTACLES];
        for (index, joints) in obstacle_joints.iter_mut().enumerate() {
            for (axis, name) in ["x", "y", "z"].iter().enumerate() {
                joints[axis] = joint(&sim, &format!("obstacle_{}_{name}", index + 1))?;
            }
        }

        // The grid: which drone starts on which pad, shuffled by the seed.
        let mut pads = Vec::with_capacity(DRONES);
        for index in 1..=DRONES {
            let geom = sim
                .geom_id(&format!("pad_{index}"))
                .ok_or_else(|| format!("no geom pad_{index}"))?;
            let p = sim.geom_pos(geom);
            pads.push([p[0], p[1], 0.02]);
        }
        let mut grid = seed.stream("grid");
        for index in (1..pads.len()).rev() {
            let other = grid.below(index as u32 + 1) as usize;
            pads.swap(index, other);
        }

        let mut drone_bodies_all = Vec::with_capacity(DRONES);
        let mut drones = Vec::with_capacity(seats as usize);
        for (index, pad) in pads.iter().copied().enumerate() {
            let prefix = format!("drone_{}_", index + 1);
            let body = sim
                .body_id(&format!("{prefix}x2"))
                .ok_or_else(|| format!("no body {prefix}x2"))?;
            let ctrl = sim
                .actuator_id(&format!("{prefix}thrust1"))
                .ok_or_else(|| format!("no actuator {prefix}thrust1"))?;
            let (_, qvel) = sim
                .body_joint(body)
                .map(|joint| sim.joint_addr(joint))
                .ok_or_else(|| format!("{prefix}x2 has no free joint"))?;
            drone_bodies_all.push(body);
            if index < seats as usize {
                drones.push(Drone {
                    body,
                    qvel,
                    flown: 0.0,
                    raced: 0,
                    ctrl,
                    pad,
                    thrust: [0.0; 4],
                    passes: 0,
                    last_pass_turn: 0,
                    lap_start: 0,
                    best_lap: None,
                    crashes: 0,
                    penalty: 0,
                    down: false,
                    retired: false,
                    flipped_turns: 0,
                    finished: None,
                    previous: pad,
                    gap: 0.0,
                    place: 1,
                    flags: 0,
                });
            }
        }

        let mut arena = Self {
            sim,
            turn: 0,
            seats,
            course,
            gate_joints,
            obstacle_joints,
            drones,
            drone_bodies: drone_bodies_all[..seats as usize].to_vec(),
            drone_bodies_all,
            start_key,
            contacts: ContactTracker::default(),
            ending: None,
            last: Frame::default(),
        };
        arena.drive(0.0);
        arena.park();
        for seat in 0..seats as usize {
            let pad = arena.drones[seat].pad;
            let to = sub(arena.course.gates[0].base, pad);
            arena.place_drone(seat, pad, yaw_quat(to[1].atan2(to[0])));
        }
        arena.apply_thrust();
        arena.sim.forward();
        for seat in 0..seats as usize {
            arena.drones[seat].gap = arena.gap(seat);
        }
        arena.series(report);
        arena.make_frame();
        Ok(arena)
    }

    fn input(&self, seat: u32) -> Vec<Values> {
        let me = seat as usize;
        let drone = &self.drones[me];
        let heading = Heading::of(&self.sim, drone.body);
        let r = rotation(self.sim.body_quat(drone.body));

        let mut imu: Vec<f32> = Vec::with_capacity(10);
        imu.extend(apply_transposed(&r, [0.0, 0.0, -1.0]).map(|v| v as f32));
        clamp(&mut imu[..3], -1.0, 1.0);
        let mut gyro = apply_transposed(&r, self.sim.body_angvel(drone.body))
            .map(|v| v as f32)
            .to_vec();
        clamp(&mut gyro, -GYRO_BOUND, GYRO_BOUND);
        imu.extend(gyro);
        let mut velocity = heading.vector(self.sim.body_linvel(drone.body)).to_vec();
        clamp(&mut velocity, -VELOCITY_BOUND, VELOCITY_BOUND);
        imu.extend(velocity);
        let mut height = vec![heading.origin[2] as f32];
        clamp(&mut height, 0.0, HEIGHT_BOUND);
        imu.extend(height);

        let mut rotors: Vec<f32> = if self.flying(me) {
            drone.thrust.map(|v| v as f32).to_vec()
        } else {
            vec![0.0; 4]
        };
        clamp(&mut rotors, 0.0, THRUST_MAX);

        let now = self.time(self.turn, 0);
        let mut course: Vec<f32> = Vec::with_capacity(GATES_AHEAD * 8);
        for ahead in 0..GATES_AHEAD {
            let gate = &self.course.gates[(drone.passes as usize + ahead) % GATES];
            course.extend(heading.point(gate.centre(now)));
            course.extend(heading.way(gate.way));
            course.extend(heading.vector(gate.velocity(now)));
        }
        clamp(&mut course, -POSITION_BOUND, POSITION_BOUND);

        // Every obstacle on the course, nearest first; the rest empty.
        let mut near: Vec<(f64, usize)> = self
            .course
            .obstacles
            .iter()
            .enumerate()
            .filter_map(|(index, o)| {
                let d = length(sub(o.as_ref()?.position(now), heading.origin));
                Some((if d.is_finite() { d } else { f64::MAX }, index))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut obstacles: Vec<f32> = Vec::with_capacity(OBSTACLES * 7);
        for slot in 0..OBSTACLES {
            match near.get(slot).and_then(|(_, i)| self.course.obstacles[*i]) {
                Some(o) => {
                    obstacles.extend(heading.point(o.position(now)));
                    obstacles.extend(heading.vector(o.velocity(now)));
                    obstacles.push(o.radius as f32);
                }
                None => obstacles.extend([0.0; 7]),
            }
        }
        clamp(&mut obstacles, -POSITION_BOUND, POSITION_BOUND);

        let mut others: Vec<(f64, usize)> = (0..self.seats as usize)
            .filter(|other| *other != me)
            .map(|other| {
                let d = length(sub(
                    self.sim.body_pos(self.drones[other].body),
                    heading.origin,
                ));
                (if d.is_finite() { d } else { f64::MAX }, other)
            })
            .collect();
        others.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut rivals: Vec<f32> = Vec::with_capacity(RIVALS * 7);
        for slot in 0..RIVALS {
            match others.get(slot) {
                Some(&(_, other)) => {
                    let body = self.drones[other].body;
                    rivals.extend(heading.point(self.sim.body_pos(body)));
                    rivals.extend(heading.vector(self.sim.body_linvel(body)));
                    rivals.push(if self.flying(other) { 1.0 } else { 0.0 });
                }
                None => rivals.extend([0.0; 7]),
            }
        }
        clamp(&mut rivals, -POSITION_BOUND, POSITION_BOUND);

        let mut bounds = vec![
            heading.to_bounds((heading.cos, heading.sin)),
            heading.to_bounds((-heading.cos, -heading.sin)),
            heading.to_bounds((-heading.sin, heading.cos)),
            heading.to_bounds((heading.sin, -heading.cos)),
            (f64::from(BOUND_UP) - heading.origin[2]) as f32,
        ];
        clamp(&mut bounds, 0.0, BOUNDS_BOUND);

        let finished = drone.finished.is_some();
        let mut race = vec![
            self.time_left(),
            (drone.passes / GATES as u32) as f32,
            if finished {
                0.0
            } else {
                (drone.passes % GATES as u32) as f32
            },
            drone.place as f32,
            drone.down as u8 as f32,
            drone.penalty as f32 * TURN_SECONDS,
            finished as u8 as f32,
            self.speed(me),
        ];
        let race_bounds = [
            RACE_SECONDS as f32,
            LAPS as f32,
            GATES as f32,
            MAX_SEATS as f32,
            1.0,
            PENALTY_SECONDS,
            1.0,
            VELOCITY_BOUND,
        ];
        for (value, max) in race.iter_mut().zip(race_bounds) {
            *value = if value.is_finite() {
                value.clamp(0.0, max)
            } else {
                0.0
            };
        }

        vec![
            Values::F32(imu),
            Values::F32(rotors),
            Values::F32(course),
            Values::F32(rivals),
            Values::F32(bounds),
            Values::F32(race),
            Values::F32(obstacles),
        ]
    }

    fn step(&mut self, outputs: &[Option<Vec<Values>>], report: &mut Report) {
        self.turn += 1;
        let seats = self.seats as usize;
        for (seat, output) in outputs.iter().enumerate().take(seats) {
            self.drones[seat].flags = 0;
            let values = output
                .as_ref()
                .and_then(|values| values.first())
                .and_then(Values::as_f32)
                .filter(|values| values.len() == 4);
            if let Some(values) = values
                && self.flying(seat)
            {
                for (thrust, value) in self.drones[seat].thrust.iter_mut().zip(values) {
                    *thrust = f64::from(*value);
                }
            }
        }
        self.apply_thrust();

        // The physics, noting each drone's hardest contact.
        let mut hardest = vec![0.0_f32; seats];
        for step in 0..SUBSTEPS {
            self.drive(self.time(self.turn - 1, step));
            self.sim.step();
            self.contacts.observe(&self.sim);
            for contact in self.sim.contacts() {
                let force = contact.normal_force as f32;
                for body in [contact.body1, contact.body2] {
                    if let Some(seat) = self.drone_bodies.iter().position(|b| *b == body) {
                        hardest[seat] = hardest[seat].max(force);
                    }
                }
            }
        }
        if self.sim.diverged() {
            self.recover();
            hardest.fill(0.0);
        }
        // The movers end the turn exactly where their motion says.
        self.drive(self.time(self.turn, 0));
        self.sim.forward();
        let (then, now) = (self.time(self.turn - 1, 0), self.time(self.turn, 0));

        let mut shaping = vec![0.0_f32; seats];
        let before: Vec<(u32, u32, u32)> = (0..seats).map(|s| self.running_key(s)).collect();
        let finishers_before = self.drones.iter().filter(|d| d.finished.is_some()).count();
        for seat in 0..seats {
            let pos = self.sim.body_pos(self.drones[seat].body);
            if self.drones[seat].retired {
                self.drones[seat].previous = pos;
                continue;
            }
            if self.drones[seat].finished.is_none() {
                let drone = &mut self.drones[seat];
                drone.raced += 1;
                if !drone.down {
                    let flown = length(sub(pos, drone.previous));
                    if flown.is_finite() && flown < 1.0 {
                        drone.flown += flown;
                    }
                }
            }

            // A crashed drone serves its penalty, then goes back in the air.
            if self.drones[seat].down {
                let drone = &mut self.drones[seat];
                drone.penalty = drone.penalty.saturating_sub(1);
                if drone.penalty == 0 {
                    self.respawn(seat);
                    self.drones[seat].flags |= flags::RESPAWN;
                    report.event(RESPAWN, Some(seat as u32));
                } else {
                    drone.previous = pos;
                }
                continue;
            }

            // Crashes: a hard hit, upside down too long, or out of bounds.
            let r = rotation(self.sim.body_quat(self.drones[seat].body));
            let upside_down = r[2][2] < 0.0;
            let drone = &mut self.drones[seat];
            drone.flipped_turns = if upside_down {
                drone.flipped_turns + 1
            } else {
                0
            };
            let out = !pos.iter().all(|v| v.is_finite())
                || pos[0].abs() > f64::from(BOUND_X)
                || pos[1].abs() > f64::from(BOUND_Y)
                || pos[2] > f64::from(BOUND_UP);
            if hardest[seat] >= CRASH_FORCE || drone.flipped_turns >= FLIPPED_TURNS || out {
                drone.flags |= flags::CRASH;
                drone.flipped_turns = 0;
                drone.previous = pos;
                report.event(CRASH, Some(seat as u32));
                if drone.finished.is_some() {
                    drone.retired = true;
                } else {
                    drone.down = true;
                    drone.penalty = PENALTY_TURNS;
                    drone.crashes += 1;
                    shaping[seat] -= 20.0;
                }
                continue;
            }

            // Gates, laps and the finish.
            if drone.finished.is_none() {
                let gate = self.course.gates[drone.passes as usize % GATES];
                let previous = drone.previous;
                if gate.passed(previous, pos, then, now) {
                    let drone = &mut self.drones[seat];
                    drone.passes += 1;
                    drone.last_pass_turn = self.turn;
                    drone.flags |= flags::GATE;
                    shaping[seat] += 10.0;
                    report.event(GATE, Some(seat as u32));
                    if drone.passes.is_multiple_of(GATES as u32) {
                        let lap = self.turn - drone.lap_start;
                        drone.best_lap = Some(drone.best_lap.map_or(lap, |best| best.min(lap)));
                        drone.lap_start = self.turn;
                        drone.flags |= flags::LAP;
                        shaping[seat] += 50.0;
                        report.event(LAP, Some(seat as u32));
                    }
                    if drone.passes >= GATE_PASSES {
                        drone.finished = Some(self.turn);
                        drone.flags |= flags::FINISH;
                        shaping[seat] += 100.0 - 10.0 * finishers_before as f32;
                        report.event(FINISH, Some(seat as u32));
                    }
                }
            }
            self.drones[seat].previous = pos;
        }

        // The running order, and overtakes once the first gate is behind.
        let places = self.places();
        for seat in 0..seats {
            let key = self.running_key(seat);
            let passed = before
                .iter()
                .enumerate()
                .filter(|(other, other_key)| {
                    *other != seat && **other_key < before[seat] && self.running_key(*other) > key
                })
                .count();
            if passed > 0 && self.drones[seat].passes > 1 {
                report.event(OVERTAKE, Some(seat as u32));
            }
            self.drones[seat].place = places[seat];
        }

        // Shaping: progress toward the next gate, steadiness.
        for (seat, reward) in shaping.iter_mut().enumerate() {
            let gap = self.gap(seat);
            let drone = &self.drones[seat];
            if self.flying(seat) && drone.finished.is_none() && drone.flags & flags::GATE == 0 {
                *reward += 0.1 * (drone.gap - gap);
            }
            if self.flying(seat) {
                let r = rotation(self.sim.body_quat(drone.body));
                let tilt = r[2][2].clamp(-1.0, 1.0).acos() as f32;
                let spin = length(self.sim.body_angvel(drone.body)) as f32;
                if tilt.is_finite() && spin.is_finite() {
                    *reward -= 0.01 * tilt + 0.005 * spin.min(GYRO_BOUND);
                }
            }
            report.shaping(seat as u32, *reward);
            self.drones[seat].gap = gap;
        }

        if self.ending.is_none() {
            if self.drones.iter().all(|d| d.finished.is_some()) {
                self.ending = Some(Ending::AllFinished);
            } else if self.turn >= MAX_TURNS {
                self.ending = Some(Ending::Time);
            }
        }
        self.apply_thrust();
        self.series(report);
        self.make_frame();
    }

    fn frame(&self) -> Vec<u8> {
        self.last.encode()
    }

    fn finish(&self, out_of_turns: bool) -> Option<Finish> {
        if self.ending.is_none() && !out_of_turns {
            return None;
        }
        // Finished drones by finish turn; the rest by gates passed, then
        // distance to the next gate. Exactly equal shares a place.
        let key = |seat: usize| {
            let drone = &self.drones[seat];
            match drone.finished {
                Some(turn) => (0, turn, 0.0),
                None => (1, u32::MAX - drone.passes, f64::from(self.gap(seat))),
            }
        };
        let mut order: Vec<usize> = (0..self.seats as usize).collect();
        order.sort_by(|a, b| {
            let (ka, kb) = (key(*a), key(*b));
            (ka.0, ka.1).cmp(&(kb.0, kb.1)).then(ka.2.total_cmp(&kb.2))
        });
        let mut places: Vec<Vec<u32>> = Vec::new();
        let mut last_key = None;
        for seat in order {
            let k = key(seat);
            match (places.last_mut(), last_key) {
                (Some(place), Some(prev)) if prev == k => place.push(seat as u32),
                _ => places.push(vec![seat as u32]),
            }
            last_key = Some(k);
        }
        let decided_by = match self.drones[places[0][0] as usize].finished {
            Some(turn) if places[0].len() > 1 => format!("A dead heat in {}", race_time(turn)),
            Some(turn) => format!("First to the finish in {}", race_time(turn)),
            None => "Time ran out: placed by gates passed".to_string(),
        };
        let full = f64::from(RACE_SECONDS);
        let seconds = |turns: u32| f64::from(turns) * f64::from(TURN_SECONDS);
        Some(
            Finish::new(places, decided_by)
                .stat(
                    FINISH_TIME,
                    self.drones
                        .iter()
                        .map(|d| d.finished.map_or(full, seconds))
                        .collect(),
                )
                .stat(
                    LAPS_DONE,
                    self.drones
                        .iter()
                        .map(|d| f64::from(d.passes / GATES as u32))
                        .collect(),
                )
                .stat(
                    GATES_PASSED,
                    self.drones.iter().map(|d| f64::from(d.passes)).collect(),
                )
                .stat(
                    CRASHES,
                    self.drones.iter().map(|d| f64::from(d.crashes)).collect(),
                )
                .stat(
                    BEST_LAP,
                    self.drones
                        .iter()
                        .map(|d| d.best_lap.map_or(full, seconds))
                        .collect(),
                )
                .stat(
                    AVG_SPEED,
                    self.drones
                        .iter()
                        .map(|d| {
                            let time = seconds(d.raced);
                            if time > 0.0 { d.flown / time } else { 0.0 }
                        })
                        .collect(),
                ),
        )
    }
}

impl Arena {
    /// For tests: the course as laid out this session.
    pub fn course(&self) -> &Course {
        &self.course
    }

    /// For tests: where every obstacle body is in the simulation.
    pub fn obstacle_positions(&self) -> Vec<[f64; 3]> {
        (1..=OBSTACLES)
            .filter_map(|n| self.sim.body_id(&format!("obstacle_{n}")))
            .map(|body| self.sim.body_pos(body))
            .collect()
    }

    /// For tests: the simulation.
    pub fn sim(&self) -> &MjSim {
        &self.sim
    }

    /// For tests: the race clock at the end of the latest turn.
    pub fn now(&self) -> f64 {
        self.time(self.turn, 0)
    }

    /// For tests: each seat's gates passed, crashes, down, and finish turn.
    pub fn standings(&self) -> Vec<(u32, u32, bool, Option<u32>)> {
        self.drones
            .iter()
            .map(|d| (d.passes, d.crashes, d.down, d.finished))
            .collect()
    }

    /// For tests: a seat's drone position.
    pub fn position(&self, seat: u32) -> [f64; 3] {
        self.sim.body_pos(self.drones[seat as usize].body)
    }

    /// For tests: put a seat's drone at rest at `pos`, level, facing `yaw`.
    pub fn teleport(&mut self, seat: u32, pos: [f64; 3], yaw: f64) {
        self.place_drone(seat as usize, pos, yaw_quat(yaw));
    }

    /// For tests: the bodies of every drone the model carries.
    pub fn drone_body_names(&self) -> Vec<String> {
        self.drone_bodies_all
            .iter()
            .filter_map(|body| self.sim.body_name(*body))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    //! Rules driven by setting state directly, where no test policy reaches
    //! them reliably.

    use super::*;

    fn arena(seats: u32) -> (Arena, Report) {
        let mut report = Report::new(&declaration(), seats);
        let arena = Arena::setup(&Seed::from_u64(1), seats, &mut report).unwrap();
        (arena, report)
    }

    fn step(arena: &mut Arena, report: &mut Report, turns: u32) -> Vec<(u32, Option<u32>)> {
        let mut events = Vec::new();
        let none = vec![None; arena.seats as usize];
        for _ in 0..turns {
            report.next_turn();
            arena.step(&none, report);
            events.extend(report.events().iter().copied());
        }
        events
    }

    #[test]
    fn race_times_read_as_minutes_seconds_tenths() {
        assert_eq!(race_time(6240), "1:02.4");
        assert_eq!(race_time(9000), "1:30.0");
        assert_eq!(race_time(512), "0:05.1");
    }

    #[test]
    fn a_crash_cuts_the_motors_then_respawns_above_the_last_gate() {
        let (mut arena, mut report) = arena(2);
        let (crash, respawn) = (
            declaration().event_index(CRASH.0).unwrap() as u32,
            declaration().event_index(RESPAWN.0).unwrap() as u32,
        );
        // Seat 0 has passed gate 3, and is flying out of bounds.
        arena.drones[0].passes = 3;
        arena.place_drone(0, [0.0, 0.0, BOUND_UP as f64 + 1.0], yaw_quat(0.0));
        arena.drones[0].thrust = [HOVER_THRUST as f64; 4];
        let events = step(&mut arena, &mut report, 1);
        assert!(events.contains(&(crash, Some(0))), "no crash in {events:?}");
        assert!(arena.drones[0].down);
        assert_eq!(arena.drones[0].crashes, 1);
        let events = step(&mut arena, &mut report, PENALTY_TURNS);
        assert!(
            events.contains(&(respawn, Some(0))),
            "no respawn in {events:?}"
        );
        assert!(!arena.drones[0].down);
        let gate = arena.course.gates[2];
        let p = arena.position(0);
        assert!(
            (p[2] - (gate.base[2] + gate.bob.amplitude + RESPAWN_HEIGHT as f64)).abs() < 0.1,
            "at {p:?}"
        );
        assert!(length(sub([p[0], p[1], 0.0], [gate.base[0], gate.base[1], 0.0])) < 1.5);
        assert_eq!(arena.drones[0].thrust, [HOVER_THRUST as f64; 4]);
    }

    #[test]
    fn a_drone_hovering_after_respawn_stays_up() {
        let (mut arena, mut report) = arena(2);
        arena.respawn(1);
        let start = arena.position(1);
        step(&mut arena, &mut report, 200);
        let p = arena.position(1);
        assert!((p[2] - start[2]).abs() < 0.3, "{start:?} → {p:?}");
        assert!(!arena.drones[1].down);
    }

    #[test]
    fn a_drone_flipped_on_the_ground_crashes() {
        let (mut arena, mut report) = arena(2);
        let pad = arena.drones[0].pad;
        arena.place_drone(0, [pad[0], pad[1], 0.2], [0.0, 1.0, 0.0, 0.0]);
        step(&mut arena, &mut report, FLIPPED_TURNS + 50);
        assert_eq!(arena.drones[0].crashes, 1);
    }

    #[test]
    fn the_last_gate_of_the_third_lap_finishes() {
        let (mut arena, mut report) = arena(3);
        let finish = declaration().event_index(FINISH.0).unwrap() as u32;
        let gate = arena.course.gates[GATES - 1];
        arena.drones[2].passes = GATE_PASSES - 1;
        let c = gate.centre(arena.now());
        let before = [c[0] - gate.way[0] * 0.3, c[1] - gate.way[1] * 0.3, c[2]];
        arena.place_drone(2, before, yaw_quat(gate.way[1].atan2(gate.way[0])));
        let mut velocities = arena.sim.qvel().to_vec();
        let qvel = arena.drones[2].qvel;
        velocities[qvel] = gate.way[0] * 5.0;
        velocities[qvel + 1] = gate.way[1] * 5.0;
        arena.sim.set_qvel(&velocities);
        arena.drones[2].thrust = [HOVER_THRUST as f64; 4];
        let events = step(&mut arena, &mut report, 20);
        assert!(
            events.contains(&(finish, Some(2))),
            "no finish in {events:?}"
        );
        assert!(arena.drones[2].finished.is_some());
        let result = arena.finish(true).unwrap();
        assert_eq!(result.places[0], vec![2]);
        assert!(
            result
                .decided_by
                .starts_with("First to the finish in 0:00."),
            "{}",
            result.decided_by
        );
    }

    #[test]
    fn at_the_limit_gates_then_distance_decide() {
        let (mut arena, _) = arena(4);
        arena.drones[0].passes = 5;
        arena.drones[1].passes = 7;
        arena.drones[2].passes = 7;
        arena.drones[3].passes = 0;
        // Seat 2 nearer its next gate than seat 1.
        let next = arena.course.gates[7].centre(arena.now());
        arena.place_drone(2, [next[0] + 0.5, next[1], next[2] + 2.0], yaw_quat(0.0));
        arena.place_drone(1, [next[0] + 4.0, next[1], next[2] + 2.0], yaw_quat(0.0));
        arena.sim.forward();
        let result = arena.finish(true).unwrap();
        assert_eq!(result.places, vec![vec![2], vec![1], vec![0], vec![3]]);
        assert_eq!(result.decided_by, "Time ran out: placed by gates passed");
    }

    #[test]
    fn finishers_rank_ahead_and_a_shared_finish_turn_is_a_dead_heat() {
        let (mut arena, _) = arena(3);
        arena.drones[0].finished = Some(5000);
        arena.drones[0].passes = GATE_PASSES;
        arena.drones[1].finished = Some(5000);
        arena.drones[1].passes = GATE_PASSES;
        arena.drones[2].passes = 23;
        let result = arena.finish(true).unwrap();
        assert_eq!(result.places, vec![vec![0, 1], vec![2]]);
        assert_eq!(result.decided_by, "A dead heat in 0:50.0");
    }

    #[test]
    fn seats_beyond_the_field_are_parked_off_the_course() {
        let (arena, _) = arena(2);
        for index in 2..DRONES {
            let p = arena.sim.body_pos(arena.drone_bodies_all[index]);
            assert!(p[1] > BOUND_Y as f64, "drone {} at {p:?}", index + 1);
        }
    }

    #[test]
    fn the_arena_section_is_the_documented_size() {
        for seats in [MIN_SEATS, MAX_SEATS] {
            let (arena, _) = arena(seats);
            assert_eq!(
                arena.arena_section().len(),
                ARENA_SECTION_HEADER
                    + ARENA_SECTION_PER_SEAT * seats as usize
                    + ARENA_SECTION_TRAILER
            );
        }
    }

    #[test]
    fn a_blow_up_puts_every_drone_back_without_counting() {
        let (mut arena, mut report) = arena(2);
        arena.drones[0].passes = 2;
        arena.recover();
        step(&mut arena, &mut report, 1);
        assert_eq!(arena.drones[0].crashes, 0);
        assert_eq!(arena.drones[0].passes, 2);
        assert!(arena.position(0)[2] > 2.0);
    }
}
