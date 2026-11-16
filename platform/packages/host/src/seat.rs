//! A bot in a seat: its own thread, its own fuelled store, its own models.
//!
//! Each turn the seat gets its turn's fuel. Running out pauses the bot
//! (wasmtime's fuel yield), and the seat has no action that turn. Next turn
//! the paused computation resumes and finishes, its result is discarded, and
//! the bot gets the current input with whatever fuel is left. It keeps its
//! memory. A trap ends the bot: it has no action for the rest of the session.
//!
//! How the counting works, since it's what makes a budget the same on every
//! machine:
//!
//! - The store holds a large reserve, and its yield interval is the fuel the
//!   current call may spend. Spending it makes wasmtime refuel the next chunk
//!   from the reserve and yield; the seat sees the yield (its waker fired
//!   during the poll) and parks the call. Its call future owns the store, so
//!   it parks whole.
//! - The chunk a parked computation resumes with is fixed when it parks, so a
//!   paused computation resumes with the allowance it was paused under, never
//!   more than a turn's.
//! - `infer` charges from inside the call: fuel left is the store's fuel less
//!   the reserve, and the charge shrinks the current chunk.
//! - The seat's thread runs a current-thread tokio runtime with its clock
//!   paused, so a WASI wait resolves at once: nothing a bot does ever blocks.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use botstadia_interface::bindings::host::agent::botstadia::agent::host as wit;
use botstadia_interface::bindings::host::agent::{Agent, AgentPre};
use wasmtime::component::{Access, Component, HasData, Linker, ResourceTable};
use wasmtime::{AsContextMut, Engine, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{
    HostMonotonicClock, HostWallClock, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView,
};

use crate::WasmtimeContext;
use crate::budget::BudgetVersion;
use crate::bundle::Bundle;
use crate::onnx::preflight::ModelInfo;
use crate::onnx::runtime::{LoadedModel, NamedTensor};
use crate::seed::Seed;

/// Fuel held back so running out of a turn's allowance yields, never traps.
const RESERVE: u64 = 1 << 52;
/// What an unbudgeted call (training) may spend: effectively unlimited.
const UNBUDGETED: u64 = 1 << 50;
/// The wall clock every bot reads: a fixed moment, so a bot can't tell a
/// warm-up from a bettable session by the date.
const WALL_CLOCK: Duration = Duration::from_secs(1_767_225_600);
/// Set in a bot's WASI environment only when someone is reading its logs.
pub const LOGS_ENV: &str = "BOTSTADIA_BOT_LOGS";
/// The WASI clock interface whose `now` runs on fuel.
const MONOTONIC_CLOCK: &str = "wasi:clocks/monotonic-clock@0.2.12";
/// A log line past this is cut.
const LOG_LINE_MAX: usize = 1024;

/// The seat engine for a budget version: one per process.
pub fn engine(budget: &BudgetVersion) -> &'static Engine {
    static V1: OnceLock<Engine> = OnceLock::new();
    assert_eq!(budget.id, 1, "budget version {} has no engine", budget.id);
    V1.get_or_init(|| Engine::new(&budget.seat_config()).expect("the seat engine"))
}

/// A virtual monotonic clock: one nanosecond per unit of fuel spent.
#[derive(Clone, Default)]
pub struct FuelClock(Arc<AtomicU64>);

impl HostMonotonicClock for FuelClock {
    fn resolution(&self) -> u64 {
        1
    }
    fn now(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

struct FixedWall;

impl HostWallClock for FixedWall {
    fn resolution(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn now(&self) -> Duration {
        WALL_CLOCK
    }
}

/// The empty sandbox: seeded randomness, virtual clocks, no files, no
/// network, no environment beyond `env`, and stdout and stderr to `logs` or
/// nowhere. Sockets and name lookup are refused at creation, before the host
/// opens anything: stated here rather than left to wasmtime-wasi's defaults,
/// which a later version could change.
pub fn sandbox_wasi(
    stream: &Seed,
    logs: Option<(MemoryOutputPipe, MemoryOutputPipe)>,
    env: &[(&str, &str)],
) -> WasiCtx {
    sandbox_wasi_with_clock(stream, logs, env, FuelClock::default())
}

fn sandbox_wasi_with_clock(
    stream: &Seed,
    logs: Option<(MemoryOutputPipe, MemoryOutputPipe)>,
    env: &[(&str, &str)],
    clock: FuelClock,
) -> WasiCtx {
    use rand::SeedableRng;
    let mut builder = WasiCtxBuilder::new();
    let secure = crate::seed::derive(stream, "secure-random");
    let insecure = crate::seed::derive(stream, "insecure-random");
    builder
        .secure_random(rand::rngs::StdRng::from_seed(secure))
        .insecure_random(rand::rngs::StdRng::from_seed(insecure))
        .insecure_random_seed(u128::from_le_bytes(
            stream[..16].try_into().expect("16 bytes"),
        ))
        .wall_clock(FixedWall)
        .monotonic_clock(clock)
        .allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false);
    for (key, value) in env {
        builder.env(key, value);
    }
    if let Some((out, err)) = logs {
        builder.stdout(out).stderr(err);
    }
    builder.build()
}

/// What one call into the host may copy out of the bot: the budget version's
/// base, plus the bytes of the largest set of inputs any of its models takes
/// (names and shapes included in the base's slack).
fn hostcall_fuel(budget: &BudgetVersion, models: &[ModelInfo]) -> usize {
    let largest = models
        .iter()
        .map(|model| {
            model
                .inputs
                .iter()
                .map(|input| input.elements().saturating_mul(4))
                .fold(0u64, u64::saturating_add)
        })
        .max()
        .unwrap_or(0);
    budget
        .hostcall_base_bytes
        .saturating_add(usize::try_from(largest).unwrap_or(usize::MAX))
}

/// A bot, compiled once and preflighted, ready to seat any number of times.
pub struct Program {
    pub bundle: Bundle,
    component: Component,
    pub models: Vec<ModelInfo>,
    pub budget: &'static BudgetVersion,
}

impl Program {
    /// Compile the bot and preflight its models, for a turn of `turn_fuel`.
    pub fn new(bundle: Bundle, budget: &'static BudgetVersion, turn_fuel: u64) -> Result<Self> {
        let component = Component::from_binary(engine(budget), &bundle.component)
            .ctx(format!("compiling the bot `{}`", bundle.name))?;
        let mut models = Vec::new();
        for (name, bytes) in &bundle.models {
            let info = crate::onnx::preflight(bytes, budget, turn_fuel).map_err(|problems| {
                anyhow!("model `{name}` fails preflight: {}", problems.join("; "))
            })?;
            models.push(info);
        }
        within_seat_memory(models.iter().map(|m| m.memory_bytes), budget)?;
        Ok(Self {
            bundle,
            component,
            models,
            budget,
        })
    }

    pub fn name(&self) -> &str {
        &self.bundle.name
    }
}

/// The inference bound covers all a seat's models together, which share its
/// one inference process (BOTS.md, "Inference memory").
fn within_seat_memory(models: impl Iterator<Item = u64>, budget: &BudgetVersion) -> Result<()> {
    let total: u64 = models.sum();
    if total > budget.inference_memory_bytes {
        return Err(anyhow!(
            "the models together hold up to {total} bytes in a call, over the seat's {}-byte bound",
            budget.inference_memory_bytes
        ));
    }
    Ok(())
}

#[cfg(test)]
mod seat_memory_tests {
    #[test]
    fn the_inference_bound_is_the_seat_s_not_each_model_s() {
        let b = &crate::budget::V1;
        let half = b.inference_memory_bytes / 2;
        assert!(super::within_seat_memory([half, half].into_iter(), b).is_ok());
        assert!(super::within_seat_memory([half, half, 1].into_iter(), b).is_err());
        assert!(super::within_seat_memory(std::iter::empty(), b).is_ok());
    }
}

/// How a seat runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SeatSettings {
    pub turn_fuel: u64,
    pub init_fuel: u64,
    /// Enforce the budget: on in sessions, optional in training.
    pub budget: bool,
    /// Capture what the bot prints, and tell it someone's reading.
    pub logs: bool,
    /// The seat's own random stream.
    pub stream: Seed,
}

/// What one turn (or `init`) did.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TurnReport {
    /// What the bot returned, unchecked. `None` when it had no output: paused,
    /// still finishing a paused computation, trapped or inactive.
    pub output: Option<Vec<u8>>,
    /// Fuel spent in the bot's own WASM this turn.
    pub wasm_fuel: u64,
    /// Fuel charged for inference this turn.
    pub inference_fuel: u64,
    /// Inference calls this turn.
    pub inference_calls: u32,
    /// This machine's wall time for the turn's calls, inference included.
    pub wall: Duration,
    /// It ran out of fuel: a computation is parked.
    pub paused: bool,
    /// It trapped this turn; it's inactive from now on.
    pub trapped: Option<String>,
    /// All its linear memories together, as the store's limits have let them
    /// grow, in bytes. A growth the limits allowed but the allocator then
    /// refused still counts: this is what it asked for and was allowed, not
    /// necessarily what it holds.
    pub memory_bytes: u64,
    /// Lines it printed, when logs are on.
    pub logs: Vec<String>,
}

/// What the store's host side shares with the seat's driver.
#[derive(Debug, Default)]
struct Meter {
    /// The reserve below the current chunk.
    reserve: u64,
    /// The current chunk's size: the yield interval.
    interval: u64,
    turn: u32,
    inference_fuel: u64,
    inference_calls: u32,
    /// The virtual clock when the running call started, and the store's fuel
    /// total then: `now` is the one plus what's been spent since.
    clock_origin: u64,
    call_total: u64,
}

/// The store's limits, made per bot rather than per memory or per table.
///
/// Wasmtime's own limits apply to each memory and each table on its own
/// (`StoreLimits`), and a bot can hold several: one per core module in its
/// component, more with reference types. So the budget version's WASM memory
/// and table elements are enforced here on their totals across the whole
/// store (BOTS.md, "Other limits"), and the total memory is the metrics
/// file's memory series. A memory or table being created arrives as growth
/// from nothing, so it counts too.
struct SeatLimits {
    inner: StoreLimits,
    memory_budget: usize,
    table_budget: usize,
    /// Every memory's size, together, in bytes.
    memory: Arc<AtomicU64>,
    /// Every table's elements, together.
    table_elements: usize,
}

impl ResourceLimiter for SeatLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = self.memory.load(Ordering::Relaxed) as usize;
        let after = total.saturating_add(desired.saturating_sub(current));
        if after > self.memory_budget {
            return Ok(false);
        }
        let allowed = self.inner.memory_growing(current, desired, maximum)?;
        if allowed {
            self.memory.store(after as u64, Ordering::Relaxed);
        }
        Ok(allowed)
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.inner.memory_grow_failed(error)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let after = self
            .table_elements
            .saturating_add(desired.saturating_sub(current));
        if after > self.table_budget {
            return Ok(false);
        }
        let allowed = self.inner.table_growing(current, desired, maximum)?;
        if allowed {
            self.table_elements = after;
        }
        Ok(allowed)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.inner.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.inner.instances()
    }

    fn tables(&self) -> usize {
        self.inner.tables()
    }

    fn memories(&self) -> usize {
        self.inner.memories()
    }
}

/// Why an inference call gave no outputs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InferFailure {
    /// The inputs don't fit the model, or ONNX Runtime refused them.
    BadInput(String),
    /// The call was stopped: the backstop cancelled it, or the seat's
    /// inference has ended for the session. The bot gets the same error as
    /// for a call it can't afford (COMPETITION.md#rules-for-failures).
    Stopped,
}

/// Where a seat's inference calls run: ONNX Runtime in this process
/// ([`LocalInference`]), or, in an enclave, the seat's own inference process.
pub trait Inference: Send {
    /// Run model `model` (manifest order) on `inputs`.
    fn run(
        &mut self,
        model: usize,
        inputs: Vec<NamedTensor>,
    ) -> Result<Vec<NamedTensor>, InferFailure>;
}

/// ONNX Runtime in the seat's own process: the CLI, `botstadia-train` and
/// local runs.
pub struct LocalInference(pub Vec<LoadedModel>);

impl LocalInference {
    /// Load a program's models.
    pub fn load(program: &Program) -> Result<Self> {
        Ok(Self(crate::onnx::runtime::load_all(
            &program.bundle.models,
            &program.models,
        )?))
    }
}

impl Inference for LocalInference {
    fn run(
        &mut self,
        model: usize,
        inputs: Vec<NamedTensor>,
    ) -> Result<Vec<NamedTensor>, InferFailure> {
        let loaded = self
            .0
            .get_mut(model)
            .ok_or_else(|| InferFailure::BadInput(format!("the bundle has no model {model}")))?;
        loaded.run(&inputs).map_err(InferFailure::BadInput)
    }
}

pub(crate) struct SeatState {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: SeatLimits,
    meter: Arc<Mutex<Meter>>,
    /// Each model's name and preflight, in manifest order.
    models: Vec<(String, ModelInfo)>,
    inference: Box<dyn Inference>,
    declaration: Arc<Vec<u8>>,
}

impl WasiView for SeatState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl wit::Host for SeatState {}

/// The marker the generated bindings reach a seat's state through.
pub(crate) struct SeatData;

impl HasData for SeatData {
    type Data<'a> = &'a mut SeatState;
}

impl<T> wit::HostWithStore<T> for SeatData {
    fn budget(mut access: Access<'_, T, Self>) -> wit::TurnBudget {
        let fuel = access.as_context_mut().get_fuel().unwrap_or(0);
        let meter = access.get().meter.clone();
        let meter = meter.lock().expect("the meter");
        wit::TurnBudget {
            fuel_left: fuel.saturating_sub(meter.reserve),
            turn: meter.turn,
        }
    }

    fn declaration(mut access: Access<'_, T, Self>) -> Vec<u8> {
        access.get().declaration.as_ref().clone()
    }

    fn models(mut access: Access<'_, T, Self>) -> Vec<wit::ModelInfo> {
        let spec = |tensor: &crate::onnx::TensorInfo| wit::TensorSpec {
            name: tensor.name.clone(),
            shape: tensor.shape.iter().map(|d| *d as u32).collect(),
        };
        access
            .get()
            .models
            .iter()
            .map(|(name, info)| wit::ModelInfo {
                name: name.clone(),
                inputs: info.inputs.iter().map(spec).collect(),
                outputs: info.outputs.iter().map(spec).collect(),
                charge: info.charge,
            })
            .collect()
    }

    fn infer(
        mut access: Access<'_, T, Self>,
        model: u32,
        inputs: Vec<wit::Tensor>,
    ) -> Result<Vec<wit::Tensor>, wit::InferenceError> {
        let fuel = access.as_context_mut().get_fuel().unwrap_or(0);
        let meter = access.get().meter.clone();
        let mut left = {
            let meter = meter.lock().expect("the meter");
            fuel.saturating_sub(meter.reserve)
        };
        // The host has already copied what the bot passed, so that's paid
        // first, one fuel an element, whatever happens next: a call that
        // goes on to fail is never free, and no loop of failing calls makes
        // the host copy for nothing.
        let copied = inputs
            .iter()
            .map(|t| t.data.len() as u64 + t.shape.len() as u64 + t.name.len() as u64 / 4)
            .fold(1u64, u64::saturating_add);
        if copied > left {
            charge_chunk(&mut access, &meter, left, 0);
            return Err(wit::InferenceError::Unaffordable);
        }
        left -= copied;
        let Some(charge) = access
            .get()
            .models
            .get(model as usize)
            .map(|(_, info)| info.charge)
        else {
            charge_chunk(&mut access, &meter, copied, left);
            return Err(wit::InferenceError::BadInput(format!(
                "the bundle has no model {model}"
            )));
        };
        if charge > left {
            charge_chunk(&mut access, &meter, copied, left);
            return Err(wit::InferenceError::Unaffordable);
        }
        charge_chunk(&mut access, &meter, copied + charge, left - charge);
        meter.lock().expect("the meter").inference_calls += 1;
        let tensors: Vec<NamedTensor> = inputs
            .into_iter()
            .map(|tensor| NamedTensor {
                name: tensor.name,
                shape: tensor.shape,
                data: tensor.data,
            })
            .collect();
        let state = access.get();
        state
            .inference
            .run(model as usize, tensors)
            .map(|outputs| {
                outputs
                    .into_iter()
                    .map(|tensor| wit::Tensor {
                        name: tensor.name,
                        shape: tensor.shape,
                        data: tensor.data,
                    })
                    .collect()
            })
            .map_err(|failure| match failure {
                InferFailure::BadInput(why) => wit::InferenceError::BadInput(why),
                InferFailure::Stopped => wit::InferenceError::Unaffordable,
            })
    }
}

/// Take `spent` out of this turn's chunk, leaving `after`: the reserve stays
/// as it is. What's taken counts as inference.
fn charge_chunk<T>(
    access: &mut Access<'_, T, SeatData>,
    meter: &Mutex<Meter>,
    spent: u64,
    after: u64,
) {
    let after = after.max(1);
    let mut store = access.as_context_mut();
    let mut meter = meter.lock().expect("the meter");
    let reserve = meter.reserve;
    if store.fuel_async_yield_interval(Some(after)).is_ok()
        && store.set_fuel(after + reserve).is_ok()
    {
        meter.interval = after;
    }
    meter.inference_fuel += spent;
}

type Call =
    Pin<Box<dyn Future<Output = (Store<SeatState>, wasmtime::Result<Option<Vec<u8>>>)> + Send>>;

/// The seat's store, between calls or inside a parked one.
enum Slot {
    Idle(Store<SeatState>),
    Parked {
        call: Call,
        /// Fuel the parked call has spent in earlier turns: whole chunks.
        spent_before: u64,
        /// The total it started from.
        started_at: u64,
    },
    Dead,
}

enum Command {
    Turn { turn: u32, input: Vec<u8> },
}

/// A seated bot, run on the calling thread: the seat process's (which may
/// start no threads once its filter is on), and under [`BotSeat`].
pub struct SeatInPlace {
    runtime: tokio::runtime::Runtime,
    driver: Driver,
}

impl SeatInPlace {
    /// Start the bot: instantiate it with `inference` for its models, and run
    /// `init` under the init budget. An error is ours (the seat couldn't
    /// start before any of the bot's code ran); the report is `init`'s.
    pub fn start(
        program: &Program,
        declaration: Arc<Vec<u8>>,
        settings: &SeatSettings,
        inference: Box<dyn Inference>,
    ) -> Result<(Self, TurnReport)> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .map_err(|error| anyhow!("starting the seat's runtime: {error}"))?;
        let mut driver = Driver::new(program, declaration, settings, inference)?;
        // From here on everything is the bot's: a crash of the host during
        // its `init` is its trap, never a void it chose.
        let init = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            driver.init(&runtime)
        })) {
            Ok(init) => init,
            Err(_) => {
                driver.slot = Slot::Dead;
                driver.agent = None;
                TurnReport {
                    trapped: Some("the seat crashed in init".into()),
                    ..TurnReport::default()
                }
            }
        };
        Ok((Self { runtime, driver }, init))
    }

    /// One turn.
    pub fn turn(&mut self, turn: u32, input: Vec<u8>) -> TurnReport {
        self.driver.turn(&self.runtime, turn, input)
    }
}

/// A seat whose turns are carried by its caller: a bot on a thread here
/// ([`BotSeat`]), or, in an enclave, a seat in processes of its own. Every
/// seat is sent its input before any is collected, so seats that don't share
/// a core think at once.
pub trait SeatLink: Send {
    fn send(&mut self, turn: u32, input: Vec<u8>);
    fn collect(&mut self) -> TurnReport;
}

/// A seated bot, running on a thread of its own.
pub struct BotSeat {
    commands: Sender<Command>,
    reports: Receiver<TurnReport>,
}

impl BotSeat {
    /// Start the bot: instantiate it, load its models, and run `init` under
    /// the init budget. The report is `init`'s.
    pub fn spawn(
        program: Arc<Program>,
        declaration: Arc<Vec<u8>>,
        settings: SeatSettings,
    ) -> Result<(Self, TurnReport)> {
        let (commands, command_rx) = channel();
        let (report_tx, reports) = channel();
        let (ready_tx, ready_rx) = channel::<Result<TurnReport>>();
        std::thread::Builder::new()
            .name(format!("seat {}", program.name()))
            .spawn(move || {
                let started = LocalInference::load(&program).and_then(|inference| {
                    SeatInPlace::start(&program, declaration, &settings, Box::new(inference))
                });
                let mut seat = match started {
                    Ok((seat, init)) => {
                        if ready_tx.send(Ok(init)).is_err() {
                            return;
                        }
                        seat
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                while let Ok(Command::Turn { turn, input }) = command_rx.recv() {
                    let report = seat.turn(turn, input);
                    if report_tx.send(report).is_err() {
                        return;
                    }
                }
            })
            .context("starting the seat's thread")?;
        let init = ready_rx
            .recv()
            .map_err(|_| anyhow!("the seat's thread stopped while starting"))??;
        Ok((Self { commands, reports }, init))
    }
}

impl BotSeat {
    /// Give the bot this turn's input. Collect the result with
    /// [`Self::collect`], so every seat thinks at once.
    pub fn send(&self, turn: u32, input: Vec<u8>) {
        let _ = self.commands.send(Command::Turn { turn, input });
    }

    pub fn collect(&self) -> TurnReport {
        self.reports.recv().unwrap_or_else(|_| TurnReport {
            trapped: Some("the seat's thread stopped".into()),
            ..TurnReport::default()
        })
    }
}

impl SeatLink for BotSeat {
    fn send(&mut self, turn: u32, input: Vec<u8>) {
        BotSeat::send(self, turn, input)
    }

    fn collect(&mut self) -> TurnReport {
        BotSeat::collect(self)
    }
}

struct Driver {
    slot: Slot,
    /// `None` when instantiation failed: the bot's start function trapped
    /// or overran, and it's inactive for the session.
    agent: Option<Arc<Agent>>,
    /// Why instantiation failed, reported as `init`'s trap.
    start_failure: Option<String>,
    /// What instantiation (the start functions) spent, out of `init`'s fuel.
    start_spent: u64,
    meter: Arc<Mutex<Meter>>,
    /// Every linear memory together, in bytes, from the store's limits.
    memory: Arc<AtomicU64>,
    clock: FuelClock,
    logs: Option<(MemoryOutputPipe, MemoryOutputPipe, usize, usize)>,
    turn_fuel: u64,
    init_fuel: u64,
    budget: bool,
    /// Every unit of fuel spent so far, for the virtual clock.
    total_spent: u64,
    /// Fuel spent past a turn's allowance, by an operation charged in one go
    /// (a bulk memory copy): paid out of the bot's following turns.
    debt: u64,
}

/// Set when the call's future woke itself during a poll: wasmtime's fuel
/// yield does exactly that, and nothing else in a bot's sandbox does.
struct SelfWake(AtomicBool);

impl Wake for SelfWake {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

enum Drive {
    Done(Store<SeatState>, wasmtime::Result<Option<Vec<u8>>>),
    Paused(Call),
}

/// Poll `call` until it finishes or yields for fuel. Waits on anything else
/// (a WASI timer) resolve at once under the paused clock.
fn drive(runtime: &tokio::runtime::Runtime, mut call: Call) -> Drive {
    let mut finished = None;
    let paused = runtime.block_on(std::future::poll_fn(|cx| {
        let wake = Arc::new(SelfWake(AtomicBool::new(false)));
        let waker = Waker::from(wake.clone());
        // Forward real wakes (timers) to the runtime too.
        let outer = cx.waker().clone();
        let combined = Waker::from(Arc::new(Both(waker, outer)));
        let mut inner = Context::from_waker(&combined);
        match call.as_mut().poll(&mut inner) {
            Poll::Ready(result) => {
                finished = Some(result);
                Poll::Ready(false)
            }
            Poll::Pending if wake.0.load(Ordering::SeqCst) => Poll::Ready(true),
            Poll::Pending => Poll::Pending,
        }
    }));
    match finished {
        Some((store, result)) if !paused => Drive::Done(store, result),
        _ => Drive::Paused(call),
    }
}

struct Both(Waker, Waker);

impl Wake for Both {
    fn wake(self: Arc<Self>) {
        self.0.wake_by_ref();
        self.1.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.wake_by_ref();
        self.1.wake_by_ref();
    }
}

impl Driver {
    fn new(
        program: &Program,
        declaration: Arc<Vec<u8>>,
        settings: &SeatSettings,
        inference: Box<dyn Inference>,
    ) -> Result<Self> {
        let budget = program.budget;
        let engine = engine(budget);
        let mut linker = Linker::<SeatState>::new(engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).ctx("linking WASI")?;
        Agent::add_to_linker::<_, SeatData>(&mut linker, |state| state)
            .ctx("linking the agent host")?;
        // The monotonic clock advances with the fuel a bot spends, inside a
        // call too: WASI's clock trait can't see the store, so `now` is
        // replaced here with one that can.
        linker.allow_shadowing(true);
        linker
            .instance(MONOTONIC_CLOCK)
            .ctx("the clock")?
            .func_wrap(
                "now",
                |store: wasmtime::StoreContextMut<'_, SeatState>, (): ()| {
                    let fuel = store.get_fuel().unwrap_or(0);
                    let meter = store.data().meter.lock().expect("the meter");
                    Ok((meter.clock_origin + meter.call_total.saturating_sub(fuel),))
                },
            )
            .ctx("the clock")?;
        linker.allow_shadowing(false);
        let models: Vec<(String, ModelInfo)> = program
            .bundle
            .models
            .iter()
            .map(|(name, _)| name.clone())
            .zip(program.models.iter().cloned())
            .collect();
        let clock = FuelClock::default();
        let logs = settings.logs.then(|| {
            (
                MemoryOutputPipe::new(4 << 20),
                MemoryOutputPipe::new(4 << 20),
            )
        });
        let env: Vec<(&str, &str)> = if settings.logs {
            vec![(LOGS_ENV, "1")]
        } else {
            Vec::new()
        };
        let wasi = sandbox_wasi_with_clock(&settings.stream, logs.clone(), &env, clock.clone());
        let meter = Arc::new(Mutex::new(Meter::default()));
        let limits = StoreLimitsBuilder::new()
            .memory_size(budget.wasm_memory_bytes)
            .table_elements(budget.table_elements)
            .instances(16)
            .tables(64)
            .memories(4)
            .trap_on_grow_failure(false)
            .build();
        let memory = Arc::new(AtomicU64::new(0));
        let limits = SeatLimits {
            inner: limits,
            memory_budget: budget.wasm_memory_bytes,
            table_budget: budget.table_elements,
            memory: memory.clone(),
            table_elements: 0,
        };
        // Linking checks every import before any of the bot's code runs, so
        // a failure here (and everything above) is ours, never the bot's.
        let pre = linker
            .instantiate_pre(&program.component)
            .ctx("linking the bot")?;
        let pre = AgentPre::new(pre).ctx("linking the bot")?;
        // House bots hold three long-lived handles and one or two passing
        // ones; the cap keeps a leaking bot from growing the host's table
        // without end. Past it, the import fails, as the bot's own failure.
        let mut table = ResourceTable::new();
        table.set_max_capacity(budget.resource_handles);
        let mut store = Store::new(
            engine,
            SeatState {
                wasi,
                table,
                limits,
                meter: meter.clone(),
                models,
                inference,
                declaration,
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_max_component_handles(budget.resource_handles);
        store.set_hostcall_fuel(hostcall_fuel(budget, &program.models));
        // Instantiation runs the bot's start functions: its own code, under
        // `init`'s budget (COMPETITION.md#rules-for-failures). It can't be
        // parked like a call, so it doesn't yield: running out of fuel traps,
        // and a trap or overrun here ends the bot, never the session.
        let start_fuel = if settings.budget {
            settings.init_fuel
        } else {
            UNBUDGETED
        };
        store.fuel_async_yield_interval(None).ctx("fuel")?;
        store.set_fuel(start_fuel).ctx("fuel")?;
        {
            let mut meter = meter.lock().expect("the meter");
            meter.call_total = start_fuel;
            meter.clock_origin = 0;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()?;
        // A crash of the host while the start function runs is the bot's
        // too: it's the bot's code that's running.
        let instantiated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(pre.instantiate_async(&mut store))
        }));
        let (slot, agent, start_failure, start_spent) = match instantiated {
            Ok(result) => {
                let spent = start_fuel.saturating_sub(store.get_fuel().unwrap_or(0));
                match result {
                    Ok(agent) => (Slot::Idle(store), Some(Arc::new(agent)), None, spent),
                    Err(error) => (
                        Slot::Dead,
                        None,
                        Some(format!("in its start function: {error:#}")),
                        spent,
                    ),
                }
            }
            Err(_) => (
                Slot::Dead,
                None,
                Some("the seat crashed in its start function".into()),
                start_fuel,
            ),
        };
        Ok(Self {
            slot,
            agent,
            start_failure,
            start_spent,
            meter,
            memory,
            clock,
            logs: logs.map(|(out, err)| (out, err, 0, 0)),
            turn_fuel: settings.turn_fuel,
            init_fuel: settings.init_fuel,
            budget: settings.budget,
            total_spent: start_spent,
            debt: 0,
        })
    }

    /// `init`, with what's left of its fuel after the start functions. A
    /// bot whose start function failed reports that as `init`'s trap.
    fn init(&mut self, runtime: &tokio::runtime::Runtime) -> TurnReport {
        let start_spent = self.start_spent;
        let Some(agent) = self.agent.clone() else {
            let mut report = TurnReport {
                trapped: self.start_failure.clone(),
                ..TurnReport::default()
            };
            report.memory_bytes = self.memory.load(Ordering::Relaxed);
            report.wasm_fuel = start_spent;
            return report;
        };
        let allowance = self.init_fuel.saturating_sub(start_spent);
        if self.budget && allowance == 0 {
            // Its start function took all of `init`'s fuel: an overrun, so
            // `init` never runs and the bot plays no part.
            self.slot = Slot::Dead;
            return TurnReport {
                trapped: Some("its start function used all of init's fuel".into()),
                wasm_fuel: start_spent,
                memory_bytes: self.memory.load(Ordering::Relaxed),
                ..TurnReport::default()
            };
        }
        let mut report = self.run(runtime, 0, allowance, move |mut store| {
            Box::pin(async move {
                let result = agent.call_init(&mut store).await.map(|()| None);
                (store, result)
            })
        });
        report.wasm_fuel += start_spent;
        report
    }

    fn turn(&mut self, runtime: &tokio::runtime::Runtime, turn: u32, input: Vec<u8>) -> TurnReport {
        let Some(agent) = self.agent.clone() else {
            return self.finish(TurnReport::default(), None);
        };
        self.run(runtime, turn, self.turn_fuel, move |mut store| {
            Box::pin(async move {
                let result = agent.call_on_tick(&mut store, &input).await.map(Some);
                (store, result)
            })
        })
    }

    /// Give the bot `allowance` fuel this turn: finish any parked
    /// computation first, then make the new call with what's left.
    fn run(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        turn: u32,
        allowance: u64,
        make: impl FnOnce(Store<SeatState>) -> Call,
    ) -> TurnReport {
        let started = Instant::now();
        let allowance = if self.budget { allowance } else { UNBUDGETED };
        {
            let mut meter = self.meter.lock().expect("the meter");
            meter.turn = turn;
            meter.inference_fuel = 0;
            meter.inference_calls = 0;
        }
        let mut report = TurnReport::default();
        // An earlier overspend comes out of this turn first.
        let repaid = self.debt.min(allowance);
        self.debt -= repaid;
        let mut left = allowance - repaid;
        report.wasm_fuel = repaid;
        if left == 0 {
            report.paused = true;
            report.wall = started.elapsed();
            return self.finish(report, None);
        }
        match std::mem::replace(&mut self.slot, Slot::Dead) {
            Slot::Dead => {
                report.trapped = None;
                report.wall = started.elapsed();
                return self.finish(report, None);
            }
            Slot::Parked {
                call,
                spent_before,
                started_at,
            } => {
                let chunk = {
                    let mut meter = self.meter.lock().expect("the meter");
                    meter.call_total = started_at;
                    meter.clock_origin = self.total_spent.saturating_sub(spent_before);
                    meter.interval
                };
                match drive(runtime, call) {
                    Drive::Paused(call) => {
                        self.park(call, spent_before + chunk, started_at);
                        report.paused = true;
                        report.wasm_fuel += chunk.min(left);
                        report.wall = started.elapsed();
                        return self.finish(report, None);
                    }
                    Drive::Done(store, result) => {
                        let spent = started_at
                            .saturating_sub(store.get_fuel().unwrap_or(0))
                            .saturating_sub(spent_before);
                        if spent > left {
                            // It finished past this turn's fuel: no action,
                            // and the rest is owed.
                            self.debt += spent - left;
                            report.wasm_fuel += left;
                            report.paused = true;
                            self.slot = Slot::Idle(store);
                            report.wall = started.elapsed();
                            return self.finish(report, None);
                        }
                        left -= spent;
                        report.wasm_fuel += spent;
                        if let Err(error) = result {
                            report.trapped = Some(format!("{error:#}"));
                            report.wall = started.elapsed();
                            return self.finish(report, None);
                        }
                        // The stale result is discarded.
                        self.slot = Slot::Idle(store);
                    }
                }
            }
            idle @ Slot::Idle(_) => self.slot = idle,
        }
        if left == 0 {
            report.paused = true;
            report.wall = started.elapsed();
            return self.finish(report, None);
        }
        let Slot::Idle(mut store) = std::mem::replace(&mut self.slot, Slot::Dead) else {
            unreachable!("the slot is idle here");
        };
        let total = left + RESERVE;
        if let Err(error) = store
            .fuel_async_yield_interval(Some(left))
            .and_then(|()| store.set_fuel(total))
        {
            report.trapped = Some(format!("setting the bot's fuel: {error:#}"));
            return self.finish(report, None);
        }
        {
            let mut meter = self.meter.lock().expect("the meter");
            meter.reserve = RESERVE;
            meter.interval = left;
            meter.call_total = total;
            meter.clock_origin = self.total_spent + report.wasm_fuel;
        }
        let output = match drive(runtime, make(store)) {
            Drive::Paused(call) => {
                let chunk = left;
                self.park(call, chunk, total);
                report.paused = true;
                report.wasm_fuel += chunk;
                None
            }
            Drive::Done(store, result) => {
                let spent = total.saturating_sub(store.get_fuel().unwrap_or(total));
                self.slot = Slot::Idle(store);
                if spent > left {
                    // An operation charged in one go ran past the turn's
                    // fuel: no action, and the rest is owed.
                    self.debt += spent - left;
                    report.wasm_fuel += left;
                    report.paused = true;
                    report.wall = started.elapsed();
                    return self.finish(report, None);
                }
                report.wasm_fuel += spent;
                match result {
                    Ok(output) => output,
                    Err(error) => {
                        self.slot = Slot::Dead;
                        report.trapped = Some(format!("{error:#}"));
                        None
                    }
                }
            }
        };
        report.wall = started.elapsed();
        self.finish(report, output)
    }

    fn park(&mut self, call: Call, spent_before: u64, started_at: u64) {
        // Wasmtime refuelled the next chunk from the reserve before yielding.
        let mut meter = self.meter.lock().expect("the meter");
        meter.reserve = meter.reserve.saturating_sub(meter.interval);
        self.slot = Slot::Parked {
            call,
            spent_before,
            started_at,
        };
    }

    fn finish(&mut self, mut report: TurnReport, output: Option<Vec<u8>>) -> TurnReport {
        {
            let meter = self.meter.lock().expect("the meter");
            report.inference_fuel = meter.inference_fuel;
            report.inference_calls = meter.inference_calls;
        }
        report.memory_bytes = self.memory.load(Ordering::Relaxed);
        // Inference was charged out of the chunk: what the WASM itself spent
        // is the rest.
        report.wasm_fuel = report.wasm_fuel.saturating_sub(report.inference_fuel);
        self.total_spent += report.wasm_fuel + report.inference_fuel;
        self.clock.0.store(self.total_spent, Ordering::Relaxed);
        report.output = output;
        if let Some((out, err, out_at, err_at)) = &mut self.logs {
            report.logs = drain(out, out_at);
            report.logs.extend(drain(err, err_at));
        }
        report
    }
}

fn drain(pipe: &MemoryOutputPipe, at: &mut usize) -> Vec<String> {
    let contents = pipe.contents();
    let fresh = &contents[(*at).min(contents.len())..];
    // Only complete lines; a partial one waits for its end.
    let Some(end) = fresh.iter().rposition(|byte| *byte == b'\n') else {
        return Vec::new();
    };
    *at += end + 1;
    String::from_utf8_lossy(&fresh[..end])
        .lines()
        .map(|line| {
            if line.len() > LOG_LINE_MAX {
                let mut cut = LOG_LINE_MAX;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}…", &line[..cut])
            } else {
                line.to_string()
            }
        })
        .collect()
}
