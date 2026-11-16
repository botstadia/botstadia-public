//! Playing a session with every seat and the arena in processes of their
//! own (BOTS.md, isolation; "One seat at a time per core"; "The backstop";
//! "The safety limit").
//!
//! The runner is the keyholder. Here it only spawns the workers, relays
//! frames between them by their headers (bodies it doesn't parse), times
//! each seat's turn in canary ticks, runs one seat at a time on the job's
//! core, and enforces the backstop, the kill grace, the inference restart
//! and the safety limit. The one body it decodes is the arena's final
//! answer, and an inference process's load result, each under a size limit.

use std::collections::VecDeque;
use std::io::BufReader;
use std::process::ChildStdout;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use botstadia_chain::outcome::{Outcome, VoidReason};
use botstadia_host::LoadedArena;
use botstadia_host::seat::{InferFailure, TurnReport};

use crate::canary::{Ticks, calibration as cal};
use crate::hold::Held;
use crate::play::{LIMITS, Played};
use crate::worker::frame::{self, Frame, MAX_FRAME, decode, encode};
use crate::worker::*;

/// What a job plays with: the runner's workers and the canary's clock.
#[derive(Clone)]
pub struct Isolation {
    pub spawner: Spawner,
    pub clock: Ticks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Arena,
    Seat(u32),
    /// A seat's inference process, by its generation: a restart's old
    /// process still reports its end after the new one exists, and must
    /// never be taken for it.
    Inference(u32, u32),
}

enum Event {
    Frame(Source, Frame),
    Ended(Source),
}

fn read_from(source: Source, mut from: BufReader<ChildStdout>, tx: Sender<Event>) {
    std::thread::spawn(move || {
        while let Ok(Some(f)) = frame::read(&mut from) {
            if tx.send(Event::Frame(source, f)).is_err() {
                return;
            }
        }
        let _ = tx.send(Event::Ended(source));
    });
}

/// One seat's processes and where it is.
#[derive(Default)]
struct SeatState {
    wasm: Option<Worker>,
    inference: Option<Worker>,
    /// The generation of `inference`, counted up at each spawn.
    inference_gen: u32,
    /// Its models as its Wasmtime process sent them, unread, for a restart.
    models: Option<Vec<u8>>,
    restarts_left: u32,
    /// Turns it has overrun its backstop: a second puts it out for the
    /// session, so no bot can stall turn after turn on the kill grace.
    overruns: u32,
    inference_over: bool,
    /// Its bot's code has started: from here a failure is the bot's.
    began: bool,
    started: bool,
    dead: bool,
    /// Why its process is gone, for every turn it then plays with no action.
    gone: Option<String>,
    start_at: u64,
    /// Ticks its start spent waiting on its models' load (outside its time).
    start_paused: u64,
    load_at: Option<u64>,
    /// Its models' first load is under way: the seat waits on its answer,
    /// which a crash or the load ceiling gives too (the bot's failure: every
    /// `infer` errors from then on).
    first_load: bool,
    reloading: Option<u64>,
    infer_at: Option<u64>,
    cancel_at: Option<u64>,
    /// An `infer` waiting on a reload.
    held_infer: Option<Vec<u8>>,
}

struct Turn {
    seat: u32,
    since: u64,
    /// Ticks spent on a reload, outside the seat's time.
    paused: u64,
    overran: bool,
    /// The seat's processes' CPU time when the turn began, by pid.
    cpu_at: Vec<(u32, u64)>,
}

/// A process's CPU time so far, in nanoseconds, summed over its threads: the
/// time it has actually run, which our own work on its core never adds to.
/// On macOS (the local runner) the kernel's rusage, through the sandbox
/// crate. None for a process already gone.
pub fn cpu_ns(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut total = 0u64;
        for task in std::fs::read_dir(format!("/proc/{pid}/task")).ok()? {
            let path = task.ok()?.path().join("schedstat");
            let text = std::fs::read_to_string(path).ok()?;
            total += text.split_whitespace().next()?.parse::<u64>().ok()?;
        }
        Some(total)
    }
    #[cfg(not(target_os = "linux"))]
    botstadia_sandbox::process_cpu_ns(pid)
}

/// A seat's time in its turn (BOTS.md#one-seat-at-a-time-per-core: each seat
/// is timed only while it runs): the canary's ticks, unless its processes
/// ran for less, at the calibrated rate. Ticks alone count anything else
/// that took its core meanwhile, our own threads included, against it;
/// CPU time alone counts a slowed machine against it. The lesser of the two
/// counts neither. Found on the testnet: four house bots put out at a 1–2 ms
/// backstop by turns of 0.1–0.2 ms.
fn seat_time(ticks: u64, cpu_ns: Option<u64>) -> u64 {
    match cpu_ns {
        Some(ns) => {
            let cpu = (ns as u128 * cal::TICKS_PER_SECOND as u128 / 1_000_000_000) as u64;
            ticks.min(cpu)
        }
        None => ticks,
    }
}

/// A seat's second overrun puts it out for the session (BOTS.md#the-backstop).
const MAX_OVERRUNS: u32 = 2;

/// The limits of one session, in canary ticks.
struct Budget {
    turn: u64,
    start: u64,
    load: u64,
    grace: u64,
    safety: u64,
}

impl Budget {
    fn new(seats: u64, max_turns: u64, turn_fuel: u64, init_fuel: u64) -> Self {
        let turn = cal::backstop(turn_fuel);
        let start = cal::COMPILE_CEILING_TICKS * cal::BACKSTOP + cal::backstop(init_fuel);
        let load = cal::LOAD_CEILING_TICKS * cal::BACKSTOP;
        let grace = cal::KILL_GRACE_TICKS;
        // Everything a session can reach without a fault of ours
        // (BOTS.md#the-safety-limit): every seat at its backstop on every turn
        // and in its start; its loads and a restart with both graces; its
        // one overrun that may run on to the kill (a second puts it out);
        // and the arena's worst step every turn and its setup, at the
        // calibrated rate (the arena is ours and fuzzed, so no backstop
        // multiple). Found on the testnet: a grace on every turn and the
        // multiple on the arena made the longest session hours, past the
        // result deadline, so no session could be anchored.
        let safety = seats * (start + 2 * load + 4 * grace + 3 * grace)
            + (max_turns + 1) * (seats * turn + cal::ticks_for_fuel(LIMITS.arena_step_fuel))
            + cal::ticks_for_fuel(LIMITS.arena_setup_fuel);
        Self {
            turn,
            start,
            load,
            grace,
            safety,
        }
    }
}

struct Job<'a> {
    held: &'a Held,
    order: Vec<usize>,
    budget_id: u32,
    declaration: Vec<u8>,
    iso: &'a Isolation,
    tx: Sender<Event>,
    arena: Worker,
    seats: Vec<SeatState>,
    queue: VecDeque<(u32, Vec<u8>)>,
    current: Option<Turn>,
    limits: Budget,
    incidents: Vec<String>,
}

fn report(trapped: Option<String>) -> Vec<u8> {
    encode(&TurnReport {
        paused: trapped.is_none(),
        trapped,
        ..TurnReport::default()
    })
}

impl Job<'_> {
    fn now(&self) -> u64 {
        self.iso.clock.now()
    }

    /// The pids of a seat's live processes: its Wasmtime one and its
    /// inference one.
    fn seat_pids(&self, i: u32) -> Vec<u32> {
        let s = &self.seats[i as usize];
        [s.wasm.as_ref(), s.inference.as_ref()]
            .into_iter()
            .flatten()
            .map(|w| w.child.id())
            .collect()
    }

    /// The current turn's seat time so far, in ticks ([`seat_time`]). CPU
    /// time is read only once the canary says the turn is past `over`, so a
    /// turn well inside its backstop costs one read, at its start.
    fn spent(&self, over: u64) -> u64 {
        let Some(t) = self.current.as_ref() else {
            return 0;
        };
        let ticks = self.now().saturating_sub(t.since).saturating_sub(t.paused);
        if ticks <= over || t.cpu_at.is_empty() {
            return ticks;
        }
        // A process started in the turn (an inference restart) counts from
        // zero; one gone since counts nothing more.
        let mut ns = 0u64;
        for pid in self.seat_pids(t.seat) {
            let Some(now) = cpu_ns(pid) else {
                return ticks;
            };
            let at = t
                .cpu_at
                .iter()
                .find(|(p, _)| *p == pid)
                .map_or(0, |(_, n)| *n);
            ns += now.saturating_sub(at);
        }
        seat_time(ticks, Some(ns))
    }

    fn send_arena(&mut self, kind: u8, seat: u32, body: Vec<u8>) {
        let _ = self.arena.send(&Frame::new(kind, seat, body));
    }

    fn send_seat(&mut self, i: u32, kind: u8, body: Vec<u8>) {
        if let Some(w) = self.seats[i as usize].wasm.as_mut() {
            let _ = w.send(&Frame::new(kind, 0, body));
        }
    }

    fn send_inference(&mut self, i: u32, kind: u8, body: Vec<u8>) -> bool {
        match self.seats[i as usize].inference.as_mut() {
            Some(w) => w.send(&Frame::new(kind, 0, body)).is_ok(),
            None => false,
        }
    }

    fn spawn_inference(&mut self, i: u32) -> bool {
        match self.iso.spawner.spawn(Kind::Inference, self.budget_id) {
            Ok(mut w) => {
                let s = &mut self.seats[i as usize];
                s.inference_gen += 1;
                read_from(
                    Source::Inference(i, s.inference_gen),
                    w.from.take().expect("fresh"),
                    self.tx.clone(),
                );
                self.seats[i as usize].inference = Some(w);
                true
            }
            Err(_) => false,
        }
    }

    /// The next queued turn, if the core is free.
    fn dispatch(&mut self) {
        while self.current.is_none() {
            let Some((i, body)) = self.queue.pop_front() else {
                return;
            };
            if self.seats[i as usize].dead {
                let why = self.seats[i as usize]
                    .gone
                    .clone()
                    .unwrap_or_else(|| "was killed".into());
                self.send_arena(ARENA_REPORT, i, report(Some(format!("its process {why}"))));
                continue;
            }
            let cpu_at = self
                .seat_pids(i)
                .into_iter()
                .filter_map(|pid| cpu_ns(pid).map(|ns| (pid, ns)))
                .collect();
            self.send_seat(i, SEAT_TURN, body);
            self.current = Some(Turn {
                seat: i,
                since: self.now(),
                paused: 0,
                overran: false,
                cpu_at,
            });
        }
    }

    /// A seat's inference process is killed: restarted once, then over.
    /// During its first load it isn't restarted: the load failed, the bot's
    /// failure, and the seat is told so at once.
    fn kill_inference(&mut self, i: u32) {
        let now = self.now();
        let s = &mut self.seats[i as usize];
        if let Some(mut w) = s.inference.take() {
            w.kill();
        }
        if s.first_load {
            self.first_load_answered(i, false, now);
            return;
        }
        let in_flight = s.infer_at.take().is_some();
        s.cancel_at = None;
        if in_flight {
            self.send_seat(
                i,
                SEAT_OUTPUTS,
                encode(&Err::<(), _>(InferFailure::Stopped)),
            );
        }
        let s = &mut self.seats[i as usize];
        if s.restarts_left > 0 && s.models.is_some() {
            s.restarts_left -= 1;
            let models = s.models.clone().expect("checked");
            if self.spawn_inference(i) && self.send_inference(i, INF_LOAD, models) {
                let s = &mut self.seats[i as usize];
                s.reloading = Some(now);
                s.load_at = Some(now);
                return;
            }
        }
        self.seats[i as usize].inference_over = true;
    }

    /// The first load's answer, `ok` or not: the seat is told, and the wait
    /// is outside its start time.
    fn first_load_answered(&mut self, i: u32, ok: bool, now: u64) {
        let s = &mut self.seats[i as usize];
        s.first_load = false;
        let load_at = s.load_at.take();
        s.start_paused += load_at.map_or(0, |a| now.saturating_sub(a));
        if !ok {
            s.inference_over = true;
        }
        self.send_seat(i, SEAT_LOADED, vec![ok as u8]);
    }

    /// A seat's Wasmtime process is gone, killed or ended.
    fn seat_gone(&mut self, i: u32, why: &str) {
        let s = &mut self.seats[i as usize];
        if s.dead {
            return;
        }
        s.dead = true;
        s.gone = Some(why.to_string());
        if let Some(mut w) = s.wasm.take() {
            // Its own end is waited for a moment; one we're ending isn't.
            let how = if why == "ended" {
                w.ended()
            } else {
                w.ended_now()
            };
            if let Some(how) = how
                && (how.contains("SIGSYS") || how.contains("signal"))
            {
                self.incidents.push(format!(
                    "seat {i} (entrant {}): its Wasmtime process ended with {how}",
                    self.order[i as usize]
                ));
            }
            w.kill();
        }
        if let Some(mut w) = self.seats[i as usize].inference.take() {
            w.kill();
        }
        let s = &self.seats[i as usize];
        if !s.started {
            let began = s.began;
            self.seats[i as usize].started = true;
            let answer: Result<TurnReport, String> = if began {
                Ok(TurnReport {
                    trapped: Some(format!("its process {why} in init")),
                    ..TurnReport::default()
                })
            } else {
                Err(format!("its process {why} before its code ran"))
            };
            self.send_arena(ARENA_SEAT_STARTED, i, encode(&answer));
            return;
        }
        if self.current.as_ref().is_some_and(|t| t.seat == i) {
            let overran = self.current.as_ref().is_some_and(|t| t.overran);
            self.current = None;
            if !overran {
                self.send_arena(ARENA_REPORT, i, report(Some(format!("its process {why}"))));
            }
            self.dispatch();
        }
    }

    fn on_arena(&mut self, f: Frame) -> Option<Played> {
        let i = f.seat;
        match f.kind {
            ARENA_SPAWN if (i as usize) < self.seats.len() => {
                let now = self.now();
                let s = &mut self.seats[i as usize];
                s.start_at = now;
                s.restarts_left = 1;
                match self.iso.spawner.spawn(Kind::Seat, self.budget_id) {
                    Ok(mut w) => {
                        read_from(
                            Source::Seat(i),
                            w.from.take().expect("fresh"),
                            self.tx.clone(),
                        );
                        let start = SeatStart {
                            budget: self.budget_id,
                            bundle: self.held.bundles[self.order[i as usize]].clone(),
                            declaration: self.declaration.clone(),
                            settings: f.body,
                        };
                        let _ = w.send(&Frame::new(SEAT_START, 0, encode(&start)));
                        self.seats[i as usize].wasm = Some(w);
                    }
                    Err(e) => {
                        self.seats[i as usize].started = true;
                        self.seats[i as usize].dead = true;
                        let answer: Result<TurnReport, String> =
                            Err(format!("starting its process: {e}"));
                        self.send_arena(ARENA_SEAT_STARTED, i, encode(&answer));
                    }
                }
                None
            }
            ARENA_TURN if (i as usize) < self.seats.len() => {
                self.queue.push_back((i, f.body));
                self.dispatch();
                None
            }
            ARENA_DONE => Some(self.finish(&f.body)),
            _ => Some(Played::void_of(
                VoidReason::EnclaveFailure,
                self.order.clone(),
            )),
        }
    }

    fn on_seat(&mut self, i: u32, f: Frame) {
        match f.kind {
            SEAT_BEGIN => self.seats[i as usize].began = true,
            // Its models come once: a second is refused (an escaped seat
            // could otherwise start a fresh inference process at will).
            SEAT_MODELS if self.seats[i as usize].models.is_some() => {}
            SEAT_MODELS => {
                let now = self.now();
                self.seats[i as usize].models = Some(f.body.clone());
                self.seats[i as usize].load_at = Some(now);
                self.seats[i as usize].first_load = true;
                if !(self.spawn_inference(i) && self.send_inference(i, INF_LOAD, f.body)) {
                    self.first_load_answered(i, false, now);
                }
            }
            SEAT_STARTED => {
                self.seats[i as usize].started = true;
                self.send_arena(ARENA_SEAT_STARTED, i, f.body);
            }
            SEAT_INFER => {
                let s = &self.seats[i as usize];
                if s.inference_over {
                    self.send_seat(
                        i,
                        SEAT_OUTPUTS,
                        encode(&Err::<(), _>(InferFailure::Stopped)),
                    );
                } else if s.reloading.is_some() {
                    self.seats[i as usize].held_infer = Some(f.body);
                } else if self.send_inference(i, INF_RUN, f.body) {
                    let now = self.now();
                    self.seats[i as usize].infer_at = Some(now);
                } else {
                    self.kill_inference(i);
                }
            }
            SEAT_REPORT => {
                if self.current.as_ref().is_some_and(|t| t.seat == i) {
                    let overran = self.current.as_ref().is_some_and(|t| t.overran);
                    self.current = None;
                    if !overran {
                        self.send_arena(ARENA_REPORT, i, f.body);
                    }
                    self.dispatch();
                }
            }
            _ => self.seat_gone(i, "broke the protocol"),
        }
    }

    fn on_inference(&mut self, i: u32, f: Frame) {
        let now = self.now();
        match f.kind {
            INF_LOADED => {
                let ok = decode::<Result<(), String>>(&f.body, 1 << 16)
                    .map(|r| r.is_ok())
                    .unwrap_or(false);
                if self.seats[i as usize].first_load {
                    self.first_load_answered(i, ok, now);
                    return;
                }
                let s = &mut self.seats[i as usize];
                s.load_at = None;
                if !ok {
                    s.inference_over = true;
                }
                if let Some(since) = s.reloading.take() {
                    if let Some(t) = self.current.as_mut().filter(|t| t.seat == i) {
                        t.paused += now.saturating_sub(since);
                    }
                    if let Some(body) = self.seats[i as usize].held_infer.take() {
                        if self.seats[i as usize].inference_over {
                            self.send_seat(
                                i,
                                SEAT_OUTPUTS,
                                encode(&Err::<(), _>(InferFailure::Stopped)),
                            );
                        } else if self.send_inference(i, INF_RUN, body) {
                            self.seats[i as usize].infer_at = Some(now);
                        }
                    }
                }
            }
            INF_OUTPUTS => {
                let s = &mut self.seats[i as usize];
                s.infer_at = None;
                s.cancel_at = None;
                self.send_seat(i, SEAT_OUTPUTS, f.body);
            }
            _ => self.kill_inference(i),
        }
    }

    /// The clocks: the backstop, the graces, the ceilings.
    fn tick(&mut self) {
        let now = self.now();
        let (turn, grace, start, load) = (
            self.limits.turn,
            self.limits.grace,
            self.limits.start,
            self.limits.load,
        );
        let over = if self.current.as_ref().is_some_and(|t| t.overran) {
            turn + 3 * grace
        } else {
            turn
        };
        let spent = self.spent(over);
        if let Some(t) = self.current.as_mut() {
            let i = t.seat;
            if !t.overran && spent > turn {
                // The turn ends at the backstop: an overrun, like running out
                // of fuel, against the bot. A call in flight is cancelled.
                t.overran = true;
                self.send_arena(ARENA_REPORT, i, report(None));
                let s = &mut self.seats[i as usize];
                s.overruns += 1;
                if s.overruns >= MAX_OVERRUNS {
                    // Its second: out for the session at once, so its
                    // overruns can't stretch every turn by the kill graces
                    // (BOTS.md#the-backstop).
                    self.seat_gone(i, "overran its backstop twice");
                    return;
                }
                let s = &mut self.seats[i as usize];
                if s.infer_at.is_some() && s.cancel_at.is_none() {
                    s.cancel_at = Some(now);
                    self.send_inference(i, INF_CANCEL, Vec::new());
                }
            }
            let s = &self.seats[i as usize];
            if let Some(c) = s.cancel_at
                && now.saturating_sub(c) > grace
            {
                self.kill_inference(i);
            } else if spent > turn + 3 * grace && s.infer_at.is_none() && s.reloading.is_none() {
                // Its own code won't stop: killed, inactive for good.
                self.seat_gone(i, "was killed at the backstop");
            }
        }
        for i in 0..self.seats.len() as u32 {
            let s = &self.seats[i as usize];
            if s.wasm.is_some() && !s.started && !s.dead {
                let spent = now
                    .saturating_sub(s.start_at)
                    .saturating_sub(s.start_paused);
                let loading = s.load_at.is_some();
                if spent > start && !loading {
                    self.seat_gone(i, "overran its start");
                    continue;
                }
            }
            let s = &self.seats[i as usize];
            if let Some(at) = s.load_at
                && now.saturating_sub(at) > load
            {
                // A load past its ceiling ends the seat's inference, as the
                // bot's failure.
                if s.first_load {
                    if let Some(mut w) = self.seats[i as usize].inference.take() {
                        w.kill();
                    }
                    self.first_load_answered(i, false, now);
                    continue;
                }
                let s = &mut self.seats[i as usize];
                s.load_at = None;
                s.reloading = None;
                s.inference_over = true;
                if let Some(mut w) = s.inference.take() {
                    w.kill();
                }
                if self.seats[i as usize].held_infer.take().is_some() {
                    self.send_seat(
                        i,
                        SEAT_OUTPUTS,
                        encode(&Err::<(), _>(InferFailure::Stopped)),
                    );
                }
            }
        }
    }

    fn finish(&mut self, body: &[u8]) -> Played {
        let order = self.order.clone();
        let done: ArenaDone = match decode(body, MAX_FRAME as u64) {
            Ok(d) => d,
            Err(_) => return Played::void_of(VoidReason::EnclaveFailure, order),
        };
        match done {
            ArenaDone::Finished {
                places,
                archive,
                metrics,
                trapped,
                ..
            } => {
                let n = order.len();
                let mut groups = Vec::with_capacity(places.len());
                for g in &places {
                    let mut group = Vec::with_capacity(g.len());
                    for &seat in g {
                        match order.get(seat as usize) {
                            Some(&pos) => group.push(pos),
                            None => return Played::void_of(VoidReason::EnclaveFailure, order),
                        }
                    }
                    groups.push(group);
                }
                let outcome = match Outcome::from_groups(n, &groups) {
                    Ok(o) => o,
                    Err(_) => return Played::void_of(VoidReason::EnclaveFailure, order),
                };
                let mut incidents: Vec<String> = trapped
                    .iter()
                    .map(|(seat, turn, why)| {
                        format!(
                            "seat {seat} (entrant {}) on turn {turn}: {why}",
                            order.get(*seat as usize).copied().unwrap_or(usize::MAX)
                        )
                    })
                    .collect();
                incidents.append(&mut self.incidents);
                Played {
                    outcome,
                    seats: order,
                    archive,
                    metrics,
                    incidents,
                }
            }
            ArenaDone::ArenaFault(_) => Played::void_of(VoidReason::ArenaTrap, order),
            ArenaDone::EntrantUnavailable(_) => {
                Played::void_of(VoidReason::EntrantUnavailable, order)
            }
            ArenaDone::Failed(_) => Played::void_of(VoidReason::EnclaveFailure, order),
        }
    }
}

/// Ask a one-shot worker of `kind` one question: `question` in, the first
/// frame of `answer` back, within `limit` ticks, or it's killed.
fn ask(
    iso: &Isolation,
    kind: Kind,
    budget: u32,
    question: Frame,
    answer: u8,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut w = iso
        .spawner
        .spawn(kind, budget)
        .map_err(|e| format!("starting its worker: {e}"))?;
    let (tx, rx) = channel();
    read_from(Source::Arena, w.from.take().expect("fresh"), tx);
    w.send(&question).map_err(|e| format!("its worker: {e}"))?;
    let began = iso.clock.now();
    loop {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(Event::Frame(_, f)) if f.kind == answer => return Ok(f.body),
            Ok(Event::Frame(..)) => return Err("its worker broke the protocol".into()),
            Ok(Event::Ended(_)) | Err(RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "its worker ended: {}",
                    w.ended().unwrap_or_else(|| "no answer".into())
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if iso.clock.now().saturating_sub(began) > limit {
            w.kill();
            return Err("its worker overran its ceiling".into());
        }
    }
}

/// The anchor-time recheck of one bundle, in a seat's own Wasmtime process
/// (BOTS.md#budget-versions): unzip, manifest, compile and preflight.
pub fn check(iso: &Isolation, budget: u32, bundle: &[u8], turn_fuel: u64) -> Result<(), String> {
    let question = Frame::new(
        SEAT_CHECK,
        0,
        encode(&SeatCheck {
            bundle: bundle.to_vec(),
            turn_fuel,
        }),
    );
    let body = ask(
        iso,
        Kind::Seat,
        budget,
        question,
        SEAT_STARTED,
        cal::COMPILE_CEILING_TICKS * cal::BACKSTOP,
    )?;
    decode::<Result<TurnReport, String>>(&body, 1 << 20)
        .map_err(|e| e.to_string())?
        .map(|_| ())
}

/// Verification in a worker of its own: the static checks and the timing
/// run, under the compile and load ceilings and the timing run's backstops.
/// Why a verification has no report: the bundle failing (a verdict, signed
/// as a failed upload report), or a fault of ours, which is no verdict
/// (BOTS.md#bundles): the arena, the worker ending, crashing or having a
/// syscall refused, or its backstop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    Bundle(String),
    Fault(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bundle(why) => write!(f, "{why}"),
            Self::Fault(why) => write!(f, "a fault of ours: {why}"),
        }
    }
}

pub fn verify(
    iso: &Isolation,
    budget: u32,
    bundle: &[u8],
    arena: &[u8],
    seed: &[u8; 32],
) -> Result<botstadia_host::verify::Report, VerifyError> {
    let declaration =
        LoadedArena::from_bytes(arena).map_err(|e| VerifyError::Fault(format!("{e:#}")))?;
    let d = &declaration.declaration;
    let limit = cal::COMPILE_CEILING_TICKS * cal::BACKSTOP
        + cal::LOAD_CEILING_TICKS * cal::BACKSTOP
        + cal::backstop(d.budgets.init_fuel)
        + (botstadia_host::verify::TIMING_TURNS as u64 + 1)
            * (cal::backstop(d.budgets.turn_fuel) + cal::backstop(LIMITS.arena_step_fuel));
    let question = Frame::new(
        VERIFY_START,
        0,
        encode(&VerifyStart {
            bundle: bundle.to_vec(),
            arena: arena.to_vec(),
            seed: *seed,
        }),
    );
    let body = ask(iso, Kind::Verify, budget, question, VERIFY_REPORT, limit)
        .map_err(VerifyError::Fault)?;
    decode::<Result<botstadia_host::verify::Report, String>>(&body, 16 << 20)
        .map_err(|e| VerifyError::Fault(e.to_string()))?
        .map_err(VerifyError::Bundle)
}

/// The longest a session of `seats` on this arena can run without a fault
/// of ours (its safety limit), in seconds at the calibrated canary rate.
pub fn longest_session_s(arena: &[u8], seats: usize) -> anyhow::Result<u64> {
    let arena = LoadedArena::from_bytes(arena)?;
    let d = &arena.declaration;
    let b = Budget::new(
        seats as u64,
        d.max_turns as u64,
        d.budgets.turn_fuel,
        d.budgets.init_fuel,
    );
    Ok(b.safety.div_ceil(cal::TICKS_PER_SECOND))
}

/// Play a held session under `seed` with every seat and the arena isolated.
pub fn play(held: &Held, seed: &[u8; 32], iso: &Isolation) -> Played {
    let n = held.bundles.len();
    let order = crate::play::seats(seed, n);
    let budget_id = held.payload.budget_version;
    if botstadia_host::budget::version(budget_id).is_none() {
        return Played::void_of(VoidReason::EnclaveFailure, order);
    }
    // The arena is ours: read its declaration for the session's limits.
    let arena = match LoadedArena::from_bytes(&held.arena) {
        Ok(a) => a,
        Err(_) => return Played::void_of(VoidReason::ArenaTrap, order),
    };
    let d = &arena.declaration;
    let limits = Budget::new(
        n as u64,
        d.max_turns as u64,
        d.budgets.turn_fuel,
        d.budgets.init_fuel,
    );
    let names: Vec<String> = order
        .iter()
        .map(|&pos| {
            held.payload
                .entrants
                .iter()
                .find(|e| e.position as usize == pos)
                .map(|e| e.bot_id.clone())
                .unwrap_or_else(|| crate::digest(&held.bundles[pos]))
        })
        .collect();
    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
    let mut arena_worker = match iso.spawner.spawn(Kind::Arena, budget_id) {
        Ok(w) => w,
        Err(_) => return Played::void_of(VoidReason::EnclaveFailure, order),
    };
    read_from(
        Source::Arena,
        arena_worker.from.take().expect("fresh"),
        tx.clone(),
    );
    let start = ArenaStart {
        arena: held.arena.clone(),
        budget: budget_id,
        seed: *seed,
        names,
    };
    if arena_worker
        .send(&Frame::new(ARENA_START, 0, encode(&start)))
        .is_err()
    {
        return Played::void_of(VoidReason::EnclaveFailure, order);
    }
    let mut job = Job {
        held,
        order: order.clone(),
        budget_id,
        declaration: arena.declaration_json.to_vec(),
        iso,
        tx,
        arena: arena_worker,
        seats: (0..n).map(|_| SeatState::default()).collect(),
        queue: VecDeque::new(),
        current: None,
        limits,
        incidents: Vec::new(),
    };
    let began = job.now();
    loop {
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(Event::Frame(Source::Arena, f)) => {
                if let Some(played) = job.on_arena(f) {
                    return played;
                }
            }
            Ok(Event::Frame(Source::Seat(i), f)) if (i as usize) < n => job.on_seat(i, f),
            Ok(Event::Frame(Source::Inference(i, g), f))
                if (i as usize) < n && g == job.seats[i as usize].inference_gen =>
            {
                job.on_inference(i, f)
            }
            Ok(Event::Ended(Source::Arena)) => {
                if let Some(how) = job.arena.ended() {
                    job.incidents
                        .push(format!("the arena's process ended with {how}"));
                }
                return Played::void_of(VoidReason::EnclaveFailure, order);
            }
            Ok(Event::Ended(Source::Seat(i))) if (i as usize) < n => job.seat_gone(i, "ended"),
            Ok(Event::Ended(Source::Inference(i, g)))
                if (i as usize) < n && g == job.seats[i as usize].inference_gen =>
            {
                if let Some(w) = job.seats[i as usize].inference.as_mut() {
                    // A crash or a refused syscall in its inference process
                    // is a possible seat escape too (OPERATIONS.md#alerts).
                    if let Some(how) = w.ended()
                        && (how.contains("SIGSYS") || how.contains("signal"))
                    {
                        job.incidents.push(format!(
                            "seat {i} (entrant {}): its inference process ended with {how}",
                            job.order[i as usize]
                        ));
                    }
                    job.kill_inference(i);
                }
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Played::void_of(VoidReason::EnclaveFailure, order);
            }
        }
        job.tick();
        if job.now().saturating_sub(began) > job.limits.safety {
            return Played::void_of(VoidReason::SafetyLimit, order);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seat_is_timed_only_while_it_runs() {
        let second = cal::TICKS_PER_SECOND;
        // Kept off its core by something else: 1 s of ticks, 1 ms of CPU.
        assert_eq!(seat_time(second, Some(1_000_000)), second / 1000);
        // A slowed machine: 2 s of CPU in 1 s of canary ticks.
        assert_eq!(seat_time(second, Some(2_000_000_000)), second);
        // No CPU figure: the ticks.
        assert_eq!(seat_time(second, None), second);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_process_cpu_time_counts_its_work_not_its_sleep() {
        // Processes of their own, as seats are: the test binary's other
        // threads come and go (CI found its sum going backwards).
        use std::process::{Command, Stdio};
        let measure = |cmd: &str, args: &[&str]| {
            let mut child = Command::new(cmd)
                .args(args)
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(20));
            let a = cpu_ns(child.id()).expect("cpu time");
            std::thread::sleep(Duration::from_millis(200));
            let b = cpu_ns(child.id()).expect("cpu time");
            child.kill().unwrap();
            child.wait().unwrap();
            b.saturating_sub(a)
        };
        let slept = measure("sleep", &["5"]);
        assert!(slept < 20_000_000, "slept 200 ms, ran {slept} ns");
        let spun = measure("yes", &[]);
        assert!(spun >= 50_000_000, "spun 200 ms, ran {spun} ns");
    }

    #[test]
    fn the_safety_limit_sits_above_every_seat_at_its_backstop() {
        let b = Budget::new(6, 1000, 50_000_000, 800_000_000);
        assert!(b.safety > 6 * 1000 * b.turn);
        assert!(b.safety > 6 * b.start);
        assert!(b.turn >= cal::BACKSTOP);
    }

    /// Found on the testnet: the longest session was hours (a kill grace on
    /// every turn, the backstop multiple on the arena's step), past the
    /// result deadline, so no session could be anchored. The published
    /// arenas' limits, in minutes at the calibrated rate, now fit well
    /// inside even the devnet's two hours with two finality lags.
    #[test]
    fn the_published_arenas_longest_sessions_fit_inside_the_deadline() {
        let minutes = |seats, turns, turn_fuel, init_fuel| {
            Budget::new(seats, turns, turn_fuel, init_fuel)
                .safety
                .div_ceil(cal::TICKS_PER_SECOND * 60)
        };
        // Boxing and dance-off: 2 seats, 4,500 turns.
        let boxing = minutes(2, 4500, 2_000_000, 32_000_000);
        // Racing: up to 6 drones, 9,000 turns.
        let racing4 = minutes(4, 9000, 1_000_000, 16_000_000);
        let racing6 = minutes(6, 9000, 1_000_000, 16_000_000);
        assert!(boxing <= 25, "boxing {boxing} min");
        assert!(racing4 <= 45, "racing (4) {racing4} min");
        assert!(racing6 <= 60, "racing (6) {racing6} min");
        // Still above every seat at its backstop on every turn.
        let b = Budget::new(2, 4500, 2_000_000, 32_000_000);
        assert!(b.safety > 2 * 4501 * b.turn + 2 * b.start);
    }
}
