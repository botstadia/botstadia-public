//! Budget versions: what a bot may compute and how much memory it may use,
//! counted identically on every machine.
//!
//! A budget version fixes the fuel cost table, the inference rates, the
//! Wasmtime and ONNX Runtime versions, the opset range, and every limit. Each
//! session names one, and the CLI and `botstadia-train` run every live one,
//! so a local count matches the enclave's exactly.
//!
//! Some parts only an enclave enforces, since they're timed in canary ticks
//! (the compile and load ceilings, the backstop and the kill grace). They're
//! listed here so a version's whole contents are in one place, and the local
//! tools report this machine's wall times beside them.

use wasmtime::{Config, OperatorCost, OptLevel, VariableOperatorCost};

/// One budget version's contents.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BudgetVersion {
    pub id: u32,
    /// Wasmtime, as pinned.
    pub wasmtime: &'static str,
    /// ONNX Runtime, as linked.
    pub onnx_runtime: &'static str,
    /// The ONNX opsets a model may declare.
    pub opset_min: i64,
    pub opset_max: i64,
    /// Fuel charged per multiply-accumulate in a reducing operator (matrix
    /// products, convolutions, recurrent cells), in thousandths.
    pub milli_fuel_per_mac: u64,
    /// Fuel charged per element any other operator touches (the larger of its
    /// inputs and outputs), in thousandths.
    pub milli_fuel_per_element: u64,
    /// Fuel charged per node, whatever it does, in thousandths: ONNX
    /// Runtime's own cost of running a node at all, so a model of very many
    /// tiny nodes pays for them.
    pub milli_fuel_per_node: u64,
    /// Fuel charged per call, whatever the model, in thousandths: the cost
    /// of a call's trip through the seat's processes in an enclave (to its
    /// inference process and back), measured on the enclave's CPU class.
    pub milli_fuel_per_call: u64,
    /// A bot's WASM memory.
    pub wasm_memory_bytes: usize,
    /// Table elements a bot may hold, across all its tables.
    pub table_elements: usize,
    /// Resource handles a bot may hold at once: both the host's table (WASI
    /// streams and the like) and the store's own, for resources the guest
    /// makes itself.
    pub resource_handles: usize,
    /// What one call from the bot to the host may copy out of it, in bytes,
    /// beyond its largest model's inputs: every WASI call (a write is at most
    /// 4 KiB), with room to spare. A seat's limit is this plus the bytes of
    /// its largest model's inputs, so `infer` can always pass a whole input.
    /// (`infer` also charges a fuel an element for what it's passed, so a
    /// loop of calls can't make the host copy for nothing.)
    pub hostcall_base_bytes: usize,
    /// Unzip stops past this total.
    pub uncompressed_bundle_bytes: u64,
    /// A seat's bound on everything its models hold while they run, all of
    /// them together: weights and every intermediate tensor.
    pub inference_memory_bytes: u64,
    /// The largest `state_in`/`state_out`, in floats.
    pub max_state: u32,
    /// What only an enclave enforces, in its canary ticks.
    pub enclave_only: &'static [&'static str],
}

/// Budget version 1.
pub const V1: BudgetVersion = BudgetVersion {
    id: 1,
    wasmtime: "50.0.1",
    onnx_runtime: "1.28",
    opset_min: 13,
    opset_max: 21,
    // Calibrated on an Apple M1 Max (`tests/calibrate.rs`): a unit of WASM
    // fuel took 0.078 ns; a matrix multiply-accumulate in ONNX Runtime took
    // 0.28 fuel's worth of time, a tanh element 6.2 and a plain add far less.
    // Policies are dominated by their matrix products, so a MAC is charged at
    // its measured cost, rounded up, and every other element between an add's
    // and a tanh's. To be recalibrated on the published enclave CPU class.
    //
    // Two additions in place, before anything was anchored under version 1:
    // pooling (`MaxPool`, `AveragePool`) is charged per window element at the
    // MAC rate (measured: 0.33 fuel a window element), where it had been
    // charged only for its input; and every node pays ONNX Runtime's cost of
    // running it at all (measured: 167 ns, 2,048 fuel), so `Identity`,
    // `Constant`, `Shape` and scalar nodes are no longer nearly free.
    milli_fuel_per_mac: 300,
    milli_fuel_per_element: 4_000,
    milli_fuel_per_node: 2_100_000,
    // Added in place too: a call's trip through an enclave seat's processes
    // (the seat's Wasmtime process, through the runner, to its inference
    // process and back) measured 11.0 µs on the enclave's CPU class (a
    // c3-standard-8, `packages/enclave/tests/limits.rs` `infer_call_cost`),
    // 108,000 fuel at its calibrated rate: charged per call, rounded up, so
    // a bot can't spend more time in calls than its fuel pays for.
    milli_fuel_per_call: 110_000_000,
    wasm_memory_bytes: 64 << 20,
    table_elements: 100_000,
    resource_handles: 1_024,
    hostcall_base_bytes: 16 << 10,
    uncompressed_bundle_bytes: 256 << 20,
    inference_memory_bytes: 512 << 20,
    max_state: botstadia_interface::policy::MAX_STATE,
    enclave_only: &[
        "the compile ceiling (unzip, model parsing and preflight)",
        "the load ceiling",
        "the backstop multiple and the kill grace",
        "the canary loop and its calibrated rate",
        "the seat process's memory cap",
    ],
};

/// Every budget version this host runs.
pub const LIVE: &[BudgetVersion] = &[V1];

/// A live budget version by id.
pub fn version(id: u32) -> Option<&'static BudgetVersion> {
    LIVE.iter().find(|version| version.id == id)
}

/// The newest live version: what local runs use unless told otherwise.
pub fn current() -> &'static BudgetVersion {
    LIVE.last().expect("at least one live budget version")
}

impl BudgetVersion {
    /// The fuel cost table. A flat cost per instruction, more for memory
    /// access and calls, and a per-byte (per-element) cost for bulk memory
    /// and table operations, so no instruction does unbounded work for a
    /// fixed price.
    pub fn operator_cost(&self) -> OperatorCost {
        let mut cost = OperatorCost::new();
        for load_or_store in [
            &mut cost.I32Load,
            &mut cost.I64Load,
            &mut cost.F32Load,
            &mut cost.F64Load,
            &mut cost.I32Load8S,
            &mut cost.I32Load8U,
            &mut cost.I32Load16S,
            &mut cost.I32Load16U,
            &mut cost.I64Load8S,
            &mut cost.I64Load8U,
            &mut cost.I64Load16S,
            &mut cost.I64Load16U,
            &mut cost.I64Load32S,
            &mut cost.I64Load32U,
            &mut cost.I32Store,
            &mut cost.I64Store,
            &mut cost.F32Store,
            &mut cost.F64Store,
            &mut cost.I32Store8,
            &mut cost.I32Store16,
            &mut cost.I64Store8,
            &mut cost.I64Store16,
            &mut cost.I64Store32,
            &mut cost.V128Load,
            &mut cost.V128Store,
        ] {
            *load_or_store = 2;
        }
        cost.Call = 2;
        cost.CallIndirect = 3;
        cost.MemoryGrow = 16;
        // Per byte, element or page of the operand, on top of the flat cost:
        // stated, not left to Wasmtime's defaults (which are these today).
        cost.variable = VariableOperatorCost {
            memory_copy_per_byte: 1,
            memory_fill_per_byte: 1,
            memory_init_per_byte: 1,
            memory_grow_per_page: 1,
            table_copy_per_element: 1,
            table_fill_per_element: 1,
            table_init_per_element: 1,
            table_grow_per_element: 1,
            array_copy_per_element: 1,
            array_fill_per_element: 1,
            array_new_data_per_element: 1,
            array_init_data_per_element: 1,
            array_new_elem_per_element: 1,
            array_init_elem_per_element: 1,
            array_new_default_per_element: 1,
            array_new_per_element: 1,
        };
        cost
    }

    /// The fuel one inference call costs, from its counts.
    pub fn inference_charge(&self, macs: u64, elements: u64, nodes: u64) -> u64 {
        let milli = self
            .milli_fuel_per_call
            .saturating_add(macs.saturating_mul(self.milli_fuel_per_mac))
            .saturating_add(elements.saturating_mul(self.milli_fuel_per_element))
            .saturating_add(nodes.saturating_mul(self.milli_fuel_per_node));
        milli.div_ceil(1000).max(1)
    }

    /// The engine every bot seat runs on: fuel counted with this version's
    /// table (stores call it asynchronously, so running out pauses rather than
    /// traps), and compilation
    /// single-threaded, as each seat's is in an enclave.
    pub fn seat_config(&self) -> Config {
        let mut config = Config::new();
        proposals(&mut config);
        config
            .consume_fuel(true)
            .operator_cost(self.operator_cost())
            .parallel_compilation(false);
        config
    }
}

/// What a bot's (and an arena's) WebAssembly may use, and how it's compiled:
/// part of the budget version, so stated here in full rather than left to
/// Wasmtime's defaults, which change between releases. Measured on every
/// component that existed (every catalog bot, the policy shell, the template
/// bots and every arena engine): beyond WASM 1.0 they use only bulk memory,
/// reference types, sign extension and saturating float-to-int, and one
/// memory each. On: what the component model needs (bulk memory, reference
/// types), the rest of WASM 2.0 (multi-value, extended const) and SIMD, the
/// one extra a builder writing numeric code would want. Everything else is
/// off, so a bot can't reach code paths no bot needs.
pub fn proposals(config: &mut Config) {
    config
        .wasm_component_model(true)
        .wasm_bulk_memory(true)
        .wasm_reference_types(true)
        .wasm_multi_value(true)
        .wasm_extended_const(true)
        .wasm_simd(true)
        .wasm_relaxed_simd(false)
        .wasm_gc(false)
        .wasm_function_references(false)
        .wasm_exceptions(false)
        .wasm_tail_call(false)
        .wasm_threads(false)
        .wasm_shared_everything_threads(false)
        .wasm_memory64(false)
        .wasm_multi_memory(false)
        .wasm_wide_arithmetic(false)
        .wasm_custom_page_sizes(false)
        .wasm_stack_switching(false)
        .wasm_component_model_async(false)
        .cranelift_opt_level(OptLevel::Speed)
        // Memories start as plain copies, not copy-on-write images: those
        // need `memfd_create`, which an enclave's seat filter doesn't allow.
        // It changes how a memory is first filled, never what anything costs.
        .memory_init_cow(false)
        // Wasmtime 50's own values, stated.
        .max_wasm_stack(512 << 10)
        .async_stack_size(2 << 20);
}

/// The engine an arena runs on. The arena is trusted once published, so it
/// is never limited: `metered` only counts its work, in arena fuel, for the
/// step-cost figures and the session's safety limit.
///
/// Arena fuel is counted with budget version 1's cost table, fixed for good
/// (not the session's budget version), so an arena's figures stay comparable
/// across arenas, arena versions and budget versions.
pub fn arena_config(metered: bool) -> Config {
    let mut config = Config::new();
    proposals(&mut config);
    if metered {
        config.consume_fuel(true).operator_cost(V1.operator_cost());
    }
    config
}

#[cfg(test)]
mod pin {
    /// The Wasmtime that's built is the one the newest budget version names:
    /// moving it is a new budget version, never a `cargo update`.
    #[test]
    fn the_locked_wasmtime_is_the_budget_version_s() {
        let lock = include_str!("../../../Cargo.lock");
        for name in ["wasmtime", "wasmtime-wasi"] {
            let version = lock
                .split("[[package]]")
                .find(|p| p.contains(&format!("\nname = \"{name}\"\n")))
                .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = ")))
                .map(|v| v.trim_matches('"'))
                .unwrap_or_else(|| panic!("{name} isn't in Cargo.lock"));
            assert_eq!(version, super::current().wasmtime, "{name}");
        }
    }

    /// One module per proposal: the ones a budget version turns on validate,
    /// the rest don't. A change here is a new budget version.
    #[test]
    fn the_proposals_are_the_budget_version_s() {
        let engine = wasmtime::Engine::new(&super::current().seat_config()).unwrap();
        let arena = wasmtime::Engine::new(&super::arena_config(true)).unwrap();
        let on = [
            (
                "bulk memory",
                "(memory 1) (func (memory.fill (i32.const 0) (i32.const 0) (i32.const 0)))",
            ),
            ("reference types", "(table 1 externref)"),
            (
                "multi-value",
                "(func (result i32 i32) (i32.const 0) (i32.const 0))",
            ),
            (
                "extended const",
                "(global i32 (i32.add (i32.const 1) (i32.const 2)))",
            ),
            ("simd", "(func (result v128) (v128.const i64x2 0 0))"),
            (
                "sign extension",
                "(func (param i32) (result i32) (i32.extend8_s (local.get 0)))",
            ),
            (
                "saturating conversion",
                "(func (param f32) (result i32) (i32.trunc_sat_f32_s (local.get 0)))",
            ),
        ];
        let off = [
            (
                "relaxed simd",
                "(func (param v128 v128) (result v128) (i8x16.relaxed_swizzle (local.get 0) (local.get 1)))",
            ),
            ("multi-memory", "(memory 1) (memory 1)"),
            ("memory64", "(memory i64 1)"),
            ("threads", "(memory 1 1 shared)"),
            ("tail calls", "(func $f (return_call $f))"),
            (
                "function references",
                "(type $t (func)) (func (param (ref $t)))",
            ),
            ("gc", "(type (struct))"),
            ("exceptions", "(tag)"),
            (
                "wide arithmetic",
                "(func (param i64 i64 i64 i64) (result i64 i64) (i64.add128 (local.get 0) (local.get 1) (local.get 2) (local.get 3)))",
            ),
            ("custom page sizes", "(memory 1 (pagesize 1))"),
        ];
        for engine in [&engine, &arena] {
            for (name, body) in on {
                let wasm = wat::parse_str(format!("(module {body})")).unwrap();
                assert!(
                    wasmtime::Module::validate(engine, &wasm).is_ok(),
                    "{name} should be on"
                );
            }
            for (name, body) in off {
                let wasm = wat::parse_str(format!("(module {body})")).unwrap();
                assert!(
                    wasmtime::Module::validate(engine, &wasm).is_err(),
                    "{name} should be off"
                );
            }
        }
    }
}
