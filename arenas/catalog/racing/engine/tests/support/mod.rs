//! Test policies. They read the declaration crate only, never the engine, as
//! the real test bots in REPOS/BOTS will.

#![allow(dead_code)]

use arena_kit::Values;
use arena_kit::declaration::Declaration;
use arena_kit::testing::Policy;
use racing_declaration::*;

/// Where a named slice of a named input starts.
pub fn slice_start(declaration: &Declaration, input: &str, slice: &str) -> usize {
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

pub fn input_index(declaration: &Declaration, name: &str) -> usize {
    declaration
        .inputs
        .iter()
        .position(|v| v.name == name)
        .expect("declared input")
}

fn read(
    declaration: &Declaration,
    input: &[Values],
    name: &str,
    slice: &str,
    n: usize,
) -> Vec<f32> {
    let values = input[input_index(declaration, name)]
        .as_f32()
        .expect("f32 input");
    let start = slice_start(declaration, name, slice);
    values[start..start + n].to_vec()
}

/// Rotor thrusts for a total thrust and body torques, through the X2's
/// geometry, each clamped to the motors' range.
pub fn mix(thrust: f32, torque: [f32; 3]) -> [f32; 4] {
    // Solve [1 1 1 1; y; -x; yaw] f = [T, τx, τy, τz] for the X2's symmetric
    // layout in closed form.
    let (ax, ay, k) = (0.14_f32, 0.18_f32, 0.0201_f32);
    let t = thrust / 4.0;
    let roll = torque[0] / (4.0 * ay);
    let pitch = torque[1] / (4.0 * ax);
    let yaw = torque[2] / (4.0 * k);
    // Rotor i: y_i * roll sign, -x_i * pitch sign, yaw sign.
    let mut out = [0.0; 4];
    for (i, rotor) in ROTORS.iter().enumerate() {
        let sy = rotor[1].signum();
        let sx = -rotor[0].signum();
        let sk = YAW_TORQUE[i].signum();
        out[i] = (t + sy * roll + sx * pitch + sk * yaw).clamp(0.0, THRUST_MAX);
    }
    out
}

/// The scripted baseline: a cascaded classical controller. Position to
/// velocity to acceleration, acceleration to a tilt and a total thrust,
/// tilt to body torques from the gravity reading and the gyro, and the
/// mixer to rotor thrusts. It flies at the next gate, leading in along the
/// way through it and aiming where a moving gate will be when it gets
/// there, steers clear of obstacles and eases off when a rival is close,
/// and climbs clear and holds once finished.
pub struct Scripted {
    pub cruise: f32,
    /// Gates passed, as the race input last said, and turns left flying
    /// straight out of the gate just passed.
    passed: f32,
    exit: u32,
}

/// It gives way to a rival nearer the gate within this distance of it.
pub const YIELD_NEAR: f32 = 8.0;
/// Turns to fly straight on after a gate, clear of its frame.
pub const EXIT_TURNS: u32 = 25;

impl Scripted {
    pub fn new() -> Self {
        Self::with_cruise(5.5)
    }

    pub fn with_cruise(cruise: f32) -> Self {
        Self {
            cruise,
            passed: 0.0,
            exit: 0,
        }
    }
}

impl Default for Scripted {
    fn default() -> Self {
        Self::new()
    }
}

fn norm(v: &[f32]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

impl Policy for Scripted {
    fn act(&mut self, declaration: &Declaration, _: u32, input: &[Values]) -> Option<Vec<Values>> {
        let r = |name: &str, slice: &str, n: usize| read(declaration, input, name, slice, n);
        let race = r(IN_RACE, "time_left", 8);
        let (down, finished) = (race[4] > 0.5, race[6] > 0.5);
        if down {
            self.exit = 0;
            return None;
        }
        // A gate just passed: fly straight on out of it for a moment, so a
        // swaying frame can't catch the drone turning across it.
        let passed = race[1] * GATES as f32 + race[2];
        if passed != self.passed {
            self.exit = if passed > self.passed { EXIT_TURNS } else { 0 };
            self.passed = passed;
        }
        let exiting = self.exit > 0;
        self.exit = self.exit.saturating_sub(1);
        let g = r(IN_IMU, "gravity", 3);
        let w = r(IN_IMU, "gyro", 3);
        let v = r(IN_IMU, "velocity", 3);
        let height = r(IN_IMU, "height", 1)[0];

        // The velocity wanted, in the heading frame.
        let mut vd = if exiting && !finished {
            [v[0], v[1], v[2]]
        } else if finished {
            [-0.5 * v[0], -0.5 * v[1], (6.0 - height).clamp(-1.0, 2.0)]
        } else {
            let now = r(IN_COURSE, "next", 3);
            let way = r(IN_COURSE, "next_way", 2);
            let gv = r(IN_COURSE, "next_velocity", 3);
            let n = [way[1], way[0], 0.0];
            // Where a moving gate will be on arrival, near enough: its
            // velocity carried forward for the time to get there.
            let eta = (norm(&now) / self.cruise).min(1.0);
            let c = [
                now[0] + gv[0] * eta,
                now[1] + gv[1] * eta,
                now[2] + gv[2] * eta,
            ];
            // How far ahead the gate's plane is along the way through it.
            let ahead = c[0] * n[0] + c[1] * n[1];
            let mut vd = if ahead > 6.0 || ahead < -0.5 {
                // Far off, or past its plane on the wrong side: make for a
                // point on its axis in front of it.
                let lead = if ahead < 0.0 {
                    3.0
                } else {
                    (ahead * 0.5).min(4.0)
                };
                let aim = [c[0] - n[0] * lead, c[1] - n[1] * lead, c[2]];
                let length = norm(&aim).max(1e-3);
                let speed = if ahead < 0.0 {
                    0.5 * self.cruise
                } else {
                    self.cruise
                };
                aim.map(|a| speed * a / length.max(1.0))
            } else {
                // The funnel, on the gate as it is now: close the miss across
                // the axis, moving with the gate as it sways or bobs, and
                // press on along it only as fast as the miss allows.
                let ahead = now[0] * n[0] + now[1] * n[1];
                let off = [now[0] - ahead * n[0], now[1] - ahead * n[1], now[2]];
                let miss = norm(&off);
                let gain = (6.0 / (ahead + 1.0)).clamp(1.5, 4.0);
                let aligned = miss < 0.25 + 0.15 * ahead.max(0.0);
                // Too close to its plane while still off line: back off,
                // rather than slide sideways into the frame.
                let forward = if aligned {
                    self.cruise
                } else if ahead < 1.2 {
                    -0.4 * self.cruise
                } else {
                    0.35 * self.cruise
                };
                let along = gv[0] * n[0] + gv[1] * n[1];
                // Through a gate that's moving fast across its axis, go
                // slower, to keep up with it.
                let across = norm(&[gv[0] - n[0] * along, gv[1] - n[1] * along, gv[2]]);
                let forward = forward * (1.0 - across / 4.0).clamp(0.5, 1.0);
                [
                    n[0] * forward + gain * off[0] + gv[0] - n[0] * along,
                    n[1] * forward + gain * off[1] + gv[1] - n[1] * along,
                    gain * off[2] + gv[2],
                ]
            };
            // Off the ground first: never dive below a metre.
            if height < 1.0 {
                vd[2] = vd[2].max(1.5);
            }
            vd
        };
        // Steer clear of obstacles, where they'll be in half a second.
        let obstacles = read(
            declaration,
            input,
            IN_OBSTACLES,
            "obstacle_1",
            OBSTACLES * 7,
        );
        for o in obstacles.chunks(7) {
            let radius = o[6];
            if radius <= 0.0 {
                continue;
            }
            let p = [o[0] + o[3] * 0.5, o[1] + o[4] * 0.5, o[2] + o[5] * 0.5];
            let d = norm(&p);
            let clear = d - radius;
            if d > 0.01 && clear < 2.5 {
                let push = 4.0 * (2.5 - clear.max(0.0)) / d;
                for axis in 0..3 {
                    vd[axis] -= push * p[axis];
                }
            }
        }
        // Rivals: keep a couple of metres away, and near a gate give way to
        // one that's closer to it, rather than jostle for the opening.
        let rivals = read(declaration, input, IN_RIVALS, "rival_1", RIVALS * 7);
        let gate = if finished {
            None
        } else {
            Some(r(IN_COURSE, "next", 3))
        };
        let mut cap = f32::INFINITY;
        for rival in rivals.chunks(7) {
            if rival[6] < 0.5 {
                continue;
            }
            let p = [rival[0], rival[1], rival[2]];
            let d = norm(&p);
            if d > 0.01 && d < 2.0 {
                let push = 3.0 * (2.0 - d) / d;
                for axis in 0..3 {
                    vd[axis] -= push * p[axis];
                }
            }
            if let Some(c) = &gate {
                let theirs = norm(&[c[0] - p[0], c[1] - p[1], c[2] - p[2]]);
                if d < 4.0 && norm(c) < YIELD_NEAR && theirs + 0.5 < norm(c) {
                    cap = cap.min(0.35 * self.cruise);
                }
            }
        }
        let level = (vd[0] * vd[0] + vd[1] * vd[1]).sqrt();
        if level > cap {
            vd[0] *= cap / level;
            vd[1] *= cap / level;
        }

        // Acceleration wanted, then the tilt and thrust that make it.
        let gravity = 9.81;
        let mut a = [
            2.5 * (vd[0] - v[0]),
            2.5 * (vd[1] - v[1]),
            2.5 * (vd[2] - v[2]) + gravity,
        ];
        let tilt_max = 40.0_f32.to_radians().tan();
        let level = (a[0] * a[0] + a[1] * a[1]).sqrt();
        if level > a[2] * tilt_max {
            let scale = a[2] * tilt_max / level;
            a[0] *= scale;
            a[1] *= scale;
        }
        a[2] = a[2].max(2.0);
        let length = norm(&a);
        let zd = a.map(|x| x / length);

        // The body's pitch and roll from the gravity reading.
        let pitch = g[0].clamp(-1.0, 1.0).asin();
        let roll = (-g[1]).atan2(-g[2]);
        let (sp, cp, sr, cr) = (pitch.sin(), pitch.cos(), roll.sin(), roll.cos());
        // Body axes in the heading frame: R = Ry(pitch) Rx(roll).
        let rb = [
            [cp, sp * sr, sp * cr],
            [0.0, cr, -sr],
            [-sp, cp * sr, cp * cr],
        ];
        // The wanted up axis in the body frame.
        let zb = [
            rb[0][0] * zd[0] + rb[1][0] * zd[1] + rb[2][0] * zd[2],
            rb[0][1] * zd[0] + rb[1][1] * zd[1] + rb[2][1] * zd[2],
        ];
        let e = [-zb[1], zb[0]];
        let yaw_error = if finished {
            0.0
        } else {
            let c = r(IN_COURSE, "next", 3);
            if (c[0] * c[0] + c[1] * c[1]).sqrt() > 1.0 {
                c[1].atan2(c[0])
            } else {
                0.0
            }
        };
        let torque = [
            4.0 * e[0] - 0.4 * w[0],
            3.0 * e[1] - 0.3 * w[1],
            0.5 * yaw_error - 0.15 * w[2],
        ];
        let up = [rb[0][2], rb[1][2], rb[2][2]];
        let thrust = (DRONE_MASS * (a[0] * up[0] + a[1] * up[1] + a[2] * up[2])).clamp(0.0, 48.0);
        Some(vec![Values::F32(mix(thrust, torque).to_vec())])
    }
}

/// Full thrust on one side: flips over at once.
pub struct Flipper;

impl Policy for Flipper {
    fn act(&mut self, _: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
        Some(vec![Values::F32(vec![THRUST_MAX, 0.0, 0.0, THRUST_MAX])])
    }
}

/// Hover thrust on every rotor: lifts off, then drifts.
pub fn hover(_: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
    Some(vec![Values::F32(vec![HOVER_THRUST; 4])])
}
