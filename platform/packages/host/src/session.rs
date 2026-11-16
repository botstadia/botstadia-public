//! One session, turn by turn.
//!
//! The host never makes up an action. Each seat is a bot run here
//! ([`Seat::Bot`]) or an external one whose output the caller gives each turn
//! ([`Seat::External`]: `botstadia-train`'s Python policy, or the fuzzer's
//! extreme patterns). Every output, from either, goes through the platform's
//! one validity rule ([`botstadia_interface::payload::check_output`]): anything
//! outside the declaration is no action, never clamped.
//!
//! A session records its replay archive and metrics file when asked, the
//! platform's per-turn series beside the arena's own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use botstadia_interface::archive::{Archive, ArchiveHeader};
use botstadia_interface::bindings::host::arena::Turn;
use botstadia_interface::declaration::Declaration;
use botstadia_interface::metrics::{Line, Metrics, MetricsRecorder, SeriesTrack};
use botstadia_interface::payload;

use crate::arena::{ArenaInstance, LoadedArena};
use crate::budget::BudgetVersion;
use crate::seat::{BotSeat, Program, SeatLink, SeatSettings, TurnReport};
use crate::seed::{self, Seed};

/// Starts a seat whose turns its caller carries: in an enclave, a bot in
/// processes of its own. The error is the seat's failure to start before any
/// of its bot's code ran (`entrant-unavailable`); the report is `init`'s.
pub trait SpawnLink: Send + Sync {
    fn spawn(&self, seat: usize, settings: SeatSettings)
    -> Result<(Box<dyn SeatLink>, TurnReport)>;
}

/// Who sits in a seat.
#[derive(Clone)]
pub enum Seat {
    /// A bot, run by the host.
    Bot(Arc<Program>),
    /// A bot run elsewhere, through `link`, by `name`.
    Linked {
        name: String,
        link: Arc<dyn SpawnLink>,
    },
    /// The caller gives this seat's output every turn, by `name`.
    External { name: String },
}

impl Seat {
    pub fn name(&self) -> &str {
        match self {
            Self::Bot(program) => program.name(),
            Self::Linked { name, .. } | Self::External { name } => name,
        }
    }
}

/// How a session runs.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// Enforce fuel budgets. On in sessions; training may turn it off.
    pub budget: bool,
    /// Keep the replay archive and metrics.
    pub record: bool,
    /// Capture what bots print.
    pub bot_logs: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            budget: true,
            record: true,
            bot_logs: false,
        }
    }
}

/// A trap or error inside the arena: always the arena's bug, never a bot's.
#[derive(Debug)]
pub struct ArenaFault {
    pub turn: u32,
    pub message: String,
}

impl std::fmt::Display for ArenaFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the arena failed on turn {}: {}",
            self.turn, self.message
        )
    }
}

impl std::error::Error for ArenaFault {}

/// A seat that couldn't start before any of its bot's code ran: its WASI or
/// host linking, its model loads, its thread. Ours, never the bot's
/// (COMPETITION.md#rules-for-failures, `entrant-unavailable`). From the bot's
/// first instruction on, its start function included, a failure is the
/// bot's and the session goes on without it.
#[derive(Debug)]
pub struct EntrantUnavailable {
    pub seat: usize,
    pub message: String,
}

impl std::fmt::Display for EntrantUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "seat {} couldn't start: {}", self.seat, self.message)
    }
}

impl std::error::Error for EntrantUnavailable {}

/// What one seat did on one turn.
#[derive(Debug, Clone, Default)]
pub struct SeatTurn {
    /// Whether its output was valid; `false` means no action.
    pub acted: bool,
    /// The output it gave, when it was valid.
    pub output: Option<Vec<u8>>,
    /// Why there was no action, when there wasn't.
    pub why_not: Option<String>,
    pub wasm_fuel: u64,
    pub inference_fuel: u64,
    pub wall: Duration,
    pub paused: bool,
    pub trapped: Option<String>,
    /// It gave an output that broke the declaration (shape, type, bounds or
    /// NaN), so it had no action.
    pub rejected: bool,
    /// All its linear memories together, as its limits have let them grow, in bytes
    /// (a growth the allocator then refused still counts).
    pub memory_bytes: u64,
    /// The input it was given and the output it gave (valid or not), bytes.
    pub input_bytes: u32,
    pub output_bytes: u32,
    pub logs: Vec<String>,
}

/// What a turn produced, for the caller.
#[derive(Debug, Clone)]
pub struct StepOut {
    /// The turn just played; 0 is setup.
    pub turn: u32,
    /// Every seat's input for the next turn.
    pub inputs: Vec<Vec<u8>>,
    /// Per seat, for training only.
    pub shaping: Vec<f32>,
    pub seats: Vec<SeatTurn>,
    /// Set once the session has ended.
    pub finish: Option<Finish>,
    /// The arena's own cost this turn (setup's for turn 0): its wall time,
    /// and its fuel when the arena is metered.
    pub arena_wall: Duration,
    pub arena_fuel: Option<u64>,
}

/// How the session ended.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Finish {
    pub places: Vec<Vec<u32>>,
    pub decided_by: String,
    pub stats: Vec<Vec<f64>>,
}

impl Finish {
    /// Seat `seat`'s result: 1 for a win (sole first), 0 for a shared first or
    /// a middle place, -1 for last, scaled across the field.
    pub fn outcome(&self, seat: u32) -> f32 {
        let places = self.places.len();
        let Some(place) = self.places.iter().position(|p| p.contains(&seat)) else {
            return 0.0;
        };
        if places <= 1 {
            return 0.0;
        }
        1.0 - 2.0 * place as f32 / (places - 1) as f32
    }
}

enum Runner {
    Bot(Box<dyn SeatLink>),
    External,
}

/// One running session.
pub struct Session {
    arena: ArenaInstance,
    declaration: Arc<Declaration>,
    arena_digest: String,
    seats: Vec<Seat>,
    runners: Vec<Runner>,
    /// Seats that trapped: inactive for the rest of the session.
    inactive: Vec<bool>,
    inputs: Vec<Vec<u8>>,
    turn: u32,
    finish: Option<Finish>,
    options: SessionOptions,
    budget: &'static BudgetVersion,
    recorder: Option<Recorder>,
    /// The init reports, kept for the caller.
    pub init: Vec<SeatTurn>,
    /// The slowest arena step so far, and setup's time.
    pub slowest_step: Duration,
    pub setup_time: Duration,
    pub engine_time: Duration,
    /// Arena fuel, when the arena is metered: setup's, the costliest turn's
    /// (its `step` and `finished` together) and the total.
    pub setup_fuel: Option<u64>,
    pub worst_step_fuel: Option<u64>,
    pub engine_fuel: Option<u64>,
}

struct Recorder {
    metrics: MetricsRecorder,
    frames: Vec<Vec<u8>>,
    no_action: Vec<Line>,
    wasm_fuel: Vec<Line>,
    inference_fuel: Vec<Line>,
    wall_ms: Vec<Line>,
    paused: Vec<Line>,
    rejected: Vec<Line>,
    trapped: Vec<Line>,
    memory_kib: Vec<Line>,
    input_bytes: Vec<Line>,
    output_bytes: Vec<Line>,
    /// The arena's own fuel per turn, when metered.
    arena_fuel: Line,
}

/// The arena owes one input per seat, every turn: a short or long list is
/// its bug, never a host panic.
fn one_input_per_seat(inputs: &[Vec<u8>], seats: usize, turn: u32) -> Result<()> {
    if inputs.len() != seats {
        return Err(anyhow!(ArenaFault {
            turn,
            message: format!("{} inputs for {seats} seats", inputs.len()),
        }));
    }
    Ok(())
}

impl Session {
    /// Seat everyone, run setup and every bot's `init`. `seed` is the
    /// session's; seats get their own streams of it.
    pub fn start(
        arena: &LoadedArena,
        seats: Vec<Seat>,
        seed: &Seed,
        budget: &'static BudgetVersion,
        options: SessionOptions,
    ) -> Result<(Self, StepOut)> {
        let declaration = arena.declaration.clone();
        let count = seats.len() as u32;
        if !declaration.entrants.accepts(count) {
            bail!(
                "{} takes {}, not {count}",
                declaration.arena_id,
                match declaration.entrants {
                    botstadia_interface::declaration::Entrants::Exact(n) =>
                        format!("exactly {n} seats"),
                    botstadia_interface::declaration::Entrants::Range { min, max } =>
                        format!("{min} to {max} seats"),
                }
            );
        }
        let mut instance = arena.instantiate(&seed::arena(seed))?;
        let started = Instant::now();
        let first: Turn = instance
            .setup(&seed::arena(seed), count)
            .map_err(|error| {
                anyhow!(ArenaFault {
                    turn: 0,
                    message: format!("{error:#}")
                })
            })?
            .map_err(|message| {
                anyhow!(ArenaFault {
                    turn: 0,
                    message: format!("setup refused: {message}")
                })
            })?;
        one_input_per_seat(&first.inputs, count as usize, 0)?;
        let setup_time = started.elapsed();
        let setup_fuel = instance.take_fuel();

        // Every bot starts, and runs `init`, at once. A linked seat starts
        // inline: whoever carries it decides what runs at once, and its
        // caller may start no threads (an enclave's filtered arena process).
        type Started = Result<(Box<dyn SeatLink>, TurnReport)>;
        enum Starting {
            Thread(std::thread::JoinHandle<Started>),
            Done(Started),
            External,
        }
        let mut runners = Vec::new();
        let mut init = Vec::new();
        let mut starting = Vec::new();
        for (index, seat) in seats.iter().enumerate() {
            let settings = SeatSettings {
                turn_fuel: declaration.budgets.turn_fuel,
                init_fuel: declaration.budgets.init_fuel,
                budget: options.budget,
                logs: options.bot_logs,
                stream: seed::seat(seed, index),
            };
            match seat {
                Seat::Bot(program) => {
                    let program = program.clone();
                    let declaration_json = arena.declaration_json.clone();
                    starting.push(Starting::Thread(std::thread::spawn(move || {
                        BotSeat::spawn(program, declaration_json, settings)
                            .map(|(seat, init)| (Box::new(seat) as Box<dyn SeatLink>, init))
                    })));
                }
                Seat::Linked { link, .. } => {
                    starting.push(Starting::Done(link.spawn(index, settings)));
                }
                Seat::External { .. } => starting.push(Starting::External),
            }
        }
        let mut inactive = vec![false; seats.len()];
        for (index, start) in starting.into_iter().enumerate() {
            let started = match start {
                Starting::External => {
                    init.push(SeatTurn::default());
                    runners.push(Runner::External);
                    continue;
                }
                Starting::Thread(handle) => handle
                    .join()
                    .map_err(|_| anyhow!("seat {index} panicked while starting"))?,
                Starting::Done(started) => started,
            };
            let (seat, report) = started.map_err(|error| {
                anyhow!(EntrantUnavailable {
                    seat: index,
                    message: format!("{error:#}"),
                })
            })?;
            if report.trapped.is_some() {
                inactive[index] = true;
            }
            init.push(seat_turn(&report, true, None));
            runners.push(Runner::Bot(seat));
        }

        let recorder = options.record.then(|| Recorder::new(&declaration, count));
        let mut session = Self {
            arena: instance,
            declaration: declaration.clone(),
            arena_digest: arena.digest.clone(),
            seats,
            runners,
            inactive,
            inputs: first.inputs.clone(),
            turn: 0,
            finish: None,
            options,
            budget,
            recorder,
            init,
            slowest_step: Duration::ZERO,
            setup_time,
            engine_time: setup_time,
            setup_fuel,
            worst_step_fuel: setup_fuel.map(|_| 0),
            engine_fuel: setup_fuel,
        };
        let finish = session.check_finished()?;
        // A `finished` call at setup counts as setup's.
        let late = session.arena.take_fuel();
        session.setup_fuel = sum(session.setup_fuel, late);
        session.engine_fuel = sum(session.engine_fuel, late);
        if let Some(recorder) = &mut session.recorder {
            recorder.turn(0, &first, &[], session.setup_fuel)?;
            // A bot that trapped in `init` is out from the start.
            for (index, report) in session.init.iter().enumerate() {
                if report.trapped.is_some() {
                    recorder.trapped[index].points.push((0, 1.0));
                }
            }
        }
        let out = StepOut {
            turn: 0,
            inputs: first.inputs,
            shaping: first.shaping,
            seats: session.init.clone(),
            finish,
            arena_wall: setup_time,
            arena_fuel: session.setup_fuel,
        };
        Ok((session, out))
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }

    pub fn finished(&self) -> Option<&Finish> {
        self.finish.as_ref()
    }

    pub fn seat_names(&self) -> Vec<String> {
        self.seats
            .iter()
            .map(|seat| seat.name().to_string())
            .collect()
    }

    /// Play one turn. `external[seat]` is an external seat's output (ignored
    /// for bot seats); `None` is no action.
    pub fn step(&mut self, external: &[Option<Vec<u8>>]) -> Result<StepOut> {
        if self.finish.is_some() {
            bail!("the session has ended");
        }
        if self.turn > self.declaration.max_turns {
            return Err(anyhow!(ArenaFault {
                turn: self.turn,
                message: "no result after maximum turns".into(),
            }));
        }
        self.turn += 1;
        let turn = self.turn;
        // Every bot thinks at once.
        for (index, runner) in self.runners.iter_mut().enumerate() {
            if let Runner::Bot(seat) = runner
                && !self.inactive[index]
            {
                seat.send(turn, self.inputs[index].clone());
            }
        }
        let schema = self.declaration.payload_schema_version;
        let mut outputs = Vec::with_capacity(self.runners.len());
        let mut reports = Vec::with_capacity(self.runners.len());
        for (index, runner) in self.runners.iter_mut().enumerate() {
            let (raw, report) = match runner {
                Runner::Bot(_) if self.inactive[index] => (None, TurnReport::default()),
                Runner::Bot(seat) => {
                    let report = seat.collect();
                    (report.output.clone(), report)
                }
                Runner::External => (
                    external.get(index).cloned().flatten(),
                    TurnReport::default(),
                ),
            };
            // An inactive bot was sent nothing this turn (one that traps now was
            // sent its input).
            let input_bytes = if self.inactive[index] && matches!(runner, Runner::Bot(_)) {
                0
            } else {
                self.inputs.get(index).map_or(0, Vec::len) as u32
            };
            if report.trapped.is_some() {
                self.inactive[index] = true;
            }
            let output_bytes = raw.as_ref().map_or(0, Vec::len) as u32;
            let mut rejected = false;
            let (valid, why_not) = match raw {
                None => (
                    None,
                    Some(if report.paused {
                        "out of fuel".to_string()
                    } else if let Some(trap) = &report.trapped {
                        format!("trapped: {trap}")
                    } else if self.inactive[index] {
                        "inactive".into()
                    } else {
                        "no output".into()
                    }),
                ),
                Some(bytes) => {
                    match payload::check_output(schema, &self.declaration.outputs, &bytes) {
                        Ok(_) => (Some(bytes), None),
                        Err(error) => {
                            rejected = true;
                            (None, Some(error.to_string()))
                        }
                    }
                }
            };
            let mut turn_report = seat_turn(&report, valid.is_some(), why_not);
            turn_report.output = valid.clone();
            turn_report.rejected = rejected;
            turn_report.input_bytes = input_bytes;
            turn_report.output_bytes = output_bytes;
            reports.push(turn_report);
            outputs.push(valid);
        }
        let began = Instant::now();
        let next = self.arena.step(&outputs).map_err(|error| {
            anyhow!(ArenaFault {
                turn,
                message: format!("{error:#}")
            })
        })?;
        let stepped = began.elapsed();
        if next.frame.len() > self.declaration.wire.max_frame_bytes as usize {
            return Err(anyhow!(ArenaFault {
                turn,
                message: format!(
                    "frame of {} bytes is over the declared {}",
                    next.frame.len(),
                    self.declaration.wire.max_frame_bytes
                ),
            }));
        }
        one_input_per_seat(&next.inputs, self.seats.len(), turn)?;
        let checked = Instant::now();
        let finish = self.check_finished()?;
        let took = stepped + checked.elapsed();
        let fuel = self.arena.take_fuel();
        self.engine_time += took;
        self.slowest_step = self.slowest_step.max(took);
        self.engine_fuel = sum(self.engine_fuel, fuel);
        self.worst_step_fuel = match (self.worst_step_fuel, fuel) {
            (Some(worst), Some(fuel)) => Some(worst.max(fuel)),
            (worst, _) => worst,
        };
        if let Some(recorder) = &mut self.recorder {
            recorder.turn(turn, &next, &reports, fuel)?;
        }
        self.inputs = next.inputs.clone();
        Ok(StepOut {
            turn,
            inputs: next.inputs,
            shaping: next.shaping,
            seats: reports,
            finish,
            arena_wall: took,
            arena_fuel: fuel,
        })
    }

    fn check_finished(&mut self) -> Result<Option<Finish>> {
        let turn = self.turn;
        let finish = self.arena.finished().map_err(|error| {
            anyhow!(ArenaFault {
                turn,
                message: format!("{error:#}")
            })
        })?;
        self.finish = finish.map(|finish| Finish {
            places: finish.places,
            decided_by: finish.decided_by,
            stats: finish.stats,
        });
        Ok(self.finish.clone())
    }

    /// The replay archive and metrics file, once the session has ended.
    pub fn record(self) -> Result<(Archive, Metrics)> {
        let finish = self
            .finish
            .clone()
            .ok_or_else(|| anyhow!("the session hasn't ended"))?;
        let recorder = self
            .recorder
            .ok_or_else(|| anyhow!("the session wasn't recorded"))?;
        let names: Vec<String> = self
            .seats
            .iter()
            .map(|seat| seat.name().to_string())
            .collect();
        let archive = Archive {
            header: ArchiveHeader {
                arena_id: self.declaration.arena_id.clone(),
                arena_version: self.declaration.arena_version.clone(),
                arena_digest: self.arena_digest.clone(),
                payload_schema_version: self.declaration.payload_schema_version,
                seats: names,
                control_rate: self.declaration.control_rate,
                turns: recorder.frames.len() as u32,
            },
            frames: recorder.frames.clone(),
        };
        let metrics =
            recorder.finish(&self.declaration, self.budget, self.options.budget, finish)?;
        Ok((archive, metrics))
    }
}

fn seat_turn(report: &TurnReport, acted: bool, why_not: Option<String>) -> SeatTurn {
    SeatTurn {
        acted,
        output: None,
        why_not,
        wasm_fuel: report.wasm_fuel,
        inference_fuel: report.inference_fuel,
        wall: report.wall,
        paused: report.paused,
        trapped: report.trapped.clone(),
        rejected: false,
        memory_bytes: report.memory_bytes,
        input_bytes: 0,
        output_bytes: 0,
        logs: report.logs.clone(),
    }
}

fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    }
}

fn push_change(line: &mut Line, turn: u32, value: f32) {
    if line
        .points
        .last()
        .is_none_or(|(_, last)| last.to_bits() != value.to_bits())
    {
        line.points.push((turn, value));
    }
}

impl Recorder {
    fn new(declaration: &Declaration, seats: u32) -> Self {
        let lines = || -> Vec<Line> {
            (0..seats)
                .map(|seat| Line {
                    seat: Some(seat),
                    points: Vec::new(),
                })
                .collect()
        };
        Self {
            metrics: MetricsRecorder::new(declaration, seats),
            frames: Vec::new(),
            no_action: lines(),
            wasm_fuel: lines(),
            inference_fuel: lines(),
            wall_ms: lines(),
            paused: lines(),
            rejected: lines(),
            trapped: lines(),
            memory_kib: lines(),
            input_bytes: lines(),
            output_bytes: lines(),
            arena_fuel: Line {
                seat: None,
                points: Vec::new(),
            },
        }
    }

    fn turn(
        &mut self,
        turn: u32,
        out: &Turn,
        seats: &[SeatTurn],
        arena_fuel: Option<u64>,
    ) -> Result<()> {
        if let Some(fuel) = arena_fuel {
            push_change(&mut self.arena_fuel, turn, fuel as f32);
        }
        let events: Vec<(u32, Option<u32>)> = out
            .events
            .iter()
            .map(|event| (event.index, event.seat))
            .collect();
        self.metrics
            .turn(turn, &out.series, &events)
            .map_err(|message| anyhow!("turn {turn}: {message}"))?;
        self.frames.push(out.frame.clone());
        for (index, seat) in seats.iter().enumerate() {
            push_change(
                &mut self.no_action[index],
                turn,
                if seat.acted { 0.0 } else { 1.0 },
            );
            push_change(&mut self.wasm_fuel[index], turn, seat.wasm_fuel as f32);
            push_change(
                &mut self.inference_fuel[index],
                turn,
                seat.inference_fuel as f32,
            );
            push_change(
                &mut self.paused[index],
                turn,
                if seat.paused { 1.0 } else { 0.0 },
            );
            push_change(
                &mut self.rejected[index],
                turn,
                if seat.rejected { 1.0 } else { 0.0 },
            );
            // A trap ends the bot: 1 from the turn it trapped on.
            let trapped = seat.trapped.is_some()
                || self.trapped[index]
                    .points
                    .last()
                    .is_some_and(|(_, value)| *value > 0.0);
            push_change(
                &mut self.trapped[index],
                turn,
                if trapped { 1.0 } else { 0.0 },
            );
            // Memory only grows, and only sometimes: on change, in KiB. A seat
            // that ran nothing this turn reports none, so keep its last.
            if seat.memory_bytes > 0 {
                push_change(
                    &mut self.memory_kib[index],
                    turn,
                    (seat.memory_bytes / 1024) as f32,
                );
            }
            push_change(&mut self.input_bytes[index], turn, seat.input_bytes as f32);
            push_change(
                &mut self.output_bytes[index],
                turn,
                seat.output_bytes as f32,
            );
            // Wall time is noisy: record it at a tenth of a millisecond.
            let ms = (seat.wall.as_secs_f32() * 10_000.0).round() / 10.0;
            push_change(&mut self.wall_ms[index], turn, ms);
        }
        Ok(())
    }

    fn finish(
        mut self,
        declaration: &Declaration,
        budget: &BudgetVersion,
        enforced: bool,
        finish: Finish,
    ) -> Result<Metrics> {
        for (key, lines) in [
            ("no_action", self.no_action),
            ("wasm_fuel", self.wasm_fuel),
            ("inference_fuel", self.inference_fuel),
            ("paused", self.paused),
            ("wall_ms", self.wall_ms),
            ("rejected", self.rejected),
            ("trapped", self.trapped),
            ("memory_kib", self.memory_kib),
            ("input_bytes", self.input_bytes),
            ("output_bytes", self.output_bytes),
        ] {
            self.metrics.platform(SeriesTrack {
                key: key.into(),
                lines,
            });
        }
        if !self.arena_fuel.points.is_empty() {
            self.metrics.platform(SeriesTrack {
                key: "arena_fuel".into(),
                lines: vec![self.arena_fuel],
            });
        }
        let mut limits = vec![
            format!("Budget version {}", budget.id),
            format!("Fuel per turn: {}", declaration.budgets.turn_fuel),
            format!("Fuel for init: {}", declaration.budgets.init_fuel),
            format!("Bot memory: {} MiB", budget.wasm_memory_bytes >> 20),
            format!(
                "Control rate: {} turns per second",
                declaration.control_rate
            ),
        ];
        if !enforced {
            limits.insert(0, "Local run: fuel budgets not enforced".into());
        }
        self.metrics.limits(limits);
        self.metrics
            .finish(declaration, finish.places, finish.decided_by, finish.stats)
            .map_err(|message| anyhow!(message))
    }
}
