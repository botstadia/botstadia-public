//! The extra checks a trained policy's model passes: the policy shell's ONNX
//! contract ([`botstadia_interface::policy`]) against its arena's declaration
//! and its bundle's manifest. Finite outputs on zeros and on a real input
//! are checked by running it ([`crate::onnx::runtime`]).

use botstadia_interface::bundle::Manifest;
use botstadia_interface::declaration::{Declaration, Dtype};
use botstadia_interface::policy::{
    CONTRACT_VERSION, META_ARENA, META_CONTRACT, META_OPSET, META_PAYLOAD_SCHEMA,
    META_TRAIN_VERSION, NOISE, STATE_IN, STATE_OUT,
};

use super::preflight::{FLOAT, ModelInfo};
use crate::budget::BudgetVersion;

/// Everything wrong with `model` as a trained policy for `declaration`.
pub fn problems(
    model: &ModelInfo,
    declaration: &Declaration,
    manifest: &Manifest,
    budget: &BudgetVersion,
) -> Vec<String> {
    let mut problems = Vec::new();
    let meta = |key: &str| model.metadata.get(key).map(String::as_str);
    match meta(META_CONTRACT) {
        Some(version) if version == CONTRACT_VERSION.to_string() => {}
        Some(version) => problems.push(format!(
            "{META_CONTRACT} is {version}; this shell runs contract {CONTRACT_VERSION}"
        )),
        None => problems.push(format!(
            "no {META_CONTRACT} metadata: export with botstadia_train.export"
        )),
    }
    for (key, want) in [
        (META_ARENA, manifest.arena.clone()),
        (
            META_PAYLOAD_SCHEMA,
            manifest.payload_schema_version.to_string(),
        ),
    ] {
        match meta(key) {
            Some(value) if value == want => {}
            Some(value) => problems.push(format!("{key} is `{value}`, the manifest says `{want}`")),
            None => problems.push(format!("no {key} metadata")),
        }
    }
    for key in [META_TRAIN_VERSION, META_OPSET] {
        if meta(key).is_none() {
            problems.push(format!("no {key} metadata"));
        }
    }
    if manifest.arena != declaration.arena_id {
        problems.push(format!(
            "the manifest is for `{}`, the arena is `{}`",
            manifest.arena, declaration.arena_id
        ));
    }

    let mut state = None;
    for input in &model.inputs {
        if input.elem_type != FLOAT {
            problems.push(format!("input `{}` must be f32", input.name));
            continue;
        }
        if input.name == STATE_IN {
            match input.shape.as_slice() {
                [1, size] if *size > 0 && *size <= i64::from(budget.max_state) => {
                    state = Some(*size)
                }
                shape => problems.push(format!(
                    "`{STATE_IN}` is {shape:?}; it must be [1, S] with S at most {}",
                    budget.max_state
                )),
            }
            continue;
        }
        if input.name == NOISE {
            problems.push(format!("`{NOISE}` is reserved and not supported yet"));
            continue;
        }
        match declaration
            .inputs
            .iter()
            .find(|value| value.name == input.name)
        {
            Some(value) => {
                let want: Vec<i64> = std::iter::once(1)
                    .chain(value.shape.iter().map(|d| i64::from(*d)))
                    .collect();
                if input.shape != want {
                    problems.push(format!(
                        "input `{}` is {:?}; the declaration makes it {want:?}",
                        input.name, input.shape
                    ));
                }
            }
            None => problems.push(format!(
                "input `{}` isn't a declared observation (declared: {})",
                input.name,
                declaration
                    .inputs
                    .iter()
                    .map(|value| value.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    let mut seen_state_out = None;
    for output in &model.outputs {
        if output.name == STATE_OUT {
            seen_state_out = Some(output.shape.clone());
            continue;
        }
        if output.elem_type != FLOAT {
            problems.push(format!("output `{}` must be f32", output.name));
        }
        let Some(value) = declaration
            .outputs
            .iter()
            .find(|value| value.name == output.name)
        else {
            problems.push(format!("output `{}` isn't a declared action", output.name));
            continue;
        };
        let mut want: Vec<i64> = std::iter::once(1)
            .chain(value.shape.iter().map(|d| i64::from(*d)))
            .collect();
        if value.dtype == Dtype::U8 {
            match discrete_choices(value) {
                Some(choices) => want.push(choices),
                None => {
                    problems.push(format!(
                        "action `{}` is discrete but its elements don't share one range",
                        value.name
                    ));
                    continue;
                }
            }
        }
        if output.shape != want {
            problems.push(format!(
                "output `{}` is {:?}; the contract makes it {want:?}",
                output.name, output.shape
            ));
        }
    }
    for value in &declaration.outputs {
        if !model.outputs.iter().any(|output| output.name == value.name) {
            problems.push(format!(
                "no output for the declared action `{}`",
                value.name
            ));
        }
    }
    match (state, seen_state_out) {
        (Some(size), Some(shape)) if shape == vec![1, size] => {}
        (Some(size), Some(shape)) => problems.push(format!(
            "`{STATE_OUT}` is {shape:?}; it must match `{STATE_IN}`, [1, {size}]"
        )),
        (Some(_), None) => problems.push(format!("`{STATE_IN}` needs a `{STATE_OUT}`")),
        (None, Some(_)) => problems.push(format!("`{STATE_OUT}` needs a `{STATE_IN}`")),
        (None, None) => {}
    }
    problems
}

/// A discrete action's choice count: every element shares one `[min, max]`.
pub fn discrete_choices(value: &botstadia_interface::declaration::ValueDecl) -> Option<i64> {
    let (min, max) = value.bounds.range(0)?;
    (0..value.elements())
        .all(|index| value.bounds.range(index) == Some((min, max)))
        .then_some((max - min) as i64 + 1)
}
