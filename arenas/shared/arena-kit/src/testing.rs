//! Playing sessions natively, and the checks every arena's engine tests make.
//!
//! An arena's `engine` crate runs these in its own tests:
//!
//! - the declaration is complete ([`declaration_problems`])
//! - action bounds equal the model's `ctrlrange` ([`bounds_match_ctrlrange`])
//! - every declared series and event is emitted, and nothing undeclared
//!   ([`Coverage`])
//! - the scripted baseline beats random on most seeds ([`win_rate`])
//! - outcomes spread ([`Spread`])
//!
//! Test bots live in the house bots' repository and never link an engine; the policies here
//! are the built-in no-op, random and extreme seats the tools also use.

use botstadia_interface::declaration::Declaration;
use botstadia_interface::payload::{self, Values};
use mujoco::MjSim;

use crate::seed::{Rng, Seed};
use crate::session::{FinishOut, Session};
use crate::world::World;

/// A seat's behaviour in a native session.
pub trait Policy {
    /// This turn's output, or `None` for no action.
    fn act(
        &mut self,
        declaration: &Declaration,
        seat: u32,
        input: &[Values],
    ) -> Option<Vec<Values>>;
}

/// Always the no-action.
pub struct NoOp;

impl Policy for NoOp {
    fn act(&mut self, _: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
        None
    }
}

/// Uniform inside the declared bounds.
pub struct Random(pub Rng);

impl Random {
    pub fn new(seed: u64) -> Self {
        Self(Seed::from_u64(seed).stream("random-policy"))
    }
}

impl Policy for Random {
    fn act(&mut self, declaration: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
        Some(payload::sample_uniform(&declaration.outputs, || {
            self.0.next_u64()
        }))
    }
}

/// Every element at a bound, flipping at random: the fuzzer's bang-bang.
pub struct Extremes(pub Rng);

impl Policy for Extremes {
    fn act(&mut self, declaration: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
        Some(payload::extremes(&declaration.outputs, || {
            self.0.chance(0.5)
        }))
    }
}

impl<F: FnMut(&Declaration, u32, &[Values]) -> Option<Vec<Values>>> Policy for F {
    fn act(
        &mut self,
        declaration: &Declaration,
        seat: u32,
        input: &[Values],
    ) -> Option<Vec<Values>> {
        self(declaration, seat, input)
    }
}

/// One finished native session.
#[derive(Debug, Clone)]
pub struct Played {
    pub finish: FinishOut,
    pub turns: u32,
    /// Declared series indices that changed from their turn-0 value.
    pub series_moved: Vec<bool>,
    /// Declared event indices that happened.
    pub events_seen: Vec<bool>,
    pub problems: Vec<String>,
}

/// Play one session to its end.
pub fn play<W: World>(seed: u64, policies: &mut [Box<dyn Policy + '_>]) -> Result<Played, String> {
    let seats = policies.len() as u32;
    let (mut session, first) = Session::<W>::setup(&seed.to_le_bytes(), seats)?;
    let declaration = session.declaration().clone();
    let schema = declaration.payload_schema_version;
    let mut inputs = first.inputs;
    let start = first.series.clone();
    let mut series_moved = vec![false; declaration.series.len()];
    let mut events_seen = vec![false; declaration.events.len()];
    loop {
        if let Some(finish) = session.finished() {
            return Ok(Played {
                finish,
                turns: session.turn(),
                series_moved,
                events_seen,
                problems: session.problems(),
            });
        }
        if session.turn() > declaration.max_turns {
            return Err("the session ran past maximum turns".into());
        }
        let outputs: Vec<Option<Vec<Values>>> = policies
            .iter_mut()
            .enumerate()
            .map(|(seat, policy)| {
                let input = payload::decode(
                    payload::Kind::Input,
                    schema,
                    &declaration.inputs,
                    &inputs[seat],
                )
                .ok()?;
                policy.act(&declaration, seat as u32, &input)
            })
            .collect();
        let encoded: Vec<Option<Vec<u8>>> = outputs
            .iter()
            .map(|output| {
                output.as_ref().and_then(|values| {
                    payload::encode(payload::Kind::Output, schema, &declaration.outputs, values)
                        .ok()
                })
            })
            .collect();
        let turn = session.step(&encoded);
        for (index, (now, then)) in turn.series.iter().zip(&start).enumerate() {
            series_moved[index] |= now != then;
        }
        for (index, _) in &turn.events {
            events_seen[*index as usize] = true;
        }
        inputs = turn.inputs;
    }
}

/// Everything wrong with a world's declaration.
pub fn declaration_problems<W: World>() -> Vec<String> {
    W::declaration()
        .problems()
        .into_iter()
        .map(|problem| problem.0)
        .collect()
}

/// Mismatches between output `value`'s bounds and the `ctrlrange` of the
/// actuators driving its elements, in element order.
pub fn bounds_match_ctrlrange(
    declaration: &Declaration,
    value: &str,
    sim: &MjSim,
    actuators: &[&str],
) -> Vec<String> {
    let Some(decl) = declaration
        .outputs
        .iter()
        .find(|output| output.name == value)
    else {
        return vec![format!("no output named `{value}`")];
    };
    if decl.elements() != actuators.len() {
        return vec![format!(
            "`{value}` has {} elements for {} actuators",
            decl.elements(),
            actuators.len()
        )];
    }
    let mut problems = Vec::new();
    for (index, name) in actuators.iter().enumerate() {
        let Some(actuator) = sim.actuator_id(name) else {
            problems.push(format!("no actuator `{name}` in the model"));
            continue;
        };
        let declared = decl
            .bounds
            .range(index)
            .map(|(min, max)| (f64::from(min), f64::from(max)));
        let model = sim.actuator_ctrlrange(actuator);
        let close =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5;
        match (declared, model) {
            (Some(declared), Some(model)) if close(declared, model) => {}
            _ => problems.push(format!(
                "`{value}`[{index}] is {declared:?} but `{name}`'s ctrlrange is {model:?}"
            )),
        }
    }
    problems
}

/// Which declared series and events a set of sessions exercised.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    pub series_moved: Vec<bool>,
    pub events_seen: Vec<bool>,
    pub problems: Vec<String>,
}

impl Coverage {
    pub fn add(&mut self, played: &Played) {
        merge(&mut self.series_moved, &played.series_moved);
        merge(&mut self.events_seen, &played.events_seen);
        for problem in &played.problems {
            if !self.problems.contains(problem) {
                self.problems.push(problem.clone());
            }
        }
    }

    /// Declared series that never moved and events that never happened.
    pub fn missing(&self, declaration: &Declaration) -> Vec<String> {
        let series = declaration
            .series
            .iter()
            .zip(&self.series_moved)
            .filter(|(_, moved)| !**moved)
            .map(|(decl, _)| format!("series `{}`", decl.key));
        let events = declaration
            .events
            .iter()
            .zip(&self.events_seen)
            .filter(|(_, seen)| !**seen)
            .map(|(decl, _)| format!("event `{}`", decl.key));
        series.chain(events).collect()
    }
}

fn merge(into: &mut Vec<bool>, from: &[bool]) {
    if into.len() < from.len() {
        into.resize(from.len(), false);
    }
    for (slot, value) in into.iter_mut().zip(from) {
        *slot |= *value;
    }
}

/// The share of `seeds` sessions seat 0 wins outright, playing `make(seed)`'s
/// policies.
pub fn win_rate<W: World>(
    seeds: std::ops::Range<u64>,
    mut make: impl FnMut(u64) -> Vec<Box<dyn Policy>>,
) -> Result<f64, String> {
    let count = seeds.end.saturating_sub(seeds.start).max(1);
    let mut wins = 0;
    for seed in seeds {
        let mut policies = make(seed);
        let played = play::<W>(seed, &mut policies)?;
        if played
            .finish
            .places
            .first()
            .is_some_and(|first| first == &[0])
        {
            wins += 1;
        }
    }
    Ok(wins as f64 / count as f64)
}

/// How varied a set of sessions' outcomes were.
#[derive(Debug, Clone, Default)]
pub struct Spread {
    pub winners: std::collections::BTreeMap<String, u32>,
    pub decided_by: std::collections::BTreeMap<String, u32>,
}

impl Spread {
    pub fn add(&mut self, played: &Played) {
        let winner = format!(
            "{:?}",
            played.finish.places.first().cloned().unwrap_or_default()
        );
        *self.winners.entry(winner).or_default() += 1;
        *self
            .decided_by
            .entry(played.finish.decided_by.clone())
            .or_default() += 1;
    }
}
