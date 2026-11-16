//! The policy shell: one program for every trained policy in every arena.
//!
//! It runs the bundle's first model by the shell's ONNX contract, version 1
//! ([`botstadia_interface::policy`]):
//!
//! - one input tensor per declared observation the model takes, by name,
//!   shaped `[1, *shape]`; `u8` values fed as `f32 / 255`
//! - the optional `state_in` / `state_out` pair, zeros at `init`, carried
//!   turn to turn
//! - continuous (`f32`) actions clamped to `[-1, 1]` and mapped affinely onto
//!   their declared bounds, NaN to the midpoint; discrete (`u8`) actions the
//!   argmax of their logits
//!
//! It reads the arena's declaration and the model's shapes from the host, so
//! nothing about any arena is built in. Anything it can't do (no model, a
//! model that doesn't fit the contract, an inference error) is no action
//! that turn, never a trap. Under `BOTSTADIA_BOT_LOGS=1`, which only the
//! local tools set, it prints what it feeds the model and the action it
//! sends, by name.

use std::cell::RefCell;

use botstadia_interface::bindings::agent::botstadia::agent::host;
use botstadia_interface::bindings::agent::{Guest, export_agent};
use botstadia_interface::declaration::{Declaration, Dtype, ValueDecl};
use botstadia_interface::payload::{self, Kind, Values};
use botstadia_interface::policy::{self as contract, STATE_IN, STATE_OUT};

/// What `init` worked out once: the plan for every turn.
struct Plan {
    declaration: Declaration,
    /// For each model input, in the model's order: which observation (or the
    /// state) and its shape.
    inputs: Vec<(Feed, Vec<u32>)>,
    /// For each declared action, its output's index in the model's outputs.
    actions: Vec<usize>,
    /// The state's size, when the model has memory, and `state_out`'s index.
    state: Option<(usize, usize)>,
    logs: bool,
}

#[derive(Clone, Copy)]
enum Feed {
    Observation(usize),
    State,
}

#[derive(Default)]
struct Shell {
    plan: Option<Plan>,
    state: Vec<f32>,
    turn: u32,
}

thread_local! {
    static SHELL: RefCell<Shell> = RefCell::new(Shell::default());
}

fn logs_on() -> bool {
    std::env::var("BOTSTADIA_BOT_LOGS").is_ok_and(|value| value == "1")
}

fn plan() -> Result<Plan, String> {
    let declaration = Declaration::from_json(&host::declaration())
        .map_err(|error| format!("reading the arena's declaration: {error}"))?;
    let models = host::models();
    let model = models
        .first()
        .ok_or("the bundle has no model: a trained policy needs artifacts/policy.onnx")?;
    let mut inputs = Vec::new();
    let mut state_size = None;
    for input in &model.inputs {
        if input.name == STATE_IN {
            let size = input.shape.iter().product::<u32>() as usize;
            state_size = Some(size);
            inputs.push((Feed::State, input.shape.clone()));
            continue;
        }
        let index = declaration
            .inputs
            .iter()
            .position(|value| value.name == input.name)
            .ok_or_else(|| {
                format!(
                    "the model takes `{}`, which the arena doesn't declare",
                    input.name
                )
            })?;
        let want: Vec<u32> = std::iter::once(1)
            .chain(declaration.inputs[index].shape.iter().copied())
            .collect();
        if input.shape != want {
            return Err(format!(
                "the model takes `{}` as {:?}, not {want:?}",
                input.name, input.shape
            ));
        }
        inputs.push((Feed::Observation(index), input.shape.clone()));
    }
    let mut actions = Vec::new();
    for value in &declaration.outputs {
        let index = model
            .outputs
            .iter()
            .position(|output| output.name == value.name)
            .ok_or_else(|| format!("the model gives no `{}`", value.name))?;
        actions.push(index);
    }
    let state = match state_size {
        Some(size) => {
            let out = model
                .outputs
                .iter()
                .position(|output| output.name == STATE_OUT)
                .ok_or("the model takes state_in but gives no state_out")?;
            Some((size, out))
        }
        None => None,
    };
    Ok(Plan {
        declaration,
        inputs,
        actions,
        state,
        logs: logs_on(),
    })
}

/// One declared action from the model's output.
fn action(value: &ValueDecl, data: &[f32]) -> Option<Values> {
    let count = value.elements();
    match value.dtype {
        Dtype::F32 => {
            if data.len() != count {
                return None;
            }
            Some(Values::F32(
                data.iter()
                    .enumerate()
                    .map(|(index, out)| {
                        let (min, max) = value.bounds.range(index).unwrap_or((-1.0, 1.0));
                        contract::to_bounds(*out, min, max)
                    })
                    .collect(),
            ))
        }
        Dtype::U8 => {
            let (min, max) = value.bounds.range(0)?;
            let choices = (max - min) as usize + 1;
            if data.len() != count * choices {
                return None;
            }
            Some(Values::U8(
                data.chunks(choices)
                    .map(|logits| (min as usize + contract::argmax(logits)) as u8)
                    .collect(),
            ))
        }
    }
}

fn describe(value: &ValueDecl, values: &Values) -> String {
    let get = |index| values.get(index).unwrap_or(0.0);
    if value.slices.is_empty() {
        return format!("{}={}", value.name, list((0..values.len()).map(get)));
    }
    let mut at = 0;
    let mut parts = Vec::new();
    for slice in &value.slices {
        let count = slice.count as usize;
        parts.push(format!(
            "{}={}",
            slice.name,
            list((at..at + count).map(get))
        ));
        at += count;
    }
    parts.join(" ")
}

fn list(values: impl Iterator<Item = f32>) -> String {
    let all: Vec<String> = values.map(|v| format!("{v:.4}")).collect();
    if all.len() == 1 {
        all[0].clone()
    } else if all.len() > 8 {
        format!("[{} … {} values]", all[..6].join(" "), all.len())
    } else {
        format!("[{}]", all.join(" "))
    }
}

fn tick(shell: &mut Shell, input: &[u8]) -> Result<Vec<u8>, String> {
    let plan = shell.plan.as_ref().ok_or("the shell has no plan")?;
    let declaration = &plan.declaration;
    let schema = declaration.payload_schema_version;
    let observations = payload::decode(Kind::Input, schema, &declaration.inputs, input)
        .map_err(|error| format!("reading the input: {error}"))?;
    let mut tensors = Vec::with_capacity(plan.inputs.len());
    for (feed, shape) in &plan.inputs {
        let (name, data) = match feed {
            Feed::State => (STATE_IN.to_string(), shell.state.clone()),
            Feed::Observation(index) => {
                let data = match &observations[*index] {
                    Values::F32(values) => values.clone(),
                    Values::U8(values) => values.iter().map(|v| contract::feed_u8(*v)).collect(),
                };
                if plan.logs {
                    eprintln!(
                        "[t{}] {}",
                        shell.turn,
                        describe(&declaration.inputs[*index], &observations[*index])
                    );
                }
                (declaration.inputs[*index].name.clone(), data)
            }
        };
        tensors.push(host::Tensor {
            name,
            shape: shape.clone(),
            data,
        });
    }
    let outputs = host::infer(0, &tensors).map_err(|error| format!("inference: {error:?}"))?;
    let mut values = Vec::with_capacity(plan.actions.len());
    for (value, index) in declaration.outputs.iter().zip(&plan.actions) {
        let out = outputs
            .get(*index)
            .ok_or("the model gave too few outputs")?;
        values.push(
            action(value, &out.data)
                .ok_or_else(|| format!("`{}` has the wrong size", value.name))?,
        );
    }
    if let Some((size, out)) = plan.state {
        let next = &outputs.get(out).ok_or("no state_out")?.data;
        if next.len() == size && next.iter().all(|v| v.is_finite()) {
            shell.state.clone_from(next);
        } else {
            shell.state = vec![0.0; size];
        }
    }
    if plan.logs {
        let sent: Vec<String> = declaration
            .outputs
            .iter()
            .zip(&values)
            .map(|(value, values)| describe(value, values))
            .collect();
        eprintln!("[t{}] → {}", shell.turn, sent.join(" "));
    }
    payload::encode(Kind::Output, schema, &declaration.outputs, &values)
        .map_err(|error| error.to_string())
}

struct PolicyShell;

impl Guest for PolicyShell {
    fn init() {
        SHELL.with(|shell| {
            let mut shell = shell.borrow_mut();
            *shell = Shell::default();
            match plan() {
                Ok(plan) => {
                    if plan.logs {
                        let fed: Vec<String> = plan
                            .inputs
                            .iter()
                            .map(|(feed, shape)| match feed {
                                Feed::State => format!("{STATE_IN}{shape:?}"),
                                Feed::Observation(index) => {
                                    format!("{}{shape:?}", plan.declaration.inputs[*index].name)
                                }
                            })
                            .collect();
                        eprintln!(
                            "policy shell: contract {} · feeding {} · sending {}",
                            contract::CONTRACT_VERSION,
                            fed.join(", "),
                            plan.declaration
                                .outputs
                                .iter()
                                .map(|v| v.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    shell.state = vec![0.0; plan.state.map_or(0, |(size, _)| size)];
                    shell.plan = Some(plan);
                }
                Err(problem) => {
                    if logs_on() {
                        eprintln!("policy shell: no action all session: {problem}");
                    }
                }
            }
        });
    }

    fn on_tick(input: Vec<u8>) -> Vec<u8> {
        SHELL.with(|shell| {
            let mut shell = shell.borrow_mut();
            shell.turn += 1;
            if shell.plan.is_none() {
                return Vec::new();
            }
            match tick(&mut shell, &input) {
                Ok(output) => output,
                Err(problem) => {
                    if shell.plan.as_ref().is_some_and(|plan| plan.logs) {
                        eprintln!("[t{}] no action: {problem}", shell.turn);
                    }
                    Vec::new()
                }
            }
        })
    }
}

export_agent!(PolicyShell with_types_in botstadia_interface::bindings::agent);
