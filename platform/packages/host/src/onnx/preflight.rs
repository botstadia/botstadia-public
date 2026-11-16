//! The ONNX preflight: everything checked about a model before it may run,
//! and its fixed charge per call.
//!
//! A model must be a static graph: every input has fixed dimensions, there's
//! no control flow, and no operator's output size depends on the data. Then
//! every tensor's size is known, every node runs on every call, and the
//! charge is exact. Shapes are inferred here, over the allowlist, from the
//! graph as uploaded.
//!
//! The charge: an operator that reduces or recurs (matrix products,
//! convolutions, recurrent cells, pooling windows) costs its
//! multiply-accumulates; any other costs the larger of its input and output
//! elements; and every node costs a fixed amount. The budget version turns
//! the three counts into fuel.
//!
//! Every model here is an upload, so nothing in it may panic the host: every
//! index is checked, every size sum saturates, and any arithmetic on a
//! model's own numbers is checked. A model that would overflow is refused
//! with a problem naming its node.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use prost::Message;

use super::proto::{
    AttributeProto, GraphProto, ModelProto, NodeProto, TensorProto, TypeProto, tensor_proto,
    tensor_shape_proto, type_proto,
};
use crate::budget::BudgetVersion;

/// An ONNX element type, as `TensorProto.DataType` numbers it.
pub type ElemType = i32;
pub const FLOAT: ElemType = 1;
pub const UINT8: ElemType = 2;
pub const INT8: ElemType = 3;
pub const INT32: ElemType = 6;
pub const INT64: ElemType = 7;
pub const BOOL: ElemType = 9;

/// A graph input or output.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TensorInfo {
    pub name: String,
    pub elem_type: ElemType,
    pub shape: Vec<i64>,
}

impl TensorInfo {
    pub fn elements(&self) -> u64 {
        elements(&self.shape)
    }
}

/// What the preflight found about a model that passes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub opset: i64,
    pub inputs: Vec<TensorInfo>,
    pub outputs: Vec<TensorInfo>,
    pub metadata: BTreeMap<String, String>,
    /// Multiply-accumulates per call.
    pub macs: u64,
    /// Elements touched per call by every other operator.
    pub elements: u64,
    /// Fuel per call, under the budget version checked against.
    pub charge: u64,
    /// The bound on what a call holds: weights and every intermediate.
    pub memory_bytes: u64,
    pub nodes: usize,
    pub operators: BTreeSet<String>,
}

/// Every operator a model may use.
pub const ALLOWLIST: &[&str] = &[
    // Reduce or recur: charged per multiply-accumulate.
    "MatMul",
    "Gemm",
    "Conv",
    "ConvTranspose",
    "LSTM",
    "GRU",
    "RNN",
    // Elementwise.
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Pow",
    "Max",
    "Min",
    "Mod",
    "Equal",
    "Less",
    "Greater",
    "LessOrEqual",
    "GreaterOrEqual",
    "And",
    "Or",
    "Xor",
    "Not",
    "Where",
    "PRelu",
    "Relu",
    "Tanh",
    "Sigmoid",
    "Exp",
    "Log",
    "Neg",
    "Abs",
    "Sqrt",
    "Reciprocal",
    "Softplus",
    "Softsign",
    "Elu",
    "Selu",
    "LeakyRelu",
    "HardSigmoid",
    "HardSwish",
    "Mish",
    "Gelu",
    "Erf",
    "Sin",
    "Cos",
    "Floor",
    "Ceil",
    "Round",
    "Sign",
    "Identity",
    "Cast",
    "Clip",
    "Dropout",
    "IsNaN",
    // Normalisation and reductions.
    "LayerNormalization",
    "BatchNormalization",
    "InstanceNormalization",
    "Softmax",
    "LogSoftmax",
    "ReduceMean",
    "ReduceSum",
    "ReduceMax",
    "ReduceMin",
    "ReduceProd",
    "ReduceL2",
    "ArgMax",
    "ArgMin",
    "GlobalAveragePool",
    "GlobalMaxPool",
    "MaxPool",
    "AveragePool",
    // Shapes.
    "Reshape",
    "Flatten",
    "Transpose",
    "Squeeze",
    "Unsqueeze",
    "Concat",
    "Slice",
    "Gather",
    "Split",
    "Expand",
    "Tile",
    "Shape",
    "Constant",
    "ConstantOfShape",
];

/// Nodes ONNX Runtime removes at load (graph optimization level 1, on a
/// static graph): they never run, so they pay no per-node charge. Measured:
/// 1.6 ns a folded Identity node against 171 ns a node that runs.
const FOLDED: &[&str] = &["Constant", "Identity", "Shape", "Dropout"];

/// The nodes that pay the per-node charge.
fn nodes_that_run(graph: &GraphProto) -> u64 {
    graph
        .node
        .iter()
        .filter(|node| !FOLDED.contains(&node.op_type()))
        .count() as u64
}

const MAC_OPERATORS: &[&str] = &[
    "MatMul",
    "Gemm",
    "Conv",
    "ConvTranspose",
    "LSTM",
    "GRU",
    "RNN",
    // A window reduction: each output element reads its whole window.
    "MaxPool",
    "AveragePool",
];

/// Check a model and compute its charge, for a turn of `turn_fuel`.
pub fn preflight(
    bytes: &[u8],
    budget: &BudgetVersion,
    turn_fuel: u64,
) -> Result<ModelInfo, Vec<String>> {
    let model =
        ModelProto::decode(bytes).map_err(|error| vec![format!("not valid ONNX: {error}")])?;
    let mut problems = Vec::new();
    let mut opset = None;
    for import in &model.opset_import {
        let domain = import.domain.as_deref().unwrap_or("");
        match domain {
            "" | "ai.onnx" => {
                if opset.is_some() {
                    problems.push("the default domain is imported more than once".into());
                }
                opset = Some(import.version.unwrap_or(0));
            }
            other => problems.push(format!("custom operator domain `{other}` isn't allowed")),
        }
    }
    let opset = opset.unwrap_or(0);
    if opset < budget.opset_min || opset > budget.opset_max {
        problems.push(format!(
            "opset {opset} is outside {}..={}",
            budget.opset_min, budget.opset_max
        ));
    }
    if !model.functions.is_empty() {
        problems.push("model-local functions aren't allowed".into());
    }
    let Some(graph) = model.graph.as_ref() else {
        problems.push("the model has no graph".into());
        return Err(problems);
    };
    let metadata = model
        .metadata_props
        .iter()
        .map(|prop| {
            (
                prop.key.clone().unwrap_or_default(),
                prop.value.clone().unwrap_or_default(),
            )
        })
        .collect();
    let mut inference = Inference::default();
    let result = inference.run(graph, opset);
    problems.extend(inference.problems.iter().cloned());
    let Ok((inputs, outputs)) = result else {
        return Err(problems);
    };
    let charge = budget.inference_charge(inference.macs, inference.elements, nodes_that_run(graph));
    if charge > turn_fuel {
        problems.push(format!(
            "one call costs {charge} fuel, more than a turn's {turn_fuel}"
        ));
    }
    if inference.memory_bytes > budget.inference_memory_bytes {
        problems.push(format!(
            "a call holds up to {} bytes, over the {}-byte bound",
            inference.memory_bytes, budget.inference_memory_bytes
        ));
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(ModelInfo {
        opset,
        inputs,
        outputs,
        metadata,
        macs: inference.macs,
        elements: inference.elements,
        charge,
        memory_bytes: inference.memory_bytes,
        nodes: graph.node.len(),
        operators: inference.operators,
    })
}

/// Elements in a shape, saturating: a size past `u64` is refused later by the
/// memory bound, never wrapped into a small one.
fn elements(shape: &[i64]) -> u64 {
    shape.iter().fold(1u64, |total, dim| {
        total.saturating_mul((*dim).max(0) as u64)
    })
}

/// Bytes a tensor holds, saturating.
fn bytes(shape: &[i64], elem_type: ElemType) -> u64 {
    elements(shape).saturating_mul(elem_size(elem_type))
}

/// Checked arithmetic on a model's own numbers, refused by name on overflow.
fn add(a: i64, b: i64) -> Res<i64> {
    a.checked_add(b)
        .ok_or_else(|| "a size overflows".to_string())
}

fn sub(a: i64, b: i64) -> Res<i64> {
    a.checked_sub(b)
        .ok_or_else(|| "a size overflows".to_string())
}

fn mul(a: i64, b: i64) -> Res<i64> {
    a.checked_mul(b)
        .ok_or_else(|| "a size overflows".to_string())
}

/// The product of dimensions, checked.
fn product(dims: &[i64]) -> Res<i64> {
    dims.iter().try_fold(1i64, |total, dim| mul(total, *dim))
}

/// An attribute list that must have exactly `len` entries.
fn exactly(values: Vec<i64>, len: usize, what: &str) -> Res<Vec<i64>> {
    if values.len() != len {
        return Err(format!("{what} has {} entries, not {len}", values.len()));
    }
    Ok(values)
}

fn elem_size(elem_type: ElemType) -> u64 {
    match elem_type {
        UINT8 | INT8 | BOOL => 1,
        INT64 | 11 => 8,
        10 | 16 => 2,
        _ => 4,
    }
}

/// A tensor known while inferring: its type and shape, and its values when
/// they're constant and small (what shape operators read).
#[derive(Debug, Clone)]
struct Known {
    elem_type: ElemType,
    shape: Vec<i64>,
    ints: Option<Vec<i64>>,
    floats: Option<Vec<f32>>,
}

impl Known {
    fn new(elem_type: ElemType, shape: Vec<i64>) -> Self {
        Self {
            elem_type,
            shape,
            ints: None,
            floats: None,
        }
    }

    fn ints(elem_type: ElemType, values: Vec<i64>) -> Self {
        Self {
            elem_type,
            shape: vec![values.len() as i64],
            ints: Some(values),
            floats: None,
        }
    }

    fn rank(&self) -> usize {
        self.shape.len()
    }
}

#[derive(Default)]
struct Inference {
    tensors: HashMap<String, Known>,
    problems: Vec<String>,
    macs: u64,
    elements: u64,
    memory_bytes: u64,
    operators: BTreeSet<String>,
}

type Res<T> = Result<T, String>;

impl Inference {
    fn run(
        &mut self,
        graph: &GraphProto,
        opset: i64,
    ) -> Result<(Vec<TensorInfo>, Vec<TensorInfo>), ()> {
        for initializer in &graph.initializer {
            if initializer.data_location == Some(tensor_proto::DataLocation::External as i32)
                || !initializer.external_data.is_empty()
            {
                self.problems.push(format!(
                    "initializer `{}` keeps its data outside the model: export with external data off",
                    initializer.name()
                ));
                continue;
            }
            let known = tensor_known(initializer);
            if known.shape.iter().any(|d| *d < 0) {
                self.problems.push(format!(
                    "initializer `{}` has a negative dimension",
                    initializer.name()
                ));
                continue;
            }
            self.memory_bytes = self
                .memory_bytes
                .saturating_add(bytes(&known.shape, known.elem_type));
            self.tensors.insert(initializer.name().to_string(), known);
        }
        if !graph.sparse_initializer.is_empty() {
            self.problems
                .push("sparse initializers aren't allowed".into());
        }
        let mut inputs = Vec::new();
        for input in &graph.input {
            if self.tensors.contains_key(input.name()) {
                continue;
            }
            match static_type(input.r#type.as_ref()) {
                Ok((elem_type, shape)) => {
                    self.memory_bytes = self.memory_bytes.saturating_add(bytes(&shape, elem_type));
                    inputs.push(TensorInfo {
                        name: input.name().to_string(),
                        elem_type,
                        shape: shape.clone(),
                    });
                    self.tensors
                        .insert(input.name().to_string(), Known::new(elem_type, shape));
                }
                Err(problem) => self
                    .problems
                    .push(format!("input `{}`: {problem}", input.name())),
            }
        }
        if !self.problems.is_empty() {
            return Err(());
        }
        for node in &graph.node {
            let op = node.op_type().to_string();
            self.operators.insert(op.clone());
            if !node.domain().is_empty() && node.domain() != "ai.onnx" {
                self.problems
                    .push(format!("`{op}` is in custom domain `{}`", node.domain()));
                return Err(());
            }
            if matches!(op.as_str(), "If" | "Loop" | "Scan") {
                self.problems.push(format!(
                    "`{op}` is control flow: a model must be a static graph (run loops in the bot's own code)"
                ));
                return Err(());
            }
            if !ALLOWLIST.contains(&op.as_str()) {
                self.problems
                    .push(format!("operator `{op}` isn't on the allowlist"));
                return Err(());
            }
            let ins: Vec<Option<Known>> = node
                .input
                .iter()
                .map(|name| {
                    if name.is_empty() {
                        None
                    } else {
                        self.tensors.get(name).cloned()
                    }
                })
                .collect();
            for (name, known) in node.input.iter().zip(&ins) {
                if !name.is_empty() && known.is_none() {
                    self.problems.push(format!(
                        "`{op}` node `{}` reads `{name}`, which nothing produces",
                        node.name()
                    ));
                    return Err(());
                }
            }
            let outs = match infer(node, &ins, opset) {
                Ok(outs) => outs,
                Err(problem) => {
                    self.problems.push(format!(
                        "`{op}` node `{}`: {problem}",
                        if node.name().is_empty() {
                            "unnamed"
                        } else {
                            node.name()
                        }
                    ));
                    return Err(());
                }
            };
            let in_elements: u64 = ins
                .iter()
                .flatten()
                .map(|k| elements(&k.shape))
                .max()
                .unwrap_or(0);
            let out_elements: u64 = outs.iter().map(|k| elements(&k.shape)).max().unwrap_or(0);
            if MAC_OPERATORS.contains(&op.as_str()) {
                self.macs = self.macs.saturating_add(macs(node, &ins, &outs));
            } else if !matches!(op.as_str(), "Constant" | "Shape" | "Identity") {
                self.elements = self.elements.saturating_add(in_elements.max(out_elements));
            }
            for (name, known) in node.output.iter().zip(outs) {
                if name.is_empty() {
                    continue;
                }
                if known.shape.iter().any(|dim| *dim < 0) {
                    self.problems.push(format!(
                        "`{op}` output `{name}` has no fixed size: a model must be a static graph"
                    ));
                    return Err(());
                }
                self.memory_bytes = self
                    .memory_bytes
                    .saturating_add(bytes(&known.shape, known.elem_type));
                self.tensors.insert(name.clone(), known);
            }
        }
        let mut outputs = Vec::new();
        for output in &graph.output {
            match self.tensors.get(output.name()) {
                Some(known) => outputs.push(TensorInfo {
                    name: output.name().to_string(),
                    elem_type: known.elem_type,
                    shape: known.shape.clone(),
                }),
                None => {
                    self.problems
                        .push(format!("output `{}` is never produced", output.name()));
                    return Err(());
                }
            }
        }
        Ok((inputs, outputs))
    }
}

fn static_type(ty: Option<&TypeProto>) -> Res<(ElemType, Vec<i64>)> {
    let Some(type_proto::Value::TensorType(tensor)) = ty.and_then(|ty| ty.value.as_ref()) else {
        return Err("only tensor inputs are allowed".into());
    };
    let shape = tensor
        .shape
        .as_ref()
        .ok_or("has no shape: every dimension must be fixed")?;
    let mut dims = Vec::new();
    for dim in &shape.dim {
        match &dim.value {
            Some(tensor_shape_proto::dimension::Value::DimValue(value)) if *value > 0 => {
                dims.push(*value)
            }
            _ => {
                return Err(
                    "has a dynamic dimension: export with fixed shapes (batch 1, no dynamic axes)"
                        .into(),
                );
            }
        }
    }
    Ok((tensor.elem_type.unwrap_or(0), dims))
}

fn tensor_known(tensor: &TensorProto) -> Known {
    let shape = tensor.dims.clone();
    let elem_type = tensor.data_type.unwrap_or(0);
    let count = elements(&shape) as usize;
    let mut known = Known::new(elem_type, shape);
    if count <= 4096 {
        known.ints = tensor_ints(tensor);
        known.floats = tensor_floats(tensor);
    }
    known
}

fn tensor_ints(tensor: &TensorProto) -> Option<Vec<i64>> {
    let raw = tensor.raw_data.as_deref().unwrap_or(&[]);
    match tensor.data_type.unwrap_or(0) {
        INT64 if !raw.is_empty() => Some(
            raw.as_chunks::<8>()
                .0
                .iter()
                .map(|c| i64::from_le_bytes(*c))
                .collect(),
        ),
        INT64 => Some(tensor.int64_data.clone()),
        INT32 if !raw.is_empty() => Some(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|c| i64::from(i32::from_le_bytes(*c)))
                .collect(),
        ),
        INT32 | INT8 | UINT8 | BOOL if raw.is_empty() => {
            Some(tensor.int32_data.iter().map(|v| i64::from(*v)).collect())
        }
        _ => None,
    }
}

fn tensor_floats(tensor: &TensorProto) -> Option<Vec<f32>> {
    let raw = tensor.raw_data.as_deref().unwrap_or(&[]);
    match tensor.data_type.unwrap_or(0) {
        FLOAT if !raw.is_empty() => Some(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect(),
        ),
        FLOAT => Some(tensor.float_data.clone()),
        _ => None,
    }
}

fn attr<'a>(node: &'a NodeProto, name: &str) -> Option<&'a AttributeProto> {
    node.attribute.iter().find(|a| a.name() == name)
}

fn attr_i(node: &NodeProto, name: &str, default: i64) -> i64 {
    attr(node, name).and_then(|a| a.i).unwrap_or(default)
}

fn attr_ints(node: &NodeProto, name: &str) -> Option<Vec<i64>> {
    attr(node, name).map(|a| a.ints.clone())
}

fn attr_s(node: &NodeProto, name: &str) -> Option<String> {
    attr(node, name)
        .and_then(|a| a.s.as_ref())
        .map(|s| String::from_utf8_lossy(s).into_owned())
}

fn input(ins: &[Option<Known>], index: usize) -> Res<&Known> {
    ins.get(index)
        .and_then(|k| k.as_ref())
        .ok_or_else(|| format!("input {index} is missing"))
}

/// An axis of a tensor of `rank`, negatives counting from the end. A scalar
/// has no axis.
fn axis(axis: i64, rank: usize) -> Res<usize> {
    let out_of_range = || format!("axis {axis} is out of range for rank {rank}");
    let rank_i = i64::try_from(rank).map_err(|_| out_of_range())?;
    let resolved = if axis < 0 {
        axis.checked_add(rank_i)
    } else {
        Some(axis)
    };
    match resolved {
        Some(r) if r >= 0 && r < rank_i => Ok(r as usize),
        _ => Err(out_of_range()),
    }
}

fn constant_ints(known: &Known, what: &str) -> Res<Vec<i64>> {
    known
        .ints
        .clone()
        .ok_or_else(|| format!("{what} must be a constant (it decides an output's size)"))
}

/// Numpy-style broadcasting.
fn broadcast(shapes: &[&[i64]]) -> Res<Vec<i64>> {
    let rank = shapes.iter().map(|s| s.len()).max().unwrap_or(0);
    let mut out = vec![1i64; rank];
    for shape in shapes {
        let offset = rank - shape.len();
        for (index, dim) in shape.iter().enumerate() {
            let slot = &mut out[offset + index];
            if *slot == 1 {
                *slot = *dim;
            } else if *dim != 1 && *dim != *slot {
                return Err(format!("shapes {shapes:?} don't broadcast"));
            }
        }
    }
    Ok(out)
}

fn same(k: &Known) -> Known {
    Known::new(k.elem_type, k.shape.clone())
}

fn reduced(shape: &[i64], axes: &[i64], keep: bool) -> Res<Vec<i64>> {
    let rank = shape.len();
    let axes: Vec<usize> = if axes.is_empty() {
        (0..rank).collect()
    } else {
        axes.iter().map(|a| axis(*a, rank)).collect::<Res<_>>()?
    };
    Ok(shape
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            if axes.contains(&i) {
                keep.then_some(1)
            } else {
                Some(*d)
            }
        })
        .collect())
}

/// The spatial dimensions a window of `kernel` slides to, over an input of
/// `[N, C, spatial…]`.
fn pool_shape(node: &NodeProto, x: &Known, kernel: &[i64]) -> Res<Vec<i64>> {
    if x.rank() < 3 {
        return Err("expects [batch, channels, spatial…]".into());
    }
    let spatial = x.rank() - 2;
    if kernel.len() != spatial {
        return Err("kernel_shape doesn't match the input's spatial rank".into());
    }
    if attr_s(node, "auto_pad").is_some_and(|p| p != "NOTSET") {
        return Err("auto_pad isn't allowed: give pads explicitly".into());
    }
    let strides = exactly(
        attr_ints(node, "strides").unwrap_or_else(|| vec![1; spatial]),
        spatial,
        "strides",
    )?;
    let dilations = exactly(
        attr_ints(node, "dilations").unwrap_or_else(|| vec![1; spatial]),
        spatial,
        "dilations",
    )?;
    let pads = exactly(
        attr_ints(node, "pads").unwrap_or_else(|| vec![0; spatial * 2]),
        spatial * 2,
        "pads",
    )?;
    if kernel
        .iter()
        .chain(&strides)
        .chain(&dilations)
        .any(|v| *v < 1)
    {
        return Err("kernel_shape, strides and dilations must be positive".into());
    }
    if pads.iter().any(|p| *p < 0) {
        return Err("pads can't be negative".into());
    }
    let ceil = attr_i(node, "ceil_mode", 0) == 1;
    let mut out = Vec::with_capacity(spatial);
    for i in 0..spatial {
        let effective = add(mul(dilations[i], kernel[i] - 1)?, 1)?;
        let span = sub(
            add(add(x.shape[2 + i], pads[i])?, pads[i + spatial])?,
            effective,
        )?;
        if span < 0 {
            return Err("the window is larger than the padded input".into());
        }
        // Both non-negative here, so this is the ceiling.
        let steps = if ceil {
            (span / strides[i]) + i64::from(span % strides[i] != 0)
        } else {
            span / strides[i]
        };
        out.push(steps + 1);
    }
    Ok(out)
}

fn infer(node: &NodeProto, ins: &[Option<Known>], opset: i64) -> Res<Vec<Known>> {
    let op = node.op_type();
    let one = |k: Known| Ok(vec![k]);
    match op {
        "Relu" | "Tanh" | "Sigmoid" | "Exp" | "Log" | "Neg" | "Abs" | "Sqrt" | "Reciprocal"
        | "Softplus" | "Softsign" | "Elu" | "Selu" | "LeakyRelu" | "HardSigmoid" | "HardSwish"
        | "Mish" | "Gelu" | "Erf" | "Sin" | "Cos" | "Floor" | "Ceil" | "Round" | "Sign"
        | "Softmax" | "LogSoftmax" | "Not" => one(same(input(ins, 0)?)),
        "Identity" => one(input(ins, 0)?.clone()),
        "IsNaN" => one(Known::new(BOOL, input(ins, 0)?.shape.clone())),
        "Dropout" => {
            let x = input(ins, 0)?;
            Ok(vec![same(x), Known::new(BOOL, x.shape.clone())])
        }
        "Clip" => {
            for index in 1..3 {
                if let Some(Some(bound)) = ins.get(index)
                    && !bound.shape.is_empty()
                    && elements(&bound.shape) != 1
                {
                    return Err("min and max must be scalars".into());
                }
            }
            one(same(input(ins, 0)?))
        }
        "Cast" => {
            let x = input(ins, 0)?;
            one(Known::new(
                attr_i(node, "to", FLOAT as i64) as ElemType,
                x.shape.clone(),
            ))
        }
        "Add" | "Sub" | "Mul" | "Div" | "Pow" | "Max" | "Min" | "Mod" | "PRelu" | "Equal"
        | "Less" | "Greater" | "LessOrEqual" | "GreaterOrEqual" | "And" | "Or" | "Xor" => {
            let shapes: Vec<&[i64]> = ins.iter().flatten().map(|k| k.shape.as_slice()).collect();
            let shape = broadcast(&shapes)?;
            let elem_type = if matches!(
                op,
                "Equal"
                    | "Less"
                    | "Greater"
                    | "LessOrEqual"
                    | "GreaterOrEqual"
                    | "And"
                    | "Or"
                    | "Xor"
            ) {
                BOOL
            } else {
                input(ins, 0)?.elem_type
            };
            let mut out = Known::new(elem_type, shape);
            // Fold small integer arithmetic, as shape computations use it.
            if let (Some(a), Some(b)) = (
                input(ins, 0)?.ints.as_ref(),
                ins.get(1)
                    .and_then(|k| k.as_ref())
                    .and_then(|k| k.ints.as_ref()),
            ) && a.len() == b.len()
                && matches!(op, "Add" | "Sub" | "Mul" | "Div")
            {
                // Checked: on any overflow (or `i64::MIN / -1`), nothing
                // is folded, and whatever needed the value says so.
                out.ints = a
                    .iter()
                    .zip(b)
                    .map(|(x, y)| match op {
                        "Add" => x.checked_add(*y),
                        "Sub" => x.checked_sub(*y),
                        "Mul" => x.checked_mul(*y),
                        _ if *y != 0 => x.checked_div(*y),
                        _ => Some(0),
                    })
                    .collect();
            }
            one(out)
        }
        "Where" => {
            let shapes: Vec<&[i64]> = ins.iter().flatten().map(|k| k.shape.as_slice()).collect();
            one(Known::new(input(ins, 1)?.elem_type, broadcast(&shapes)?))
        }
        "MatMul" => {
            let (a, b) = (input(ins, 0)?, input(ins, 1)?);
            if a.rank() < 1 || b.rank() < 1 {
                return Err("MatMul needs tensors".into());
            }
            let mut a_shape = a.shape.clone();
            let mut b_shape = b.shape.clone();
            let a_vec = a_shape.len() == 1;
            let b_vec = b_shape.len() == 1;
            if a_vec {
                a_shape.insert(0, 1);
            }
            if b_vec {
                b_shape.push(1);
            }
            let (m, k1) = (a_shape[a_shape.len() - 2], a_shape[a_shape.len() - 1]);
            let (k2, n) = (b_shape[b_shape.len() - 2], b_shape[b_shape.len() - 1]);
            if k1 != k2 {
                return Err(format!("inner dimensions {k1} and {k2} differ"));
            }
            let batch = broadcast(&[&a_shape[..a_shape.len() - 2], &b_shape[..b_shape.len() - 2]])?;
            let mut shape = batch;
            if !a_vec {
                shape.push(m);
            }
            if !b_vec {
                shape.push(n);
            }
            one(Known::new(a.elem_type, shape))
        }
        "Gemm" => {
            let (a, b) = (input(ins, 0)?, input(ins, 1)?);
            if a.rank() != 2 || b.rank() != 2 {
                return Err("Gemm takes matrices".into());
            }
            let m = if attr_i(node, "transA", 0) == 1 {
                a.shape[1]
            } else {
                a.shape[0]
            };
            let n = if attr_i(node, "transB", 0) == 1 {
                b.shape[0]
            } else {
                b.shape[1]
            };
            one(Known::new(a.elem_type, vec![m, n]))
        }
        "Conv" => {
            let (x, w) = (input(ins, 0)?, input(ins, 1)?);
            if x.rank() < 3 || w.rank() != x.rank() {
                return Err(
                    "expects [batch, channels, spatial…] and weights of the same rank".into(),
                );
            }
            let kernel = attr_ints(node, "kernel_shape").unwrap_or_else(|| w.shape[2..].to_vec());
            let mut shape = vec![x.shape[0], w.shape[0]];
            shape.extend(pool_shape(node, x, &kernel)?);
            one(Known::new(x.elem_type, shape))
        }
        "ConvTranspose" => {
            let (x, w) = (input(ins, 0)?, input(ins, 1)?);
            if x.rank() < 3 || w.rank() != x.rank() {
                return Err(
                    "expects [batch, channels, spatial…] and weights of the same rank".into(),
                );
            }
            let spatial = x.rank() - 2;
            let kernel = exactly(
                attr_ints(node, "kernel_shape").unwrap_or_else(|| w.shape[2..].to_vec()),
                spatial,
                "kernel_shape",
            )?;
            let strides = exactly(
                attr_ints(node, "strides").unwrap_or_else(|| vec![1; spatial]),
                spatial,
                "strides",
            )?;
            let pads = exactly(
                attr_ints(node, "pads").unwrap_or_else(|| vec![0; spatial * 2]),
                spatial * 2,
                "pads",
            )?;
            let dilations = exactly(
                attr_ints(node, "dilations").unwrap_or_else(|| vec![1; spatial]),
                spatial,
                "dilations",
            )?;
            let extra = exactly(
                attr_ints(node, "output_padding").unwrap_or_else(|| vec![0; spatial]),
                spatial,
                "output_padding",
            )?;
            if attr(node, "output_shape").is_some() {
                return Err("give pads, not output_shape".into());
            }
            if kernel
                .iter()
                .chain(&strides)
                .chain(&dilations)
                .any(|v| *v < 1)
            {
                return Err("kernel_shape, strides and dilations must be positive".into());
            }
            let group = attr_i(node, "group", 1);
            if group < 1 {
                return Err("group must be positive".into());
            }
            let mut shape = vec![x.shape[0], mul(w.shape[1], group)?];
            for i in 0..spatial {
                let grown = add(
                    add(mul(strides[i], sub(x.shape[2 + i], 1)?)?, extra[i])?,
                    add(mul(sub(kernel[i], 1)?, dilations[i])?, 1)?,
                )?;
                shape.push(sub(sub(grown, pads[i])?, pads[i + spatial])?);
            }
            one(Known::new(x.elem_type, shape))
        }
        "MaxPool" | "AveragePool" => {
            let x = input(ins, 0)?;
            let kernel = attr_ints(node, "kernel_shape").ok_or("kernel_shape is required")?;
            let spatial = pool_shape(node, x, &kernel)?;
            let mut shape = x.shape[..2].to_vec();
            shape.extend(spatial);
            one(Known::new(x.elem_type, shape))
        }
        "GlobalAveragePool" | "GlobalMaxPool" => {
            let x = input(ins, 0)?;
            if x.rank() < 3 {
                return Err("expects [batch, channels, spatial…]".into());
            }
            let mut shape = x.shape[..2].to_vec();
            shape.extend(std::iter::repeat_n(1, x.rank() - 2));
            one(Known::new(x.elem_type, shape))
        }
        "BatchNormalization" | "InstanceNormalization" => one(same(input(ins, 0)?)),
        "LayerNormalization" => {
            let x = input(ins, 0)?;
            let first = axis(attr_i(node, "axis", -1), x.rank())?;
            let mut stat = x.shape[..first].to_vec();
            stat.extend(std::iter::repeat_n(1, x.rank() - first));
            Ok(vec![
                same(x),
                Known::new(FLOAT, stat.clone()),
                Known::new(FLOAT, stat),
            ])
        }
        "ReduceMean" | "ReduceSum" | "ReduceMax" | "ReduceMin" | "ReduceProd" | "ReduceL2" => {
            let x = input(ins, 0)?;
            let keep = attr_i(node, "keepdims", 1) == 1;
            let takes_axes_input = op == "ReduceSum" && opset >= 13 || opset >= 18;
            let axes = if takes_axes_input {
                match ins.get(1).and_then(|k| k.as_ref()) {
                    Some(axes) => constant_ints(axes, "axes")?,
                    None => Vec::new(),
                }
            } else {
                attr_ints(node, "axes").unwrap_or_default()
            };
            if axes.is_empty() && attr_i(node, "noop_with_empty_axes", 0) == 1 {
                return one(same(x));
            }
            one(Known::new(x.elem_type, reduced(&x.shape, &axes, keep)?))
        }
        "ArgMax" | "ArgMin" => {
            let x = input(ins, 0)?;
            let keep = attr_i(node, "keepdims", 1) == 1;
            let a = attr_i(node, "axis", 0);
            one(Known::new(INT64, reduced(&x.shape, &[a], keep)?))
        }
        "Reshape" => {
            let x = input(ins, 0)?;
            let target = constant_ints(input(ins, 1)?, "the target shape")?;
            let allow_zero = attr_i(node, "allowzero", 0) == 1;
            let total = product(&x.shape)?;
            if target.iter().filter(|d| **d == -1).count() > 1 || target.iter().any(|d| *d < -1) {
                return Err(format!("{target:?} isn't a shape"));
            }
            let mut shape: Vec<i64> = target
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    if *d == 0 && !allow_zero {
                        x.shape.get(i).copied().unwrap_or(0)
                    } else {
                        *d
                    }
                })
                .collect();
            if let Some(slot) = shape.iter().position(|d| *d == -1) {
                let rest: Vec<i64> = shape.iter().copied().filter(|d| *d != -1).collect();
                let known = product(&rest)?;
                if known == 0 {
                    return Err("can't infer -1 next to a zero dimension".into());
                }
                shape[slot] = total / known;
            }
            if product(&shape)? != total {
                return Err(format!("can't reshape {:?} to {shape:?}", x.shape));
            }
            let mut out = Known::new(x.elem_type, shape);
            out.ints = x.ints.clone();
            out.floats = x.floats.clone();
            one(out)
        }
        "Flatten" => {
            let x = input(ins, 0)?;
            // Its axis runs over [-rank, rank], negatives counting from the
            // rank itself.
            let a = attr_i(node, "axis", 1);
            let rank = x.rank() as i64;
            let at = if a < 0 { a.checked_add(rank) } else { Some(a) }
                .filter(|at| (0..=rank).contains(at))
                .ok_or_else(|| format!("axis {a} is out of range for rank {rank}"))?
                as usize;
            let outer = product(&x.shape[..at])?;
            let inner = product(&x.shape[at..])?;
            one(Known::new(x.elem_type, vec![outer, inner]))
        }
        "Transpose" => {
            let x = input(ins, 0)?;
            let perm =
                attr_ints(node, "perm").unwrap_or_else(|| (0..x.rank() as i64).rev().collect());
            let mut sorted = perm.clone();
            sorted.sort_unstable();
            if sorted != (0..x.rank() as i64).collect::<Vec<_>>() {
                return Err(format!("{perm:?} isn't a permutation of the axes"));
            }
            let shape = perm.iter().map(|p| x.shape[*p as usize]).collect();
            one(Known::new(x.elem_type, shape))
        }
        "Squeeze" | "Unsqueeze" => {
            let x = input(ins, 0)?;
            let axes = if opset >= 13 {
                match ins.get(1).and_then(|k| k.as_ref()) {
                    Some(axes) => constant_ints(axes, "axes")?,
                    None => Vec::new(),
                }
            } else {
                attr_ints(node, "axes").unwrap_or_default()
            };
            let shape = if op == "Squeeze" {
                if axes.is_empty() {
                    x.shape.iter().copied().filter(|d| *d != 1).collect()
                } else {
                    let axes: Vec<usize> = axes
                        .iter()
                        .map(|a| axis(*a, x.rank()))
                        .collect::<Res<_>>()?;
                    x.shape
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !axes.contains(i))
                        .map(|(_, d)| *d)
                        .collect()
                }
            } else {
                let rank = x.rank() + axes.len();
                let mut axes: Vec<usize> =
                    axes.iter().map(|a| axis(*a, rank)).collect::<Res<_>>()?;
                axes.sort_unstable();
                if axes.windows(2).any(|w| w[0] == w[1]) {
                    return Err("an axis is named twice".into());
                }
                // Sorted and distinct, each below the final rank: every
                // insert lands within the shape built so far.
                let mut shape = x.shape.clone();
                for a in axes {
                    shape.insert(a, 1);
                }
                shape
            };
            let mut out = Known::new(x.elem_type, shape);
            out.ints = x.ints.clone();
            out.floats = x.floats.clone();
            one(out)
        }
        "Concat" => {
            let first = input(ins, 0)?;
            let at = axis(attr_i(node, "axis", 0), first.rank())?;
            let mut shape = first.shape.clone();
            shape[at] = 0;
            let mut ints = Some(Vec::new());
            for k in ins.iter().flatten() {
                if k.rank() != first.rank() {
                    return Err("inputs differ in rank".into());
                }
                shape[at] = add(shape[at], k.shape[at])?;
                match (&mut ints, &k.ints) {
                    (Some(all), Some(values)) => all.extend(values),
                    _ => ints = None,
                }
            }
            let mut out = Known::new(first.elem_type, shape);
            if first.rank() == 1 {
                out.ints = ints;
            }
            one(out)
        }
        "Slice" => {
            let x = input(ins, 0)?;
            let starts = constant_ints(input(ins, 1)?, "starts")?;
            let ends = constant_ints(input(ins, 2)?, "ends")?;
            let axes = match ins.get(3).and_then(|k| k.as_ref()) {
                Some(axes) => constant_ints(axes, "axes")?,
                None => (0..starts.len() as i64).collect(),
            };
            let steps = match ins.get(4).and_then(|k| k.as_ref()) {
                Some(steps) => constant_ints(steps, "steps")?,
                None => vec![1; starts.len()],
            };
            if ends.len() != starts.len()
                || axes.len() != starts.len()
                || steps.len() != starts.len()
            {
                return Err("starts, ends, axes and steps differ in length".into());
            }
            let mut shape = x.shape.clone();
            let mut ints = x.ints.clone();
            for i in 0..starts.len() {
                let a = axis(axes[i], x.rank())?;
                let dim = x.shape[a];
                let step = steps[i];
                if step == 0 {
                    return Err("a step of 0".into());
                }
                let clamp = |v: i64, lo: i64, hi: i64| v.max(lo).min(hi);
                let norm = |v: i64| if v < 0 { v + dim } else { v };
                let (start, end) = if step > 0 {
                    (clamp(norm(starts[i]), 0, dim), clamp(norm(ends[i]), 0, dim))
                } else {
                    (
                        clamp(norm(starts[i]), -1, dim - 1),
                        clamp(norm(ends[i]), -1, dim - 1),
                    )
                };
                // Wide, so no step or bound of the model's own overflows.
                let (start_w, end_w, step_w) = (start as i128, end as i128, step as i128);
                let ceil_div = |a: i128, b: i128| a / b + i128::from(a % b != 0);
                let len = if step_w > 0 {
                    ceil_div((end_w - start_w).max(0), step_w)
                } else {
                    ceil_div((start_w - end_w).max(0), -step_w)
                };
                shape[a] = i64::try_from(len).map_err(|_| "a slice overflows".to_string())?;
                if x.rank() == 1 {
                    // A constant whose data disagrees with its dims has no
                    // value to pick: nothing is folded.
                    ints = ints.and_then(|values| {
                        (0..len)
                            .map(|n| {
                                let at = start_w + n * step_w;
                                usize::try_from(at)
                                    .ok()
                                    .and_then(|at| values.get(at).copied())
                            })
                            .collect()
                    });
                } else {
                    ints = None;
                }
            }
            let mut out = Known::new(x.elem_type, shape);
            out.ints = ints;
            one(out)
        }
        "Gather" => {
            let (x, indices) = (input(ins, 0)?, input(ins, 1)?);
            let at = axis(attr_i(node, "axis", 0), x.rank())?;
            let mut shape = x.shape[..at].to_vec();
            shape.extend(&indices.shape);
            shape.extend(&x.shape[at + 1..]);
            let mut out = Known::new(x.elem_type, shape);
            if let (Some(values), Some(picks)) = (&x.ints, &indices.ints)
                && x.rank() == 1
            {
                out.ints = picks
                    .iter()
                    .map(|p| {
                        let p = if *p < 0 { p + values.len() as i64 } else { *p };
                        values.get(p as usize).copied()
                    })
                    .collect();
            }
            one(out)
        }
        "Split" => {
            let x = input(ins, 0)?;
            let at = axis(attr_i(node, "axis", 0), x.rank())?;
            let count = node.output.len() as i64;
            if count == 0 {
                return Err("a Split with no outputs".into());
            }
            let sizes = match ins.get(1).and_then(|k| k.as_ref()) {
                Some(split) => constant_ints(split, "split")?,
                None => match attr_ints(node, "split") {
                    Some(split) => split,
                    None => {
                        let dim = x.shape[at];
                        let each = (dim + count - 1) / count;
                        (0..count).map(|i| each.min(dim - each * i)).collect()
                    }
                },
            };
            if sizes.len() as i64 != count {
                return Err(format!("{} sizes for {count} outputs", sizes.len()));
            }
            Ok(sizes
                .iter()
                .map(|size| {
                    let mut shape = x.shape.clone();
                    shape[at] = *size;
                    Known::new(x.elem_type, shape)
                })
                .collect())
        }
        "Expand" => {
            let x = input(ins, 0)?;
            let target = constant_ints(input(ins, 1)?, "the target shape")?;
            one(Known::new(x.elem_type, broadcast(&[&x.shape, &target])?))
        }
        "Tile" => {
            let x = input(ins, 0)?;
            let repeats = exactly(
                constant_ints(input(ins, 1)?, "repeats")?,
                x.rank(),
                "repeats",
            )?;
            one(Known::new(
                x.elem_type,
                x.shape
                    .iter()
                    .zip(&repeats)
                    .map(|(d, r)| mul(*d, *r))
                    .collect::<Res<_>>()?,
            ))
        }
        "Shape" => {
            let x = input(ins, 0)?;
            let rank = x.rank() as i64;
            let norm = |v: i64| (if v < 0 { v + rank } else { v }).clamp(0, rank) as usize;
            let start = norm(attr_i(node, "start", 0));
            let end = norm(attr_i(node, "end", rank));
            one(Known::ints(INT64, x.shape[start..end.max(start)].to_vec()))
        }
        "Constant" => {
            if let Some(tensor) = attr(node, "value").and_then(|a| a.t.as_ref()) {
                return one(tensor_known(tensor));
            }
            if let Some(a) = attr(node, "value_ints") {
                return one(Known::ints(INT64, a.ints.clone()));
            }
            if let Some(i) = attr(node, "value_int").and_then(|a| a.i) {
                let mut k = Known::new(INT64, vec![]);
                k.ints = Some(vec![i]);
                return one(k);
            }
            if let Some(f) = attr(node, "value_float").and_then(|a| a.f) {
                let mut k = Known::new(FLOAT, vec![]);
                k.floats = Some(vec![f]);
                return one(k);
            }
            if let Some(a) = attr(node, "value_floats") {
                let mut k = Known::new(FLOAT, vec![a.floats.len() as i64]);
                k.floats = Some(a.floats.clone());
                return one(k);
            }
            Err("a Constant with no value this preflight reads".into())
        }
        "ConstantOfShape" => {
            let shape = constant_ints(input(ins, 0)?, "the shape")?;
            let elem_type = attr(node, "value")
                .and_then(|a| a.t.as_ref())
                .and_then(|t| t.data_type)
                .unwrap_or(FLOAT);
            one(Known::new(elem_type, shape))
        }
        "LSTM" | "GRU" | "RNN" => {
            let x = input(ins, 0)?;
            if x.rank() != 3 {
                return Err("expects [sequence, batch, input]".into());
            }
            let hidden = attr_i(node, "hidden_size", 0);
            if hidden <= 0 {
                return Err("hidden_size is required".into());
            }
            if attr_i(node, "layout", 0) != 0 {
                return Err("only layout 0 is allowed".into());
            }
            if ins.get(4).and_then(|k| k.as_ref()).is_some() {
                return Err(
                    "sequence_lens isn't allowed: every call runs the whole sequence".into(),
                );
            }
            let directions = if attr_s(node, "direction").as_deref() == Some("bidirectional") {
                2
            } else {
                1
            };
            let (seq, batch) = (x.shape[0], x.shape[1]);
            let mut outs = vec![
                Known::new(x.elem_type, vec![seq, directions, batch, hidden]),
                Known::new(x.elem_type, vec![directions, batch, hidden]),
            ];
            if op == "LSTM" {
                outs.push(Known::new(x.elem_type, vec![directions, batch, hidden]));
            }
            Ok(outs)
        }
        other => Err(format!("`{other}` has no shape rule")),
    }
}

/// Multiply-accumulates for one call of a reducing operator. Saturating, and
/// every index checked: a count past `u64` prices the model out, never wraps.
fn macs(node: &NodeProto, ins: &[Option<Known>], outs: &[Known]) -> u64 {
    let out = outs.first().map(|k| elements(&k.shape)).unwrap_or(0);
    let shape = |i: usize| {
        ins.get(i)
            .and_then(|k| k.as_ref())
            .map(|k| k.shape.clone())
            .unwrap_or_default()
    };
    let dim = |s: &[i64], i: usize| s.get(i).copied().unwrap_or(0).max(0) as u64;
    match node.op_type() {
        "MatMul" => {
            let a = shape(0);
            out.saturating_mul(a.last().copied().unwrap_or(1).max(0) as u64)
        }
        "Gemm" => {
            let a = shape(0);
            let k = if attr_i(node, "transA", 0) == 1 {
                dim(&a, 0)
            } else {
                dim(&a, 1)
            };
            out.saturating_mul(k)
        }
        "Conv" => {
            let w = shape(1);
            // Each output element sums over its group's input channels and the kernel.
            out.saturating_mul(elements(w.get(1..).unwrap_or(&[])))
        }
        "ConvTranspose" => {
            let (x, w) = (shape(0), shape(1));
            elements(&x).saturating_mul(elements(w.get(1..).unwrap_or(&[])))
        }
        "MaxPool" | "AveragePool" => out.saturating_mul(elements(
            &attr_ints(node, "kernel_shape").unwrap_or_default(),
        )),
        "LSTM" | "GRU" | "RNN" => {
            let x = shape(0);
            let gates = match node.op_type() {
                "LSTM" => 4,
                "GRU" => 3,
                _ => 1,
            };
            let hidden = attr_i(node, "hidden_size", 0).max(0) as u64;
            let directions = if attr_s(node, "direction").as_deref() == Some("bidirectional") {
                2
            } else {
                1
            };
            let (seq, batch, input) = (dim(&x, 0), dim(&x, 1), dim(&x, 2));
            [
                seq,
                batch,
                directions,
                gates,
                hidden,
                input.saturating_add(hidden),
            ]
            .into_iter()
            .fold(1u64, u64::saturating_mul)
        }
        _ => 0,
    }
}

#[cfg(test)]
mod fuzz {
    //! Preflight reads uploads, so no model may panic it. Valid little graphs
    //! over every allowlisted operator, mutated: their attributes, ranks and
    //! dims, extreme numbers, initializers whose data disagrees with their
    //! dims, and duplicate opset imports. Deterministic and small enough for
    //! `task test`.

    use super::super::proto::{
        AttributeProto, GraphProto, ModelProto, NodeProto, OperatorSetIdProto, TensorProto,
        TensorShapeProto, TypeProto, ValueInfoProto, tensor_shape_proto, type_proto,
    };
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len() as u64) as usize]
        }
        fn int(&mut self) -> i64 {
            *self.pick(&[
                0,
                1,
                2,
                3,
                -1,
                -2,
                7,
                64,
                i64::MAX,
                i64::MIN,
                i64::MAX / 2,
                1 << 32,
                -(1 << 40),
            ])
        }
        fn ints(&mut self) -> Vec<i64> {
            (0..self.below(7)).map(|_| self.int()).collect()
        }
    }

    fn tensor_input(name: &str, dims: &[i64]) -> ValueInfoProto {
        ValueInfoProto {
            name: Some(name.into()),
            r#type: Some(TypeProto {
                value: Some(type_proto::Value::TensorType(type_proto::Tensor {
                    elem_type: Some(FLOAT),
                    shape: Some(TensorShapeProto {
                        dim: dims
                            .iter()
                            .map(|d| tensor_shape_proto::Dimension {
                                value: Some(tensor_shape_proto::dimension::Value::DimValue(*d)),
                                ..Default::default()
                            })
                            .collect(),
                    }),
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn constant(rng: &mut Rng, name: &str) -> TensorProto {
        let dims = (0..rng.below(4)).map(|_| rng.int()).collect();
        let data_type = *rng.pick(&[FLOAT, INT64, INT32, BOOL, UINT8]);
        TensorProto {
            name: Some(name.into()),
            dims,
            data_type: Some(data_type),
            // Deliberately unrelated to the dims.
            int64_data: rng.ints(),
            int32_data: (0..rng.below(5)).map(|_| rng.int() as i32).collect(),
            float_data: (0..rng.below(5)).map(|_| rng.below(9) as f32).collect(),
            raw_data: rng
                .below(3)
                .eq(&0)
                .then(|| (0..rng.below(40)).map(|_| rng.next() as u8).collect()),
            ..Default::default()
        }
    }

    const ATTRS: &[&str] = &[
        "axis",
        "axes",
        "perm",
        "kernel_shape",
        "strides",
        "pads",
        "dilations",
        "output_padding",
        "group",
        "ceil_mode",
        "keepdims",
        "transA",
        "transB",
        "hidden_size",
        "allowzero",
        "split",
        "start",
        "end",
        "to",
        "noop_with_empty_axes",
        "value_ints",
        "value_int",
    ];

    fn model(rng: &mut Rng) -> ModelProto {
        let op = *rng.pick(ALLOWLIST);
        let rank = rng.below(6) as usize;
        let dims: Vec<i64> = (0..rank).map(|_| *rng.pick(&[1, 2, 3, 5, 8])).collect();
        let mut initializer = Vec::new();
        let mut inputs = vec!["x".to_string()];
        for i in 0..rng.below(5) {
            let name = format!("c{i}");
            initializer.push(constant(rng, &name));
            inputs.push(name);
        }
        if rng.below(4) == 0 {
            inputs.insert(1, String::new());
        }
        let attribute = (0..rng.below(5))
            .map(|_| AttributeProto {
                name: Some((*rng.pick(ATTRS)).into()),
                i: Some(rng.int()),
                ints: rng.ints(),
                ..Default::default()
            })
            .collect();
        let outputs = (0..1 + rng.below(3)).map(|i| format!("y{i}")).collect();
        let mut opset_import = vec![OperatorSetIdProto {
            domain: Some(String::new()),
            version: Some(*rng.pick(&[13, 17, 18, 21])),
        }];
        if rng.below(10) == 0 {
            opset_import.push(OperatorSetIdProto {
                domain: Some("ai.onnx".into()),
                version: Some(99),
            });
        }
        ModelProto {
            opset_import,
            graph: Some(GraphProto {
                node: vec![NodeProto {
                    op_type: Some(op.into()),
                    input: inputs,
                    output: outputs,
                    attribute,
                    ..Default::default()
                }],
                initializer,
                input: vec![tensor_input("x", &dims)],
                output: vec![tensor_input("y0", &[1])],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn no_model_panics_preflight() {
        let budget = crate::budget::current();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for case in 0..20_000 {
            let model = model(&mut rng);
            let bytes = model.encode_to_vec();
            let op = model.graph.as_ref().unwrap().node[0].op_type().to_string();
            let outcome = std::panic::catch_unwind(|| preflight(&bytes, budget, 1_000_000));
            assert!(
                outcome.is_ok(),
                "case {case}: `{op}` panicked preflight: {model:?}"
            );
        }
    }

    fn graph_of(nodes: Vec<NodeProto>, input: ValueInfoProto, output: &str) -> Vec<u8> {
        ModelProto {
            opset_import: vec![OperatorSetIdProto {
                domain: Some(String::new()),
                version: Some(17),
            }],
            graph: Some(GraphProto {
                node: nodes,
                input: vec![input],
                output: vec![tensor_input(output, &[1])],
                ..Default::default()
            }),
            ..Default::default()
        }
        .encode_to_vec()
    }

    /// A 256×256 input pooled by a 128×128 window at stride 1 does about a
    /// billion comparisons, and is charged for each.
    #[test]
    fn pooling_is_charged_per_window_element() {
        let budget = crate::budget::current();
        let pool = NodeProto {
            op_type: Some("MaxPool".into()),
            input: vec!["x".into()],
            output: vec!["y".into()],
            attribute: vec![AttributeProto {
                name: Some("kernel_shape".into()),
                ints: vec![128, 128],
                ..Default::default()
            }],
            ..Default::default()
        };
        let info = preflight(
            &graph_of(vec![pool], tensor_input("x", &[1, 1, 256, 256]), "y"),
            budget,
            u64::MAX,
        )
        .unwrap();
        assert_eq!(info.macs, 129 * 129 * 128 * 128);
        assert!(info.charge > 80_000_000, "{}", info.charge);
    }

    /// A node that runs pays for running at all; one folded away at load
    /// doesn't.
    #[test]
    fn every_node_that_runs_pays_a_fixed_charge() {
        let budget = crate::budget::current();
        let chain = |op: &str| -> Vec<NodeProto> {
            (0..100)
                .map(|i| NodeProto {
                    op_type: Some(op.into()),
                    input: vec![if i == 0 { "x".into() } else { format!("t{i}") }],
                    output: vec![if i == 99 {
                        "y".into()
                    } else {
                        format!("t{}", i + 1)
                    }],
                    ..Default::default()
                })
                .collect()
        };
        let charge = |op: &str| {
            preflight(
                &graph_of(chain(op), tensor_input("x", &[1]), "y"),
                budget,
                u64::MAX,
            )
            .unwrap()
            .charge
        };
        assert_eq!(
            charge("Identity"),
            budget.milli_fuel_per_call / 1000,
            "folded at load, so only the call itself"
        );
        assert_eq!(
            charge("Abs"),
            (budget.milli_fuel_per_call
                + 100 * budget.milli_fuel_per_node
                + 100 * budget.milli_fuel_per_element)
                / 1000
        );
    }

    #[test]
    fn the_reported_panics_are_refused_by_name() {
        let budget = crate::budget::current();
        let x = |dims: &[i64]| tensor_input("x", dims);
        let attr = |name: &str, ints: Vec<i64>| AttributeProto {
            name: Some(name.into()),
            ints,
            ..Default::default()
        };
        let one = |op: &str, attribute: Vec<AttributeProto>, input: ValueInfoProto| ModelProto {
            opset_import: vec![OperatorSetIdProto {
                domain: Some(String::new()),
                version: Some(17),
            }],
            graph: Some(GraphProto {
                node: vec![NodeProto {
                    op_type: Some(op.into()),
                    input: vec!["x".into()],
                    output: vec!["y".into()],
                    attribute,
                    ..Default::default()
                }],
                input: vec![input],
                output: vec![tensor_input("y", &[1])],
                ..Default::default()
            }),
            ..Default::default()
        };
        let cases = [
            // Pool attributes shorter than the spatial rank, and a zero stride.
            one(
                "MaxPool",
                vec![attr("kernel_shape", vec![2, 2]), attr("strides", vec![1])],
                x(&[1, 1, 4, 4]),
            ),
            one(
                "MaxPool",
                vec![attr("kernel_shape", vec![2]), attr("strides", vec![0])],
                x(&[1, 1, 4]),
            ),
            // Pooling a tensor of rank below two.
            one("MaxPool", vec![attr("kernel_shape", vec![])], x(&[4])),
            one("GlobalMaxPool", vec![], x(&[4])),
            // An axis on a scalar.
            one("Concat", vec![], x(&[])),
        ];
        for model in cases {
            let result = preflight(&model.encode_to_vec(), budget, 1_000_000);
            assert!(result.is_err(), "{model:?}");
        }
        // Duplicate default-domain imports: the last no longer hides the first.
        let mut twice = one("Relu", vec![], x(&[1]));
        twice.opset_import.push(OperatorSetIdProto {
            domain: Some("ai.onnx".into()),
            version: Some(17),
        });
        let problems = preflight(&twice.encode_to_vec(), budget, 1_000_000).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("more than once")),
            "{problems:?}"
        );
    }
}
