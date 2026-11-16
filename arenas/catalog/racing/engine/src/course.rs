//! The course, laid out from the seed every session: its shape, where the
//! gates stand and how the moving ones move, and the obstacles. Pure
//! geometry with no simulation, so the engine and the fairness tests share
//! it. The design note (`../README.md`) has the rules in words.
//!
//! Every course the seed can draw keeps the start identical (the pads, gate
//! 1 and the start straight never move), keeps every gate and obstacle well
//! inside the airspace, and keeps every obstacle's whole sweep out of the
//! corridor between consecutive gates, off every gate's frame and legs and
//! clear of every respawn spot, so no obstacle can block the only line.
//! [`problems`] checks all of it; a layout that fails is drawn again.

use arena_kit::seed::Rng;
use racing_declaration::*;

/// The start straight runs east along `START_Y` from `START_X`; gate 1
/// stands on it at `GATE_1`, the same in every session.
pub const START_X: f64 = -14.0;
pub const START_Y: f64 = -8.0;
pub const GATE_1: [f64; 3] = [-6.0, -8.0, 2.0];
/// The straights' length and the bends' radius, drawn per session.
pub const STRAIGHT: (f64, f64) = (18.0, 28.0);
pub const BEND: (f64, f64) = (6.0, 10.0);
/// One session in three is a kidney: the east bend tighter than the west,
/// its radius less by half of a pull of up to 6 m, so the straights
/// converge toward it.
pub const KIDNEY_CHANCE: f64 = 1.0 / 3.0;
pub const KIDNEY_PULL: f64 = 6.0;
pub const KIDNEY_BEND_MIN: f64 = 4.0;
/// Gates 2 to 8 sit a pitch apart along the lap (an eighth of it), each
/// moved by up to this share of a pitch.
pub const PITCH_JITTER: f64 = 0.15;
/// A gate centre's height, its offset sideways from the line, and its turn
/// from the line's heading.
pub const HEIGHT: (f64, f64) = (1.5, 5.0);
pub const OFFSET: f64 = 2.0;
pub const YAW: f64 = 20.0 * std::f64::consts::PI / 180.0;
/// The most a lap's consecutive gates may climb or drop, at any moment, and
/// lie apart.
pub const CLIMB: f64 = 3.0;
pub const SPACING: f64 = 30.0;
/// How many of gates 2 to 8 move, and how: a sideways sway, a bob up and
/// down, or both, each a sine.
pub const MOVERS: (u32, u32) = (2, 4);
pub const SWAY: (f64, f64) = (0.5, 2.0);
pub const BOB: (f64, f64) = (0.3, 1.0);
pub const PERIOD: (f64, f64) = (4.0, 10.0);
/// Margins: every gate (its whole motion) stays this far inside the
/// airspace's walls, and any two gates' centres this far apart.
pub const AIRSPACE_SPARE: f64 = 3.0;
pub const GATE_SEPARATION: f64 = 5.0;
/// The obstacles: how many a session uses, each sphere's radius (fixed in
/// the model), and how they move: drifting to and fro on a line, or
/// orbiting level, over this reach and period.
pub const OBSTACLES: usize = 8;
pub const USED: (u32, u32) = (3, 8);
pub const RADII: [f64; OBSTACLES] = [0.4, 0.4, 0.4, 0.6, 0.6, 0.6, 0.8, 0.8];
pub const REACH: (f64, f64) = (1.0, 3.0);
pub const DRIFT: (f64, f64) = (6.0, 14.0);
/// An obstacle's surface stays this far out of the corridor between
/// consecutive gates (the hull of their whole motion), this far from any
/// gate's frame or legs, and this far from any respawn spot.
pub const CORRIDOR: f64 = 1.5;
pub const FRAME_CLEARANCE: f64 = 0.5;
pub const RESPAWN_CLEARANCE: f64 = 1.0;
/// The start area obstacles never enter: the pads, gate 1 and the air
/// between them, plus this margin.
pub const START_BOX: ([f64; 3], [f64; 3]) = ([-17.0, -12.0, 0.0], [-2.0, -4.0, 6.0]);
/// Where unused obstacles wait: on the ground south of the airspace.
pub const PARK: [f64; 2] = [-14.0, -26.0];
pub const PARK_SPACING: f64 = 4.0;
/// Respawn spots sit this far each side of a gate's centre at most: the
/// six seats' offsets.
pub const RESPAWN_REACH: f64 = (MAX_SEATS as f64 - 1.0) / 2.0 * RESPAWN_SPACING as f64;

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// The distance from `p` to the segment `a`–`b`.
pub fn to_segment(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let t = (dot(sub(p, a), ab) / dot(ab, ab).max(1e-12)).clamp(0.0, 1.0);
    length(sub(p, add(a, scale(ab, t))))
}

/// A sine: `amplitude · sin(2π t / period + phase)`, and its rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wave {
    pub amplitude: f64,
    pub period: f64,
    pub phase: f64,
}

impl Wave {
    pub const STILL: Self = Self {
        amplitude: 0.0,
        period: 1.0,
        phase: 0.0,
    };

    fn draw(rng: &mut Rng, amplitude: (f64, f64), period: (f64, f64)) -> Self {
        Self {
            amplitude: rng.range(amplitude.0, amplitude.1),
            period: rng.range(period.0, period.1),
            phase: rng.range(0.0, std::f64::consts::TAU),
        }
    }

    pub fn at(&self, t: f64) -> f64 {
        self.amplitude * (std::f64::consts::TAU * t / self.period + self.phase).sin()
    }

    pub fn rate(&self, t: f64) -> f64 {
        let w = std::f64::consts::TAU / self.period;
        self.amplitude * w * (w * t + self.phase).cos()
    }
}

/// One gate as placed for this session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// Its centre at rest.
    pub base: [f64; 3],
    /// The way through, level and unit length, and left of it.
    pub way: [f64; 3],
    pub side: [f64; 3],
    /// Along `side`, and up.
    pub sway: Wave,
    pub bob: Wave,
}

impl Gate {
    pub fn yaw(&self) -> f64 {
        self.way[1].atan2(self.way[0])
    }

    pub fn moves(&self) -> bool {
        self.sway.amplitude > 0.0 || self.bob.amplitude > 0.0
    }

    pub fn centre(&self, t: f64) -> [f64; 3] {
        let mut c = add(self.base, scale(self.side, self.sway.at(t)));
        c[2] += self.bob.at(t);
        c
    }

    pub fn velocity(&self, t: f64) -> [f64; 3] {
        let mut v = scale(self.side, self.sway.rate(t));
        v[2] += self.bob.rate(t);
        v
    }

    /// Corners of its centre's motion, a 3 × 3 grid over sway and bob.
    pub fn envelope(&self) -> Vec<[f64; 3]> {
        let mut out = Vec::with_capacity(9);
        for s in [-1.0, 0.0, 1.0] {
            for b in [-1.0, 0.0, 1.0] {
                let mut c = add(self.base, scale(self.side, s * self.sway.amplitude));
                c[2] += b * self.bob.amplitude;
                out.push(c);
            }
        }
        out
    }

    /// Whether `from` → `to`, a drone's move over a turn, goes through the
    /// opening from the front, judged in the gate's own frame: the gate at
    /// `then` at the start, at `now` at the end.
    pub fn passed(&self, from: [f64; 3], to: [f64; 3], then: f64, now: f64) -> bool {
        let (r0, r1) = (sub(from, self.centre(then)), sub(to, self.centre(now)));
        let (a, b) = (dot(r0, self.way), dot(r1, self.way));
        if !(a < 0.0 && b >= 0.0 && b - a > 0.0) {
            return false;
        }
        let t = a / (a - b);
        let r = add(r0, scale(sub(r1, r0), t));
        let half = f64::from(GATE_HALF);
        dot(r, self.side).abs() < half && r[2].abs() < half
    }

    /// The frame's four edges and two legs, at its centre `c`.
    pub fn parts(&self, c: [f64; 3]) -> Vec<([f64; 3], [f64; 3])> {
        let h = 1.05;
        let corner = |s: f64, u: f64| add(add(c, scale(self.side, s)), [0.0, 0.0, u]);
        let (bl, br, tl, tr) = (corner(-h, -h), corner(h, -h), corner(-h, h), corner(h, h));
        let ground = |p: [f64; 3]| [p[0], p[1], 0.0];
        vec![
            (tl, tr),
            (bl, br),
            (bl, tl),
            (br, tr),
            (bl, ground(bl)),
            (br, ground(br)),
        ]
    }

    /// Where the respawn spots above it run: a level segment along its side
    /// at rest, above the highest it bobs.
    pub fn respawn_line(&self) -> ([f64; 3], [f64; 3]) {
        let z = self.base[2] + self.bob.amplitude + f64::from(RESPAWN_HEIGHT);
        let at = |s: f64| {
            let p = add(self.base, scale(self.side, s));
            [p[0], p[1], z]
        };
        (at(-RESPAWN_REACH), at(RESPAWN_REACH))
    }
}

/// How an obstacle moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// To and fro along a unit direction.
    Drift { direction: [f64; 3], wave: Wave },
    /// Round a level circle, anticlockwise or not.
    Orbit { wave: Wave, clockwise: bool },
}

/// One obstacle a session uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Obstacle {
    pub radius: f64,
    pub centre: [f64; 3],
    pub motion: Motion,
}

impl Obstacle {
    pub fn position(&self, t: f64) -> [f64; 3] {
        match self.motion {
            Motion::Drift { direction, wave } => add(self.centre, scale(direction, wave.at(t))),
            Motion::Orbit { wave, clockwise } => {
                let angle = std::f64::consts::TAU * t / wave.period + wave.phase;
                let s = if clockwise { -1.0 } else { 1.0 };
                add(
                    self.centre,
                    [
                        wave.amplitude * angle.cos(),
                        s * wave.amplitude * angle.sin(),
                        0.0,
                    ],
                )
            }
        }
    }

    pub fn velocity(&self, t: f64) -> [f64; 3] {
        match self.motion {
            Motion::Drift { direction, wave } => scale(direction, wave.rate(t)),
            Motion::Orbit { wave, clockwise } => {
                let w = std::f64::consts::TAU / wave.period;
                let angle = w * t + wave.phase;
                let s = if clockwise { -1.0 } else { 1.0 };
                [
                    -wave.amplitude * w * angle.sin(),
                    s * wave.amplitude * w * angle.cos(),
                    0.0,
                ]
            }
        }
    }

    fn period(&self) -> f64 {
        match self.motion {
            Motion::Drift { wave, .. } | Motion::Orbit { wave, .. } => wave.period,
        }
    }

    /// Its centre through one whole period, at 32 moments.
    pub fn sweep(&self) -> Vec<[f64; 3]> {
        (0..32)
            .map(|i| self.position(self.period() * f64::from(i) / 32.0))
            .collect()
    }
}

/// A session's course.
#[derive(Debug, Clone, PartialEq)]
pub struct Course {
    pub gates: [Gate; GATES],
    /// Each obstacle body, used or parked.
    pub obstacles: [Option<Obstacle>; OBSTACLES],
    pub straight: f64,
    pub bend: f64,
    /// The east bend's radius: the same as `bend` unless it's a kidney.
    pub east_bend: f64,
}

/// The racing line the gates are spaced along: the start straight east, the
/// east bend anticlockwise, the north side west, the west bend.
struct Line {
    points: Vec<[f64; 3]>,
    /// Arc length at each point.
    at: Vec<f64>,
}

impl Line {
    fn new(straight: f64, bend: f64, east_bend: f64) -> Self {
        use std::f64::consts::{PI, TAU};
        let west = [START_X, START_Y + bend];
        let east = [START_X + straight, START_Y + east_bend];
        // The north side: the outer tangent from the east circle to the west.
        let (dx, dy) = (west[0] - east[0], west[1] - east[1]);
        let d = (dx * dx + dy * dy).sqrt();
        let (ux, uy) = (dx / d, dy / d);
        let c = (east_bend - bend) / d;
        let s = (1.0 - c * c).max(0.0).sqrt();
        // n = c·u ± s·perp(u); the one facing north.
        let n = [c * ux + s * uy, c * uy - s * ux];
        let n = if n[1] < 0.0 {
            [c * ux - s * uy, c * uy + s * ux]
        } else {
            n
        };
        let t_east = [east[0] + east_bend * n[0], east[1] + east_bend * n[1]];
        let t_west = [west[0] + bend * n[0], west[1] + bend * n[1]];
        let mut points: Vec<[f64; 2]> = Vec::new();
        let step = 0.25;
        let line = |a: [f64; 2], b: [f64; 2], points: &mut Vec<[f64; 2]>| {
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let n = (len / step).ceil().max(1.0) as usize;
            for i in 0..n {
                let t = i as f64 / n as f64;
                points.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        };
        let arc = |centre: [f64; 2], r: f64, from: f64, to: f64, points: &mut Vec<[f64; 2]>| {
            let to = if to <= from { to + TAU } else { to };
            let n = (r * (to - from) / step).ceil().max(1.0) as usize;
            for i in 0..n {
                let a = from + (to - from) * i as f64 / n as f64;
                points.push([centre[0] + r * a.cos(), centre[1] + r * a.sin()]);
            }
        };
        line([START_X, START_Y], [east[0], START_Y], &mut points);
        arc(east, east_bend, -PI / 2.0, n[1].atan2(n[0]), &mut points);
        line(t_east, t_west, &mut points);
        arc(west, bend, n[1].atan2(n[0]), -PI / 2.0, &mut points);
        points.push([START_X, START_Y]);
        let mut at = vec![0.0];
        for pair in points.windows(2) {
            let d = ((pair[1][0] - pair[0][0]).powi(2) + (pair[1][1] - pair[0][1]).powi(2)).sqrt();
            at.push(at.last().unwrap() + d);
        }
        Self {
            points: points.iter().map(|p| [p[0], p[1], 0.0]).collect(),
            at,
        }
    }

    fn length(&self) -> f64 {
        *self.at.last().unwrap()
    }

    /// The point at arc length `s` (wrapping), and the heading there.
    fn point(&self, s: f64) -> ([f64; 3], f64) {
        let s = s.rem_euclid(self.length());
        let i = self
            .at
            .partition_point(|a| *a <= s)
            .clamp(1, self.at.len() - 1);
        let (a, b) = (self.points[i - 1], self.points[i]);
        let t = (s - self.at[i - 1]) / (self.at[i] - self.at[i - 1]).max(1e-12);
        (
            add(a, scale(sub(b, a), t)),
            (b[1] - a[1]).atan2(b[0] - a[0]),
        )
    }
}

/// Lay a course out from the seed, drawing again until it passes every
/// check. Each part draws from its own labelled stream.
pub fn draw(seed: &arena_kit::Seed) -> Course {
    let mut layout = seed.stream("layout");
    let mut motion = seed.stream("motion");
    let mut obstacles = seed.stream("obstacles");
    loop {
        let course = attempt(&mut layout, &mut motion, &mut obstacles);
        if problems(&course).is_empty() {
            return course;
        }
    }
}

fn attempt(layout: &mut Rng, motion: &mut Rng, obstacles: &mut Rng) -> Course {
    let straight = layout.range(STRAIGHT.0, STRAIGHT.1);
    let bend = layout.range(BEND.0, BEND.1);
    let east_bend = if layout.chance(KIDNEY_CHANCE) {
        (bend - layout.range(0.0, KIDNEY_PULL) / 2.0).max(KIDNEY_BEND_MIN)
    } else {
        bend
    };
    let line = Line::new(straight, bend, east_bend);
    let pitch = line.length() / GATES as f64;
    let first = GATE_1[0] - START_X;

    // Which of gates 2 to 8 move, and how, before the heights, which leave
    // room for the bobbing.
    let mut waves = [(Wave::STILL, Wave::STILL); GATES];
    let movers = MOVERS.0 + motion.below(MOVERS.1 - MOVERS.0 + 1);
    let mut order: Vec<usize> = (1..GATES).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, motion.below(i as u32 + 1) as usize);
    }
    for &index in order.iter().take(movers as usize) {
        let kind = motion.below(3);
        let sway = if kind != 1 {
            Wave::draw(motion, SWAY, PERIOD)
        } else {
            Wave::STILL
        };
        let bob = if kind != 0 {
            Wave::draw(motion, BOB, PERIOD)
        } else {
            Wave::STILL
        };
        waves[index] = (sway, bob);
    }

    let mut gates = [Gate {
        base: GATE_1,
        way: [1.0, 0.0, 0.0],
        side: [0.0, 1.0, 0.0],
        sway: Wave::STILL,
        bob: Wave::STILL,
    }; GATES];
    let mut previous = (GATE_1[2], 0.0);
    for index in 1..GATES {
        let (sway, bob) = waves[index];
        let s = first + pitch * (index as f64 + layout.range(-PITCH_JITTER, PITCH_JITTER));
        let (on_line, heading) = line.point(s);
        let normal = [-heading.sin(), heading.cos(), 0.0];
        let offset = layout.range(-OFFSET, OFFSET);
        // Height: room for the bob, and no more climb than CLIMB at any
        // moment from the gate before (and, for gate 8, to gate 1).
        let room = CLIMB - bob.amplitude - previous.1;
        let mut low = (HEIGHT.0 + bob.amplitude).max(previous.0 - room);
        let mut high = HEIGHT.1.min(previous.0 + room);
        if index == GATES - 1 {
            let room = CLIMB - bob.amplitude;
            low = low.max(GATE_1[2] - room);
            high = high.min(GATE_1[2] + room);
        }
        let height = if high > low {
            layout.range(low, high)
        } else {
            low
        };
        let yaw = heading + layout.range(-YAW, YAW);
        let way = [yaw.cos(), yaw.sin(), 0.0];
        gates[index] = Gate {
            base: [
                on_line[0] + normal[0] * offset,
                on_line[1] + normal[1] * offset,
                height,
            ],
            way,
            side: [-way[1], way[0], 0.0],
            sway,
            bob,
        };
        previous = (height, bob.amplitude);
    }

    let mut placed = [None; OBSTACLES];
    let used = USED.0 + obstacles.below(USED.1 - USED.0 + 1);
    let mut bodies: Vec<usize> = (0..OBSTACLES).collect();
    for i in (1..bodies.len()).rev() {
        bodies.swap(i, obstacles.below(i as u32 + 1) as usize);
    }
    for &body in bodies.iter().take(used as usize) {
        for _ in 0..60 {
            let obstacle = place(obstacles, RADII[body], &gates);
            if obstacle_problems(&obstacle, &gates).is_empty() {
                placed[body] = Some(obstacle);
                break;
            }
        }
    }
    Course {
        gates,
        obstacles: placed,
        straight,
        bend,
        east_bend,
    }
}

/// An obstacle drawn beside a stretch between two gates.
fn place(rng: &mut Rng, radius: f64, gates: &[Gate; GATES]) -> Obstacle {
    let k = rng.below(GATES as u32) as usize;
    let (a, b) = (gates[k].base, gates[(k + 1) % GATES].base);
    let along = add(a, scale(sub(b, a), rng.range(0.2, 0.8)));
    let dir = sub(b, a);
    let level = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt().max(1e-9);
    let normal = [-dir[1] / level, dir[0] / level, 0.0];
    let reach = rng.range(REACH.0, REACH.1);
    let out = radius + CORRIDOR + reach + 1.5 + rng.range(0.0, 3.0);
    let side = if rng.chance(0.5) { 1.0 } else { -1.0 };
    let mut centre = add(along, scale(normal, side * out));
    centre[2] = (along[2] + rng.range(-1.5, 2.5)).max(radius + reach + 1.0);
    let wave = Wave {
        amplitude: reach,
        period: rng.range(DRIFT.0, DRIFT.1),
        phase: rng.range(0.0, std::f64::consts::TAU),
    };
    let motion = if rng.chance(0.5) {
        let (theta, z) = (rng.range(0.0, std::f64::consts::TAU), rng.range(-0.5, 0.5));
        let norm = (1.0 + z * z).sqrt();
        Motion::Drift {
            direction: [theta.cos() / norm, theta.sin() / norm, z / norm],
            wave,
        }
    } else {
        Motion::Orbit {
            wave,
            clockwise: rng.chance(0.5),
        }
    };
    Obstacle {
        radius,
        centre,
        motion,
    }
}

/// Where an unused obstacle waits.
pub fn parked(index: usize) -> [f64; 3] {
    [PARK[0] + PARK_SPACING * index as f64, PARK[1], RADII[index]]
}

/// Everything wrong with one obstacle's sweep against the gates.
pub fn obstacle_problems(obstacle: &Obstacle, gates: &[Gate; GATES]) -> Vec<String> {
    let mut out = Vec::new();
    let r = obstacle.radius;
    let gate_times: Vec<f64> = (0..24).map(|i| f64::from(i) * 0.5).collect();
    for p in obstacle.sweep() {
        let inside = p[0].abs() + r <= f64::from(BOUND_X) - AIRSPACE_SPARE
            && p[1].abs() + r <= f64::from(BOUND_Y) - AIRSPACE_SPARE
            && p[2] - r >= 1.0
            && p[2] + r <= f64::from(BOUND_UP) - AIRSPACE_SPARE;
        if !inside {
            out.push(format!("its sweep leaves the airspace at {p:?}"));
            break;
        }
        let (lo, hi) = START_BOX;
        if (0..3).all(|axis| p[axis] + r > lo[axis] && p[axis] - r < hi[axis]) {
            out.push(format!("its sweep enters the start area at {p:?}"));
            break;
        }
        for k in 0..GATES {
            let (a, b) = (gates[k].envelope(), gates[(k + 1) % GATES].envelope());
            let near = a
                .iter()
                .flat_map(|pa| b.iter().map(move |pb| (pa, pb)))
                .map(|(pa, pb)| to_segment(p, *pa, *pb))
                .fold(f64::INFINITY, f64::min);
            if near < r + CORRIDOR {
                out.push(format!(
                    "its sweep comes within {near:.2} m of the line from gate {} to gate {}",
                    k + 1,
                    (k + 1) % GATES + 1
                ));
                return out;
            }
        }
        for (k, gate) in gates.iter().enumerate() {
            for t in &gate_times {
                for (a, b) in gate.parts(gate.centre(*t)) {
                    if to_segment(p, a, b) < r + FRAME_CLEARANCE {
                        out.push(format!("its sweep touches gate {}'s frame or legs", k + 1));
                        return out;
                    }
                }
            }
            let (a, b) = gate.respawn_line();
            if to_segment(p, a, b) < r + RESPAWN_CLEARANCE {
                out.push(format!("its sweep crosses gate {}'s respawn spots", k + 1));
                return out;
            }
        }
    }
    out
}

/// Everything wrong with a course, each naming what: the checks the design
/// note promises for every course the seed can draw.
pub fn problems(course: &Course) -> Vec<String> {
    let mut out = Vec::new();
    let gates = &course.gates;
    let spare = AIRSPACE_SPARE;
    for (k, gate) in gates.iter().enumerate() {
        for c in gate.envelope() {
            let inside = c[0].abs() + 1.05 <= f64::from(BOUND_X) - spare
                && c[1].abs() + 1.05 <= f64::from(BOUND_Y) - spare
                && c[2] + 1.05 <= f64::from(BOUND_UP) - spare;
            if !inside {
                out.push(format!(
                    "gate {} leaves the airspace's margin at {c:?}",
                    k + 1
                ));
                break;
            }
            if c[2] - f64::from(GATE_HALF) < 0.3 {
                out.push(format!(
                    "gate {}'s opening dips to the ground at {c:?}",
                    k + 1
                ));
                break;
            }
        }
        let next = &gates[(k + 1) % GATES];
        let apart = length(sub(next.base, gate.base));
        if apart > SPACING - gate.sway.amplitude - next.sway.amplitude {
            out.push(format!(
                "gates {} and {} are {apart:.1} m apart",
                k + 1,
                (k + 1) % GATES + 1
            ));
        }
        let climb = (next.base[2] - gate.base[2]).abs() + gate.bob.amplitude + next.bob.amplitude;
        if climb > CLIMB + 1e-9 {
            out.push(format!(
                "gates {} and {} climb {climb:.2} m",
                k + 1,
                (k + 1) % GATES + 1
            ));
        }
        for (j, other) in gates.iter().enumerate().skip(k + 1) {
            let near = gate
                .envelope()
                .iter()
                .flat_map(|a| {
                    other
                        .envelope()
                        .into_iter()
                        .map(move |b| length(sub(*a, b)))
                })
                .fold(f64::INFINITY, f64::min);
            if near < GATE_SEPARATION {
                out.push(format!(
                    "gates {} and {} come {near:.1} m apart",
                    k + 1,
                    j + 1
                ));
            }
        }
    }
    let still = Gate {
        base: GATE_1,
        way: [1.0, 0.0, 0.0],
        side: [0.0, 1.0, 0.0],
        sway: Wave::STILL,
        bob: Wave::STILL,
    };
    if gates[0] != still {
        out.push("gate 1 moved".into());
    }
    for (index, obstacle) in course.obstacles.iter().enumerate() {
        if let Some(obstacle) = obstacle {
            if (obstacle.radius - RADII[index]).abs() > 1e-9 {
                out.push(format!("obstacle {} isn't its body's size", index + 1));
            }
            for problem in obstacle_problems(obstacle, gates) {
                out.push(format!("obstacle {}: {problem}", index + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stadium_line_is_two_straights_and_two_half_circles() {
        let line = Line::new(24.0, 8.0, 8.0);
        let expected = 2.0 * 24.0 + std::f64::consts::TAU * 8.0;
        assert!((line.length() - expected).abs() < 0.1, "{}", line.length());
        let (p, heading) = line.point(8.0);
        assert!((p[0] - GATE_1[0]).abs() < 1e-6 && (p[1] - START_Y).abs() < 1e-6);
        assert!(heading.abs() < 1e-6);
    }

    #[test]
    fn a_kidney_converges_toward_its_tighter_bend() {
        let line = Line::new(24.0, 9.0, 6.0);
        let (p, _) = line.point(24.0 + 6.0 * std::f64::consts::PI + 1.0);
        // Just past the east bend, the north side is below the west bend's top.
        assert!(p[1] < START_Y + 2.0 * 9.0, "{p:?}");
    }

    #[test]
    fn a_moving_gate_counts_from_its_own_frame() {
        let gate = Gate {
            base: [0.0, 0.0, 2.0],
            way: [1.0, 0.0, 0.0],
            side: [0.0, 1.0, 0.0],
            sway: Wave {
                amplitude: 2.0,
                period: 4.0,
                phase: 0.0,
            },
            bob: Wave::STILL,
        };
        // At t = 1 the gate has swayed 2 m left: through it there…
        assert!(gate.passed([-0.1, 2.0, 2.0], [0.1, 2.0, 2.0], 1.0, 1.0));
        // …and not through where it stood at rest.
        assert!(!gate.passed([-0.1, 0.0, 2.0], [0.1, 0.0, 2.0], 1.0, 1.0));
        assert!(
            !gate.passed([0.1, 2.0, 2.0], [-0.1, 2.0, 2.0], 1.0, 1.0),
            "backwards"
        );
    }
}
