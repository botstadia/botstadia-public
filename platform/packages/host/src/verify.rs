//! Upload verification's checks (BOTS.md#bundles): the manifest, the ONNX
//! preflight, a compile, instantiation against the `agent` world, and a
//! timing run against the arena's no-action in every other seat.
//!
//! One function, run by `botstadia check` on a builder's machine and by the
//! enclave's verification job, so the two agree on everything but wall
//! times (BEFORE-PROD.md: "`botstadia check` and upload verification
//! agreeing on test bundles"). Fuel and inference charges are counted
//! exactly the same on any machine.

use std::sync::Arc;

use botstadia_interface::bundle::BotKind;
use botstadia_interface::declaration::Dtype;
use botstadia_interface::payload::{self, Kind};
use serde::{Deserialize, Serialize};

use crate::budget::{self, BudgetVersion};
use crate::onnx::runtime::{LoadedModel, NamedTensor};
use crate::seat::Program;
use crate::{Bundle, LoadedArena, Seat, Session, SessionOptions, seed};

/// Turns the timing run plays.
pub const TIMING_TURNS: u32 = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelReport {
    pub name: String,
    pub charge: u64,
    pub share_of_turn: f64,
    pub macs: u64,
    pub elements: u64,
    pub memory_bytes: u64,
    pub opset: i64,
}

/// What verification found. An enclave signs this as the upload report;
/// a failure's reason goes to the builder only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub digest: String,
    pub arena_digest: String,
    pub budget_version: u32,
    pub kind: Option<String>,
    pub passed: bool,
    pub problems: Vec<String>,
    pub models: Vec<ModelReport>,
    /// A trained policy's `component.wasm` digest (the policy shell's).
    pub shell: Option<String>,
    pub turn_fuel: u64,
    pub init_fuel_used: u64,
    pub fuel_p50: u64,
    pub fuel_p99: u64,
    pub fuel_max: u64,
    pub out_of_fuel_turns: u32,
    pub no_action_turns: u32,
    pub timing_turns: u32,
    /// Wall times: this machine's. Only an enclave's decide.
    pub wall_ms_p50: f64,
    pub wall_ms_max: f64,
    /// The fuel and wall-time histograms the upload report publishes:
    /// (fuel, wall ms) per timing turn.
    pub per_turn: Vec<(u64, f64)>,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Verify a bundle against an arena under a budget version.
///
/// `seed` drives the timing run's session. Draw it fresh for every run (the
/// enclave from its own randomness): with a known constant, a bot could
/// recognise verification by its random stream and behave only there.
pub fn verify(
    bundle: Bundle,
    arena: &LoadedArena,
    budget: &'static BudgetVersion,
    seed: &seed::Seed,
) -> Report {
    let declaration = arena.declaration.clone();
    let mut report = Report {
        digest: bundle.digest.clone(),
        arena_digest: arena.digest.clone(),
        budget_version: budget.id,
        turn_fuel: declaration.budgets.turn_fuel,
        kind: bundle.kind().map(|k| BotKind::name(k).to_string()),
        ..Report::default()
    };
    match &bundle.manifest {
        None => report
            .problems
            .push("a bare component isn't a bundle: run `botstadia build`".into()),
        Some(manifest) => {
            if manifest.arena != declaration.arena_id {
                report.problems.push(format!(
                    "the bundle is for `{}`, not `{}`",
                    manifest.arena, declaration.arena_id
                ));
            }
            if manifest.payload_schema_version != declaration.payload_schema_version {
                report.problems.push(format!(
                    "the bundle is for payload schema {}, the arena is on {}: refresh and rebuild",
                    manifest.payload_schema_version, declaration.payload_schema_version
                ));
            }
        }
    }
    if bundle.kind() == Some(BotKind::Policy) {
        report.shell = Some(crate::digest(&bundle.component));
    }
    let kind = bundle.kind();
    let manifest = bundle.manifest.clone();
    let models = bundle.models.clone();
    let program = match Program::new(bundle, budget, declaration.budgets.turn_fuel) {
        Ok(program) => Arc::new(program),
        Err(error) => {
            report.problems.push(format!("{error:#}"));
            return finish(report);
        }
    };
    for (info, (name, bytes)) in program.models.iter().zip(&models) {
        report.models.push(ModelReport {
            name: name.clone(),
            charge: info.charge,
            share_of_turn: info.charge as f64 / declaration.budgets.turn_fuel as f64,
            macs: info.macs,
            elements: info.elements,
            memory_bytes: info.memory_bytes,
            opset: info.opset,
        });
        if let (Some(BotKind::Policy), Some(manifest)) = (kind, &manifest) {
            report.problems.extend(crate::onnx::policy::problems(
                info,
                &declaration,
                manifest,
                budget,
            ));
            report
                .problems
                .extend(finite_outputs(name, bytes, info, arena, seed));
        }
    }
    if !report.problems.is_empty() {
        return finish(report);
    }

    let count = declaration.entrants.min().max(1);
    let mut seats = vec![Seat::Bot(program)];
    for index in 1..count {
        seats.push(Seat::External {
            name: format!("no-action {index}"),
        });
    }
    let options = SessionOptions {
        budget: true,
        record: false,
        bot_logs: false,
    };
    let (mut session, first) = match Session::start(arena, seats, seed, budget, options) {
        Ok(s) => s,
        Err(error) => {
            report
                .problems
                .push(format!("the session wouldn't start: {error:#}"));
            return finish(report);
        }
    };
    report.init_fuel_used = first.seats[0].wasm_fuel + first.seats[0].inference_fuel;
    if let Some(trap) = &first.seats[0].trapped {
        report.problems.push(format!("it trapped in init: {trap}"));
        return finish(report);
    }
    let mut fuel = Vec::new();
    let mut walls = Vec::new();
    while session.finished().is_none() && session.turn() < TIMING_TURNS {
        let step = match session.step(&[]) {
            Ok(step) => step,
            Err(error) => {
                report.problems.push(format!("the arena failed: {error:#}"));
                break;
            }
        };
        let seat = &step.seats[0];
        let f = seat.wasm_fuel + seat.inference_fuel;
        let w = seat.wall.as_secs_f64() * 1000.0;
        report.per_turn.push((f, w));
        fuel.push(f);
        walls.push(w);
        report.out_of_fuel_turns += u32::from(seat.paused);
        report.no_action_turns += u32::from(!seat.acted);
        if let Some(trap) = &seat.trapped {
            report
                .problems
                .push(format!("it trapped on turn {}: {trap}", step.turn));
            break;
        }
    }
    report.timing_turns = fuel.len() as u32;
    fuel.sort_unstable();
    walls.sort_by(f64::total_cmp);
    report.fuel_p50 = percentile(&fuel, 0.5);
    report.fuel_p99 = percentile(&fuel, 0.99);
    report.fuel_max = fuel.last().copied().unwrap_or(0);
    report.wall_ms_p50 = walls.get(walls.len() / 2).copied().unwrap_or(0.0);
    report.wall_ms_max = walls.last().copied().unwrap_or(0.0);
    finish(report)
}

fn finish(mut report: Report) -> Report {
    report.passed = report.problems.is_empty();
    report
}

/// A trained policy must give finite outputs on zeros and on a real input.
fn finite_outputs(
    name: &str,
    bytes: &[u8],
    info: &crate::onnx::ModelInfo,
    arena: &LoadedArena,
    seed: &seed::Seed,
) -> Vec<String> {
    let mut problems = Vec::new();
    let Ok(mut model) = LoadedModel::load(name, bytes, info.clone()) else {
        return vec![format!("model `{name}` won't load in ONNX Runtime")];
    };
    let zeros: Vec<NamedTensor> = info
        .inputs
        .iter()
        .map(|input| NamedTensor {
            name: input.name.clone(),
            shape: input.shape.iter().map(|d| *d as u32).collect(),
            data: vec![0.0; input.elements() as usize],
        })
        .collect();
    // A real input: turn 0 of a session, its values as the shell feeds them.
    let real = (|| -> Option<Vec<NamedTensor>> {
        let options = SessionOptions {
            budget: false,
            record: false,
            bot_logs: false,
        };
        let count = arena.declaration.entrants.min().max(1);
        let seats = (0..count)
            .map(|i| Seat::External {
                name: format!("{i}"),
            })
            .collect();
        let (_, first) = Session::start(arena, seats, seed, budget::current(), options).ok()?;
        let declaration = &arena.declaration;
        let values = payload::decode(
            Kind::Input,
            declaration.payload_schema_version,
            &declaration.inputs,
            &first.inputs[0],
        )
        .ok()?;
        Some(
            zeros
                .iter()
                .map(|zero| {
                    let data = declaration
                        .inputs
                        .iter()
                        .position(|value| value.name == zero.name)
                        .map(
                            |index| match (&declaration.inputs[index].dtype, &values[index]) {
                                (Dtype::U8, values) => (0..values.len())
                                    .map(|i| values.get(i).unwrap_or(0.0) / 255.0)
                                    .collect(),
                                (_, values) => (0..values.len())
                                    .map(|i| values.get(i).unwrap_or(0.0))
                                    .collect(),
                            },
                        )
                        .unwrap_or_else(|| zero.data.clone());
                    NamedTensor {
                        data,
                        ..zero.clone()
                    }
                })
                .collect(),
        )
    })();
    for (what, inputs) in [("zeros", Some(zeros)), ("a real input", real)] {
        let Some(inputs) = inputs else { continue };
        match model.run(&inputs) {
            Ok(outputs) => {
                if outputs
                    .iter()
                    .any(|t| t.data.iter().any(|v| !v.is_finite()))
                {
                    problems.push(format!(
                        "model `{name}` gives a non-finite output on {what}"
                    ));
                }
            }
            Err(error) => problems.push(format!("model `{name}` fails on {what}: {error}")),
        }
    }
    problems
}
