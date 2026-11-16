//! The runner's workers (BOTS.md, isolation): every seat runs in processes
//! of its own, its Wasmtime instance in one and its ONNX Runtime in
//! another, and the arena steps in a third, all holding no key. Each is
//! `botstadia-runner worker <kind>`, started for one job and ended with it.
//!
//! A worker pins itself to the job's core, isolates itself (namespaces),
//! caps its memory and installs its syscall filter before it reads anything
//! a bundle or an action controls. Its only channel is its stdin and stdout
//! to the runner, in [`frame`]s. The runner ([`crate::keyholder`]) relays
//! bodies it doesn't parse, times the seats and enforces the backstop.

pub mod arena;
pub mod frame;
pub mod inference;
pub mod seat;
pub mod verify;

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};

pub use frame::Frame;

/// A worker's end of its pipe to the runner.
pub type Reader = Box<dyn std::io::Read + Send>;
pub type Writer = Box<dyn std::io::Write + Send>;

/// Frame kinds. To a seat's Wasmtime process:
pub const SEAT_START: u8 = 1;
pub const SEAT_TURN: u8 = 2;
pub const SEAT_OUTPUTS: u8 = 3;
/// A static check instead of a seat: unzip, manifest, compile, preflight,
/// answered with `SEAT_STARTED` (an empty report, or why it failed). The
/// anchor-time recheck (BOTS.md#budget-versions).
pub const SEAT_CHECK: u8 = 5;
/// To the verification worker, and its report back.
pub const VERIFY_START: u8 = 60;
pub const VERIFY_REPORT: u8 = 61;
/// The runner's word that the seat's models are loaded (body: one byte, 1
/// loaded, 0 its inference has ended), so its bot's code starts only then.
pub const SEAT_LOADED: u8 = 4;
/// From a seat's Wasmtime process:
pub const SEAT_STARTED: u8 = 10;
pub const SEAT_MODELS: u8 = 11;
pub const SEAT_INFER: u8 = 12;
pub const SEAT_REPORT: u8 = 13;
/// Its bot's first instruction is next: from here a failure is the bot's.
pub const SEAT_BEGIN: u8 = 14;
/// To a seat's inference process:
pub const INF_LOAD: u8 = 20;
pub const INF_RUN: u8 = 21;
pub const INF_CANCEL: u8 = 22;
/// From a seat's inference process:
pub const INF_LOADED: u8 = 30;
pub const INF_OUTPUTS: u8 = 31;
/// To the arena process:
pub const ARENA_START: u8 = 40;
pub const ARENA_SEAT_STARTED: u8 = 41;
pub const ARENA_REPORT: u8 = 42;
/// From the arena process:
pub const ARENA_SPAWN: u8 = 50;
pub const ARENA_TURN: u8 = 51;
pub const ARENA_DONE: u8 = 52;

/// Which worker a process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Seat,
    Inference,
    Arena,
    Verify,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Seat => "seat",
            Kind::Inference => "inference",
            Kind::Arena => "arena",
            Kind::Verify => "verify",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [Kind::Seat, Kind::Inference, Kind::Arena, Kind::Verify]
            .into_iter()
            .find(|k| k.name() == name)
    }

    /// The process's memory cap (`RLIMIT_DATA`, which counts writable
    /// private mappings, so Wasmtime's reserved but inaccessible address
    /// space doesn't count), per budget version.
    pub fn memory_cap(self, budget: &botstadia_host::budget::BudgetVersion) -> u64 {
        match self {
            // The bot's WASM memory, its compiled code, the unzipped bundle
            // and Wasmtime's own: the seat process's cap.
            // At its start it holds the unzipped bundle and, for a moment,
            // one encoding of its models to send on: twice the bundle.
            Kind::Seat => {
                budget.wasm_memory_bytes as u64 + 2 * budget.uncompressed_bundle_bytes + (256 << 20)
            }
            // All the seat's models together are bound by preflight
            // (`inference_memory_bytes`, per seat); while they load, their
            // encoding as received and its decoded copy (each at most the
            // unzipped bundle), and ONNX Runtime's own runtime, on top.
            Kind::Inference => {
                budget.inference_memory_bytes + 2 * budget.uncompressed_bundle_bytes + (256 << 20)
            }
            Kind::Arena => 2 << 30,
            // The bot's seat, its models and the arena in one process: never
            // less than the session's processes allow each, so a bot that
            // passes verification never fails a session's cap.
            Kind::Verify => {
                Kind::Seat.memory_cap(budget)
                    + Kind::Inference.memory_cap(budget)
                    + Kind::Arena.memory_cap(budget)
            }
        }
    }
}

/// The caps of everything a session of `seats` runs at once: each seat's
/// Wasmtime and inference processes, and the arena's; and in the runner,
/// which holds the keys, each seat's models kept for a restart plus one more
/// copy while one is sent on.
pub fn session_memory(budget: &botstadia_host::budget::BudgetVersion, seats: usize) -> u64 {
    let seats = seats as u64;
    seats * (Kind::Seat.memory_cap(budget) + Kind::Inference.memory_cap(budget))
        + Kind::Arena.memory_cap(budget)
        + (seats + 1) * budget.uncompressed_bundle_bytes
}

/// A seat's start: what the runner adds (the bundle's plaintext, the budget
/// version, the declaration) around the arena's settings for the seat,
/// which it passes on unread.
#[derive(Debug, Serialize, Deserialize)]
pub struct SeatStart {
    pub budget: u32,
    pub bundle: Vec<u8>,
    pub declaration: Vec<u8>,
    /// `SeatSettings`, as the arena process encoded them.
    pub settings: Vec<u8>,
}

/// One model for the inference process: its name, bytes and preflight.
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelToLoad {
    pub name: String,
    pub bytes: Vec<u8>,
    pub info: botstadia_host::onnx::preflight::ModelInfo,
}

/// A static check: the bundle against the arena's turn fuel.
#[derive(Debug, Serialize, Deserialize)]
pub struct SeatCheck {
    pub bundle: Vec<u8>,
    pub turn_fuel: u64,
}

/// Verification's start: the bundle, its arena and the timing run's seed.
#[derive(Debug, Serialize, Deserialize)]
pub struct VerifyStart {
    pub bundle: Vec<u8>,
    pub arena: Vec<u8>,
    pub seed: [u8; 32],
}

/// The arena process's start.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArenaStart {
    pub arena: Vec<u8>,
    pub budget: u32,
    pub seed: [u8; 32],
    /// The seats' names, in seat order.
    pub names: Vec<String>,
}

/// The arena process's answer: the session's end, or how it failed.
#[derive(Debug, Serialize, Deserialize)]
pub enum ArenaDone {
    Finished {
        /// Places by seat.
        places: Vec<Vec<u32>>,
        archive: Vec<u8>,
        metrics: Vec<u8>,
        /// Seats that trapped, with the turn: the deferred report rule's
        /// incidents.
        trapped: Vec<(u32, u32, String)>,
        /// The arena's fuel over the session, for the record.
        arena_fuel: u64,
    },
    /// The arena trapped or broke a rule of the platform.
    ArenaFault(String),
    /// A seat couldn't start before any of its bot's code ran.
    EntrantUnavailable(u32),
    /// Anything else of ours.
    Failed(String),
}

/// How to start a worker: the runner binary, which `worker <kind>` turns
/// into one.
#[derive(Debug, Clone)]
pub struct Spawner {
    pub exe: PathBuf,
    /// The job's core, where the platform pins.
    pub core: Option<usize>,
    /// Whether isolation must hold: always in an image. Locally (macOS) the
    /// workers run unisolated and say so.
    pub strict: bool,
}

impl Spawner {
    /// This runner, for the job on `core`.
    pub fn this(core: Option<usize>) -> anyhow::Result<Self> {
        Ok(Self {
            exe: std::env::current_exe()?,
            core,
            strict: cfg!(botstadia_image),
        })
    }

    pub fn spawn(&self, kind: Kind, budget: u32) -> anyhow::Result<Worker> {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("worker")
            .arg(kind.name())
            .arg(budget.to_string())
            .arg(self.core.map_or("-".to_string(), |c| c.to_string()))
            .arg(if self.strict { "strict" } else { "lenient" })
            .env_clear()
            // Only where its libraries are, when the runner was told.
            .envs(std::env::var_os("LD_LIBRARY_PATH").map(|v| ("LD_LIBRARY_PATH", v)))
            .envs(std::env::var_os("BOTSTADIA_SECCOMP_TRAP").map(|v| ("BOTSTADIA_SECCOMP_TRAP", v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        Ok(Worker {
            kind,
            child,
            to: BufWriter::new(stdin),
            from: Some(BufReader::new(stdout)),
        })
    }
}

/// A running worker, from the runner's side.
pub struct Worker {
    pub kind: Kind,
    pub child: Child,
    pub to: BufWriter<ChildStdin>,
    /// Taken by the reader thread that relays its frames.
    pub from: Option<BufReader<ChildStdout>>,
}

impl Worker {
    pub fn send(&mut self, frame: &Frame) -> std::io::Result<()> {
        frame::write(&mut self.to, frame)
    }

    /// Kill it now (it's ours to end, whatever it's doing).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// How it ended, if it has: a signal (a crash, or a refused syscall's
    /// SIGSYS) is a possible seat escape, reported under the deferred report
    /// rule.
    ///
    /// Asked once its output has closed: a dying process closes its files
    /// before it can be reaped, so its status is waited for a moment (a
    /// worker that closed its output and lives on is killed by the caller,
    /// and says nothing).
    pub fn ended(&mut self) -> Option<String> {
        self.ended_within(50)
    }

    /// How it ended, if it already has (asked of a worker the caller is
    /// about to kill).
    pub fn ended_now(&mut self) -> Option<String> {
        self.ended_within(1)
    }

    fn ended_within(&mut self, tries: u32) -> Option<String> {
        for _ in 0..tries {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(describe(status)),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.kill();
    }
}

fn describe(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return match signal {
                31 => "a refused syscall (SIGSYS)".into(),
                9 => "killed".into(),
                s => format!("signal {s}"),
            };
        }
    }
    format!("exit {}", status.code().unwrap_or(-1))
}

/// A worker's own setup, before it reads anything a bundle controls: its
/// core, its namespaces and its memory cap. The syscall filter comes last,
/// from the worker itself, once everything it needs to start is loaded.
fn confine(kind: Kind, budget: u32, core: Option<usize>, strict: bool) -> anyhow::Result<()> {
    let version = botstadia_host::budget::version(budget)
        .ok_or_else(|| anyhow::anyhow!("no budget version {budget}"))?;
    let mut problems = Vec::new();
    if let Some(core) = core
        && let Err(e) = botstadia_sandbox::pin(&[core])
    {
        problems.push(format!("pin: {e}"));
    }
    // Namespaces need a privilege a container may lack. They're defence in
    // depth: the filter alone already refuses every socket and file open,
    // so without them the worker says so and goes on.
    if let Err(e) = botstadia_sandbox::isolate() {
        eprintln!(
            "worker {}: no namespaces here ({e}); the filter holds network and files",
            kind.name()
        );
    }
    if let Err(e) = botstadia_sandbox::cap_memory(kind.memory_cap(version)) {
        problems.push(format!("memory cap: {e}"));
    }
    if !problems.is_empty() {
        anyhow::ensure!(
            !strict,
            "{} can't be confined: {}",
            kind.name(),
            problems.join("; ")
        );
        eprintln!(
            "worker {}: unisolated here ({})",
            kind.name(),
            problems.join("; ")
        );
    }
    Ok(())
}

/// The filter, last of all: in strict mode it must hold.
fn filter(kind: Kind, strict: bool) -> anyhow::Result<()> {
    let list = match kind {
        Kind::Seat | Kind::Arena => botstadia_sandbox::Filter::Seat,
        Kind::Inference => botstadia_sandbox::Filter::Inference,
        Kind::Verify => botstadia_sandbox::Filter::Threaded,
    };
    match botstadia_sandbox::seccomp_for(list) {
        Ok(()) => Ok(()),
        Err(e) if !strict => {
            eprintln!("worker {}: no syscall filter here ({e})", kind.name());
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// `botstadia-runner worker <kind> <budget> <core|-> <strict|lenient>`: run
/// as that worker until the runner closes its stdin.
pub fn main(args: &[String]) -> anyhow::Result<()> {
    let [kind, budget, core, mode] = args else {
        anyhow::bail!("usage: worker <kind> <budget> <core|-> <strict|lenient>");
    };
    let kind = Kind::parse(kind).ok_or_else(|| anyhow::anyhow!("no worker kind {kind}"))?;
    let budget: u32 = budget.parse()?;
    let core = (core != "-").then(|| core.parse()).transpose()?;
    let strict = mode == "strict";
    confine(kind, budget, core, strict)?;
    let r: Reader = Box::new(BufReader::new(std::io::stdin()));
    let w: Writer = Box::new(BufWriter::new(std::io::stdout()));
    let filter = move || filter(kind, strict);
    match kind {
        Kind::Seat => seat::run(budget, r, w, &filter),
        Kind::Inference => inference::run(budget, r, w, &filter),
        Kind::Arena => arena::run(budget, r, w, &filter),
        Kind::Verify => verify::run(budget, r, w, &filter),
    }
}
