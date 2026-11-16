//! Calibrating a budget version's inference rates: on the published CPU class,
//! a turn's fuel spent in inference should take about as long as a turn's fuel
//! spent in WASM. Run with `cargo test --release -p botstadia-host --test
//! calibrate -- --ignored --nocapture` and set `milli_fuel_per_mac` and
//! `milli_fuel_per_element` in `budget.rs` from what it prints.

use std::time::Instant;

use botstadia_host::onnx::proto::{
    GraphProto, ModelProto, NodeProto, OperatorSetIdProto, TensorProto, TensorShapeProto,
    TypeProto, ValueInfoProto, tensor_shape_proto, type_proto,
};
use botstadia_host::onnx::runtime::{LoadedModel, NamedTensor};
use botstadia_host::{budget, onnx};
use prost::Message;

fn value(name: &str, shape: &[i64]) -> ValueInfoProto {
    ValueInfoProto {
        name: Some(name.into()),
        r#type: Some(TypeProto {
            value: Some(type_proto::Value::TensorType(type_proto::Tensor {
                elem_type: Some(1),
                shape: Some(TensorShapeProto {
                    dim: shape
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

fn model(
    nodes: Vec<NodeProto>,
    inputs: Vec<ValueInfoProto>,
    outputs: Vec<ValueInfoProto>,
    init: Vec<TensorProto>,
) -> Vec<u8> {
    ModelProto {
        ir_version: Some(10),
        opset_import: vec![OperatorSetIdProto {
            domain: Some(String::new()),
            version: Some(18),
        }],
        graph: Some(GraphProto {
            node: nodes,
            name: Some("calibrate".into()),
            initializer: init,
            input: inputs,
            output: outputs,
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn node(op: &str, inputs: &[&str], outputs: &[&str]) -> NodeProto {
    NodeProto {
        op_type: Some(op.into()),
        input: inputs.iter().map(|s| s.to_string()).collect(),
        output: outputs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn time_per_call(bytes: &[u8], input: NamedTensor) -> (f64, onnx::ModelInfo) {
    let info = onnx::preflight(bytes, budget::current(), u64::MAX).expect("preflight");
    let mut loaded = LoadedModel::load("m", bytes, info.clone()).unwrap();
    for _ in 0..20 {
        loaded.run(std::slice::from_ref(&input)).unwrap();
    }
    let calls = 200;
    let started = Instant::now();
    for _ in 0..calls {
        loaded.run(std::slice::from_ref(&input)).unwrap();
    }
    (started.elapsed().as_secs_f64() / f64::from(calls), info)
}

/// Nanoseconds per unit of WASM fuel.
fn ns_per_fuel() -> f64 {
    botstadia_host::calibrate::ns_per_fuel(50_000_000).unwrap()
}

#[test]
#[ignore]
fn calibrate() {
    let wasm = ns_per_fuel();
    println!("WASM: {wasm:.3} ns per fuel");

    let (n, k) = (256i64, 256i64);
    let weights = TensorProto {
        dims: vec![k, n],
        data_type: Some(1),
        name: Some("w".into()),
        raw_data: Some(
            vec![0u8; (k * n * 4) as usize]
                .into_iter()
                .enumerate()
                .map(|(i, _)| (i % 7) as u8)
                .collect(),
        ),
        ..Default::default()
    };
    let rows = 64i64;
    let matmul = model(
        vec![node("MatMul", &["x", "w"], &["y"])],
        vec![value("x", &[rows, k])],
        vec![value("y", &[rows, n])],
        vec![weights],
    );
    let (seconds, info) = time_per_call(
        &matmul,
        NamedTensor {
            name: "x".into(),
            shape: vec![rows as u32, k as u32],
            data: vec![0.5; (rows * k) as usize],
        },
    );
    let ns_per_mac = seconds * 1e9 / info.macs as f64;
    println!(
        "MatMul: {} MACs, {:.1} µs a call, {ns_per_mac:.4} ns per MAC → {:.0} milli-fuel per MAC",
        info.macs,
        seconds * 1e6,
        ns_per_mac / wasm * 1000.0
    );

    let elems = 1 << 18;
    let tanh = model(
        vec![node("Tanh", &["x"], &["y"])],
        vec![value("x", &[1, elems])],
        vec![value("y", &[1, elems])],
        vec![],
    );
    let (seconds, info) = time_per_call(
        &tanh,
        NamedTensor {
            name: "x".into(),
            shape: vec![1, elems as u32],
            data: vec![0.3; elems as usize],
        },
    );
    let ns_per_element = seconds * 1e9 / info.elements as f64;
    println!(
        "Tanh: {} elements, {:.1} µs a call, {ns_per_element:.4} ns per element → {:.0} milli-fuel per element",
        info.elements,
        seconds * 1e6,
        ns_per_element / wasm * 1000.0
    );

    // Pooling: a window reduction, charged per window element like a MAC.
    let side = 256i64;
    let pool = model(
        vec![NodeProto {
            attribute: vec![botstadia_host::onnx::proto::AttributeProto {
                name: Some("kernel_shape".into()),
                ints: vec![8, 8],
                r#type: Some(7),
                ..Default::default()
            }],
            ..node("MaxPool", &["x"], &["y"])
        }],
        vec![value("x", &[1, 1, side, side])],
        vec![value("y", &[1, 1, side - 7, side - 7])],
        vec![],
    );
    let (seconds, info) = time_per_call(
        &pool,
        NamedTensor {
            name: "x".into(),
            shape: vec![1, 1, side as u32, side as u32],
            data: vec![0.3; (side * side) as usize],
        },
    );
    let windows = ((side - 7) * (side - 7) * 64) as f64;
    println!(
        "MaxPool 8×8: {windows} window elements, {:.1} µs a call, {:.4} ns per window element → {:.0} milli-fuel; charged {} MACs",
        seconds * 1e6,
        seconds * 1e9 / windows,
        seconds * 1e9 / windows / wasm * 1000.0,
        info.macs
    );

    // Dispatch: a chain of scalar nodes, so each node's own work is nothing.
    let count = 1_000;
    let one = TensorProto {
        dims: vec![1],
        data_type: Some(1),
        name: Some("c".into()),
        float_data: vec![1.0],
        ..Default::default()
    };
    let names: Vec<String> = (0..=count).map(|i| format!("t{i}")).collect();
    let chain = model(
        (0..count)
            .map(|i| node("Add", &[&names[i], "c"], &[&names[i + 1]]))
            .collect(),
        vec![value("t0", &[1])],
        vec![value(&names[count], &[1])],
        vec![one],
    );
    let (seconds, info) = time_per_call(
        &chain,
        NamedTensor {
            name: "t0".into(),
            shape: vec![1],
            data: vec![0.0],
        },
    );
    let ns_per_node = seconds * 1e9 / count as f64;
    println!(
        "Dispatch: {} nodes, {:.1} µs a call, {ns_per_node:.1} ns per node → {:.0} milli-fuel per node",
        info.nodes,
        seconds * 1e6,
        ns_per_node / wasm * 1000.0
    );

    // Nodes ONNX Runtime removes at load (graph optimization level 1): a
    // chain of Identity nodes should cost about nothing per node.
    let identities = model(
        (0..count)
            .map(|i| node("Identity", &[&names[i]], &[&names[i + 1]]))
            .collect(),
        vec![value("t0", &[1])],
        vec![value(&names[count], &[1])],
        vec![],
    );
    let (seconds, _) = time_per_call(
        &identities,
        NamedTensor {
            name: "t0".into(),
            shape: vec![1],
            data: vec![0.0],
        },
    );
    println!(
        "Folded: {count} Identity nodes, {:.1} µs a call, {:.1} ns per node",
        seconds * 1e6,
        seconds * 1e9 / count as f64
    );
}
