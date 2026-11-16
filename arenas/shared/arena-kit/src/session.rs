//! One session of a world: the runtime behind the WASM export, and what
//! native tests drive.
//!
//! It speaks bytes on the outside, exactly as the `arena` world does, and
//! typed values to the world inside. It never traps on what the host sends:
//! an output that doesn't decode is the seat's no-action, and a world that
//! breaks its own contract (an undeclared key, no result at maximum turns) is
//! recorded as a problem the tests fail on, with a safe fallback.

use botstadia_interface::declaration::Declaration;
use botstadia_interface::payload::{self, Kind, Values};

use crate::report::{Finish, Report};
use crate::seed::Seed;
use crate::world::World;

/// What a turn produced, as the `arena` world's `turn` record.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOut {
    pub inputs: Vec<Vec<u8>>,
    pub frame: Vec<u8>,
    pub series: Vec<Vec<f32>>,
    /// `(declared event index, seat)`.
    pub events: Vec<(u32, Option<u32>)>,
    pub shaping: Vec<f32>,
}

/// How the session ended, as the `arena` world's `finish` record.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishOut {
    pub places: Vec<Vec<u32>>,
    pub decided_by: String,
    pub stats: Vec<Vec<f64>>,
}

pub struct Session<W: World> {
    world: W,
    declaration: Declaration,
    seats: u32,
    turn: u32,
    report: Report,
    problems: Vec<String>,
}

impl<W: World> Session<W> {
    pub fn setup(seed: &[u8], seats: u32) -> Result<(Self, TurnOut), String> {
        let declaration = W::declaration();
        if !declaration.entrants.accepts(seats) {
            return Err(format!(
                "{seats} seats, but the arena accepts {:?}",
                declaration.entrants
            ));
        }
        let mut report = Report::new(&declaration, seats);
        let world = W::setup(&Seed::new(seed), seats, &mut report)?;
        let mut session = Self {
            world,
            declaration,
            seats,
            turn: 0,
            report,
            problems: Vec::new(),
        };
        let out = session.turn_out();
        Ok((session, out))
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    pub fn world(&self) -> &W {
        &self.world
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }

    /// Everything the world did against its own declaration so far.
    pub fn problems(&self) -> Vec<String> {
        self.problems
            .iter()
            .chain(&self.report.problems)
            .cloned()
            .collect()
    }

    /// Advance one turn with each seat's output bytes (`None`: no action).
    pub fn step(&mut self, outputs: &[Option<Vec<u8>>]) -> TurnOut {
        let decoded: Vec<Option<Vec<Values>>> = (0..self.seats as usize)
            .map(|seat| {
                let bytes = outputs.get(seat)?.as_ref()?;
                payload::check(
                    Kind::Output,
                    self.declaration.payload_schema_version,
                    &self.declaration.outputs,
                    bytes,
                )
                .ok()
            })
            .collect();
        self.step_values(&decoded)
    }

    /// Advance one turn with already-typed outputs, for native tests.
    pub fn step_values(&mut self, outputs: &[Option<Vec<Values>>]) -> TurnOut {
        self.report.next_turn();
        self.turn += 1;
        self.world.step(outputs, &mut self.report);
        self.turn_out()
    }

    /// The result, once there is one. At maximum turns a world that gives
    /// none is a bug: it's recorded, and the session ends as a tie.
    pub fn finished(&mut self) -> Option<FinishOut> {
        let out_of_turns = self.turn >= self.declaration.max_turns;
        let finish = match self.world.finish(out_of_turns) {
            Some(finish) => finish,
            None if out_of_turns => {
                self.problem("no result at maximum turns".into());
                Finish::new(vec![(0..self.seats).collect()], "Time ran out")
            }
            None => return None,
        };
        let stats = match finish.ordered_stats(&self.declaration, self.seats) {
            Ok(stats) => stats,
            Err(problem) => {
                self.problem(problem);
                self.declaration
                    .stats
                    .iter()
                    .map(|_| vec![0.0; self.seats as usize])
                    .collect()
            }
        };
        let mut seen: Vec<u32> = finish.places.iter().flatten().copied().collect();
        seen.sort_unstable();
        let places = if seen == (0..self.seats).collect::<Vec<_>>()
            && finish.places.iter().all(|place| !place.is_empty())
        {
            finish.places
        } else {
            self.problem(format!(
                "places {:?} don't list every seat exactly once",
                finish.places
            ));
            vec![(0..self.seats).collect()]
        };
        Some(FinishOut {
            places,
            decided_by: finish.decided_by,
            stats,
        })
    }

    fn problem(&mut self, text: String) {
        if !self.problems.contains(&text) {
            self.problems.push(text);
        }
    }

    fn turn_out(&mut self) -> TurnOut {
        let schema = self.declaration.payload_schema_version;
        let mut inputs = Vec::with_capacity(self.seats as usize);
        for seat in 0..self.seats {
            let values = self.world.input(seat);
            match payload::encode(Kind::Input, schema, &self.declaration.inputs, &values) {
                Ok(bytes) => inputs.push(bytes),
                Err(error) => {
                    self.problem(format!(
                        "seat {seat}'s input doesn't match the declaration: {error}"
                    ));
                    inputs.push(Vec::new());
                }
            }
        }
        let frame = self.world.frame();
        if frame.len() > self.declaration.wire.max_frame_bytes as usize {
            self.problem(format!(
                "turn {}'s frame is {} bytes, over the declared {}",
                self.turn,
                frame.len(),
                self.declaration.wire.max_frame_bytes
            ));
        }
        TurnOut {
            inputs,
            frame,
            series: self.report.series.clone(),
            events: self.report.events.clone(),
            shaping: self.report.shaping.clone(),
        }
    }
}
