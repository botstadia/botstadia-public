// A tiny two-seat world: each seat swings its own pendulum, and the higher
// peak wins. Shared by the kit's native tests (tests/swing.rs) and its WASM
// component example (examples/swing.rs).

use arena_kit::declaration::*;
use arena_kit::mujoco::MjSim;
use arena_kit::sim::{ContactTracker, frame, step_turn};
use arena_kit::{EventKey, Finish, Report, Seed, SeriesKey, StatKey, Values, World};

const MODEL: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../mujoco/tests/fixtures/pendulum.mjb"));
const ANGLE: SeriesKey = SeriesKey("angle");
const PEAK: EventKey = EventKey("peak");
const BEST: StatKey = StatKey("best");

struct Swing {
    sims: Vec<MjSim>,
    best: Vec<f64>,
    peaked: Vec<bool>,
    turn: u32,
    contacts: ContactTracker,
}

impl World for Swing {
    fn declaration() -> Declaration {
        Declaration {
            arena_id: "swing".into(),
            arena_version: "0.1.0".into(),
            changelog: vec![ChangelogEntry { version: "0.1.0".into(), changes: "First.".into() }],
            payload_schema_version: 1,
            entrants: Entrants::Exact(2),
            inputs: vec![ValueDecl::f32_slices("arm", "Your arm.", -100.0, 100.0, vec![
                slice("angle", 1, "rad", "The arm's angle."),
                slice("speed", 1, "rad/s", "How fast it turns."),
            ])],
            outputs: vec![ValueDecl::new("torque", Dtype::F32, &[1], Bounds::Uniform { min: -1.0, max: 1.0 }, "Motor command.")],
            no_action: "no torque".into(),
            control_rate: 50,
            budgets: Budgets { turn_fuel: 1_000_000, init_fuel: 16_000_000 },
            max_turns: 150,
            wire: WireSizes { max_input_bytes: 64, max_output_bytes: 32, max_frame_bytes: 1024 },
            stats: vec![StatDecl {
                key: "best".into(),
                label: "Best swing".into(),
                unit: "rad".into(),
                kind: StatKind::Measure,
                form: Aggregate::Max,
                better: Better::Higher,
                doc: "The highest angle reached.".into(),
            }],
            series: vec![SeriesDecl { key: "angle".into(), label: "Angle".into(), unit: "rad".into(), scope: SeriesScope::PerSeat, doc: "Each arm's angle.".into() }],
            events: vec![EventDecl { key: "peak".into(), label: "Over the top".into(), doc: "An arm passed 1 rad.".into() }],
            shaping_reward: "The arm's height.".into(),
            min_trainer_version: "0.1.0".into(),
        }
    }

    fn setup(seed: &Seed, seats: u32, report: &mut Report) -> Result<Self, String> {
        let mut start = seed.stream("start");
        let mut sims = Vec::new();
        for seat in 0..seats {
            let mut sim = MjSim::load(MODEL)?;
            sim.reset(sim.keyframe_id("start"));
            sim.set_qpos(&[start.range(-0.05, 0.05)]);
            sim.forward();
            report.seat_series(ANGLE, seat, sim.qpos()[0] as f32);
            sims.push(sim);
        }
        Ok(Self { best: vec![0.0; seats as usize], peaked: vec![false; seats as usize], sims, turn: 0, contacts: ContactTracker::default() })
    }

    fn input(&self, seat: u32) -> Vec<Values> {
        let sim = &self.sims[seat as usize];
        vec![Values::F32(vec![sim.qpos()[0] as f32, sim.qvel()[0] as f32])]
    }

    fn step(&mut self, outputs: &[Option<Vec<Values>>], report: &mut Report) {
        self.turn += 1;
        for (seat, sim) in self.sims.iter_mut().enumerate() {
            let torque = outputs[seat].as_ref().and_then(|values| values[0].get(0)).unwrap_or(0.0);
            sim.set_ctrl(&[f64::from(torque)]);
            step_turn(sim, 4, &mut self.contacts);
            if sim.diverged() {
                sim.reset(sim.keyframe_id("start"));
            }
            let angle = sim.qpos()[0].abs();
            self.best[seat] = self.best[seat].max(angle);
            report.seat_series(ANGLE, seat as u32, sim.qpos()[0] as f32);
            report.shaping(seat as u32, angle as f32);
            if angle > 1.0 && !self.peaked[seat] {
                self.peaked[seat] = true;
                report.event(PEAK, Some(seat as u32));
            }
        }
    }

    fn frame(&self) -> Vec<u8> {
        frame(&self.sims[0], "pendulum", self.turn, Vec::new(), Vec::new(), Vec::new()).encode()
    }

    fn finish(&self, out_of_turns: bool) -> Option<Finish> {
        out_of_turns.then(|| Finish::by_score(&self.best, "Higher swing").stat(BEST, self.best.clone()))
    }
}

