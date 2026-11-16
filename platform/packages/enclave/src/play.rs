//! Running a session (TRUST.md#every-session-one-flow, step 4), once its
//! anchor is final: the seed, the seats drawn from it, the session on
//! `botstadia-host`, and its outcome over the spec's entrants.
//!
//! Failures of ours void (COMPETITION.md#rules-for-failures):
//! - an entrant that can't be loaded, compiled or instantiated, before any
//!   of its code runs: `entrant-unavailable`;
//! - the arena trapping or erroring: `arena-trap`;
//! - the session's work passing the safety limit: `safety-limit`;
//! - anything else failing inside the enclave: `enclave-failure`.
//!
//! From a bot's first instruction on, its failures are its own and the host
//! plays it with no action.

use std::sync::Arc;

use botstadia_chain::outcome::{Outcome, VoidReason};
use botstadia_chain::{B256, keccak256};
use botstadia_host::session::{ArenaFault, EntrantUnavailable};
use botstadia_host::{LoadedArena, Seat, Session, SessionOptions};

use crate::hold::Held;

/// The session's seed: `hash(session id, anchor, fresh randomness)`, the
/// randomness drawn inside the enclave for this session alone, so neither
/// we nor the sequencer can steer it.
pub fn seed(session: B256, anchor: B256, fresh: [u8; 32]) -> [u8; 32] {
    keccak256([session.as_slice(), anchor.as_slice(), &fresh].concat()).0
}

/// Fresh randomness for one session.
pub fn fresh() -> anyhow::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(b)
}

/// Seats from the seed: `seats[i]` is the spec position sitting in seat `i`.
/// The published seating version, so anyone redraws them from the result's
/// seed (BOTS.md#the-bot-interface).
pub use botstadia_seating::seats;

/// The safety limit's arena allowances, constants of the image. They sit
/// several times above the worst measured arena step and setup (Phase 0:
/// dance-off, 82M and 335M arena fuel).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub arena_step_fuel: u64,
    pub arena_setup_fuel: u64,
    /// A seat's backstop as a multiple of its budget.
    pub backstop: u64,
}

pub const LIMITS: Limits = Limits {
    arena_step_fuel: 400_000_000,
    arena_setup_fuel: 2_000_000_000,
    backstop: 2,
};

impl Limits {
    /// Everything a session can reach without a fault of ours
    /// (BOTS.md#the-safety-limit), in fuel.
    pub fn session(&self, max_turns: u64, seats: u64, turn_fuel: u64, init_fuel: u64) -> u64 {
        let per_turn = seats * turn_fuel * self.backstop + self.arena_step_fuel;
        max_turns.saturating_mul(per_turn)
            + seats * init_fuel * self.backstop
            + self.arena_setup_fuel
    }
}

/// What a played session gives the result batch and the archive upload.
pub struct Played {
    pub outcome: Outcome,
    /// The spec position in each seat, recorded with the archive.
    pub seats: Vec<usize>,
    /// The replay archive and metrics file, uploaded only once the batch
    /// carrying the result is included.
    pub archive: Vec<u8>,
    pub metrics: Vec<u8>,
    /// Seat crashes, held under the deferred report rule: reported only once
    /// the batch carrying the result is included (TRUST.md#the-deferred-
    /// report-rule).
    pub incidents: Vec<String>,
}

impl Played {
    /// A void: no archive, no metrics.
    pub fn void_of(reason: VoidReason, seats: Vec<usize>) -> Self {
        Self::void(reason, seats)
    }

    fn void(reason: VoidReason, seats: Vec<usize>) -> Self {
        Self {
            outcome: Outcome::Void { reason },
            seats,
            archive: Vec::new(),
            metrics: Vec::new(),
            incidents: Vec::new(),
        }
    }

    /// The archive hash the result signs: over the replay and the metrics.
    pub fn archive_hash(&self) -> B256 {
        archive_hash(&self.archive, &self.metrics)
    }
}

/// `keccak256(sha256(archive) ‖ sha256(metrics))`; zero for a void with no
/// archive.
pub fn archive_hash(archive: &[u8], metrics: &[u8]) -> B256 {
    use sha2::{Digest, Sha256};
    if archive.is_empty() && metrics.is_empty() {
        return B256::ZERO;
    }
    keccak256(
        [
            Sha256::digest(archive).as_slice(),
            Sha256::digest(metrics).as_slice(),
        ]
        .concat(),
    )
}

/// A failure's void reason: the arena's own faults are `arena-trap`, and a
/// seat that couldn't start before any bot code ran is `entrant-unavailable`.
/// A bot's own start function failing never reaches here: that seat is
/// inactive and the session goes on.
pub fn classify(error: &anyhow::Error) -> VoidReason {
    if error
        .chain()
        .any(|e| e.downcast_ref::<ArenaFault>().is_some())
    {
        VoidReason::ArenaTrap
    } else if error
        .chain()
        .any(|e| e.downcast_ref::<EntrantUnavailable>().is_some())
    {
        VoidReason::EntrantUnavailable
    } else {
        VoidReason::EnclaveFailure
    }
}

/// How this runner plays its jobs once [`isolate_jobs`] has been called:
/// every seat and the arena in processes of their own, one job per core.
struct Isolated {
    exe: std::path::PathBuf,
    strict: bool,
    clock: crate::canary::Ticks,
    cores: std::sync::Mutex<Vec<usize>>,
    /// The machine's memory not yet promised to a run's processes' caps,
    /// and the queue of runs waiting for it.
    memory: std::sync::Mutex<Promises>,
    freed: std::sync::Condvar,
    total: u64,
}

/// How a run waits for its memory.
#[derive(Clone, Copy)]
pub enum Turn {
    /// In arrival order, behind every run before it.
    Queued,
    /// As soon as it fits, ahead of the queue: only for a run of seconds
    /// whose lateness costs a session, the anchor-time recheck. It can delay
    /// the run being served by no more than its own few seconds.
    Brief,
}

/// Memory not yet promised, and tickets in arrival order: runs take their
/// memory first come, first served, so smaller runs never keep overtaking a
/// large race that's waiting.
struct Promises {
    free: u64,
    next_ticket: u64,
    serving: u64,
}

/// What one run holds, given back however the run ends, a panic included
/// (the runner survives a panicked job, so a leak would starve every later
/// one).
struct Promised<'a> {
    isolated: &'a Isolated,
    memory: u64,
    core: Option<usize>,
}

impl Drop for Promised<'_> {
    fn drop(&mut self) {
        if let Some(core) = self.core {
            self.isolated
                .cores
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(core);
        }
        let mut m = match self.isolated.memory.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        m.free += self.memory;
        drop(m);
        self.isolated.freed.notify_all();
    }
}

impl Isolated {
    /// Wait for this run's turn and its memory, then take a core.
    #[cfg(test)]
    fn promise(&self, memory: u64) -> Promised<'_> {
        self.promise_as(memory, Turn::Queued)
    }

    /// Wait for memory as `turn` says, then take a core.
    fn promise_as(&self, memory: u64, turn: Turn) -> Promised<'_> {
        // A run larger than the whole machine runs alone.
        let memory = memory.min(self.total);
        let mut m = self.memory.lock().unwrap_or_else(|p| p.into_inner());
        let ticket = match turn {
            Turn::Queued => {
                let t = m.next_ticket;
                m.next_ticket += 1;
                Some(t)
            }
            Turn::Brief => None,
        };
        while ticket.is_some_and(|t| m.serving != t) || m.free < memory {
            m = self.freed.wait(m).unwrap_or_else(|p| p.into_inner());
        }
        m.free -= memory;
        if ticket.is_some() {
            m.serving += 1;
        }
        drop(m);
        // The next in line may fit too.
        self.freed.notify_all();
        let core = self.cores.lock().unwrap_or_else(|p| p.into_inner()).pop();
        Promised {
            isolated: self,
            memory,
            core,
        }
    }
}

/// What the runner keeps for itself, the keyholders, the canary and the
/// kernel: never promised to a run.
const MEMORY_KEPT: u64 = 2 << 30;

/// The machine's memory, from `/proc/meminfo` (none where it can't be read).
fn machine_memory() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

static ISOLATED: std::sync::OnceLock<Isolated> = std::sync::OnceLock::new();

/// From now on, play every job isolated (BOTS.md, isolation), each on one
/// of `cores`, timed by `clock`. Always in an image.
pub fn isolate_jobs(
    exe: std::path::PathBuf,
    strict: bool,
    clock: crate::canary::Ticks,
    cores: Vec<usize>,
) {
    let total = machine_memory()
        .map(|m| m.saturating_sub(MEMORY_KEPT))
        .unwrap_or(u64::MAX);
    let _ = ISOLATED.set(Isolated {
        exe,
        strict,
        clock,
        cores: std::sync::Mutex::new(cores),
        memory: std::sync::Mutex::new(Promises {
            free: total,
            next_ticket: 0,
            serving: 0,
        }),
        freed: std::sync::Condvar::new(),
        total,
    });
}

/// Whether jobs play isolated.
pub fn isolated() -> bool {
    ISOLATED.get().is_some()
}

/// Run `f` with an isolation on one of the runner's job cores, or `None`
/// where the runner doesn't isolate (the local default). `memory` is the
/// sum of the caps of every process `f` runs at once: it waits until the
/// machine can promise all of it, so the caps of everything running sit
/// below the machine's memory (BOTS.md, "The whole machine"). All or
/// nothing, so runs waiting never hold part of it, and in arrival order.
pub fn with_isolation<R>(
    memory: u64,
    f: impl FnOnce(Option<&crate::keyholder::Isolation>) -> R,
) -> R {
    with_isolation_as(memory, Turn::Queued, f)
}

/// [`with_isolation`], waiting as `turn` says. The wait and the run block
/// their thread, so on the runtime's worker they're moved off it first: a
/// wait behind a large race never takes a worker from the rest.
pub fn with_isolation_as<R>(
    memory: u64,
    turn: Turn,
    f: impl FnOnce(Option<&crate::keyholder::Isolation>) -> R,
) -> R {
    let Some(i) = ISOLATED.get() else {
        return f(None);
    };
    blocking(|| {
        let promised = i.promise_as(memory, turn);
        let iso = crate::keyholder::Isolation {
            spawner: crate::worker::Spawner {
                exe: i.exe.clone(),
                core: promised.core,
                strict: i.strict,
            },
            clock: i.clock.clone(),
        };
        f(Some(&iso))
    })
}

/// Run blocking work off the async runtime's worker where there is one.
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Play a job's session: isolated where the runner isolates, in-process
/// (the local default) otherwise.
pub fn play_job(held: &Held, seed: &[u8; 32], limits: &Limits) -> Played {
    let memory = botstadia_host::budget::version(held.payload.budget_version)
        .map(|b| crate::worker::session_memory(b, held.bundles.len()))
        .unwrap_or(0);
    with_isolation(memory, |iso| match iso {
        Some(iso) => crate::keyholder::play(held, seed, iso),
        None => play(held, seed, limits),
    })
}

/// Play a held session under `seed`, every seat in this process.
pub fn play(held: &Held, seed: &[u8; 32], limits: &Limits) -> Played {
    let n = held.bundles.len();
    let order = seats(seed, n);
    let Some(budget) = botstadia_host::budget::version(held.payload.budget_version) else {
        return Played::void(VoidReason::EnclaveFailure, order);
    };
    let arena = match LoadedArena::from_bytes(&held.arena) {
        Ok(a) => a,
        Err(_) => return Played::void(VoidReason::ArenaTrap, order),
    };
    let budgets = arena.declaration.budgets;
    // Every entrant loads and compiles before any of its code runs.
    let mut programs = Vec::with_capacity(n);
    for zip in &held.bundles {
        let program = botstadia_host::Bundle::from_zip(zip, budget)
            .and_then(|b| botstadia_host::seat::Program::new(b, budget, budgets.turn_fuel));
        match program {
            Ok(p) => programs.push(Arc::new(p)),
            Err(_) => return Played::void(VoidReason::EntrantUnavailable, order),
        }
    }
    let seated: Vec<Seat> = order
        .iter()
        .map(|&pos| Seat::Bot(programs[pos].clone()))
        .collect();
    let limit = limits.session(
        arena.declaration.max_turns as u64,
        n as u64,
        budgets.turn_fuel,
        budgets.init_fuel,
    );
    let (mut session, first) =
        match Session::start(&arena, seated, seed, budget, SessionOptions::default()) {
            Ok(s) => s,
            Err(e) => return Played::void(classify(&e), order),
        };
    let fuel = |out: &botstadia_host::session::StepOut| -> u64 {
        out.arena_fuel.unwrap_or(0)
            + out
                .seats
                .iter()
                .map(|s| s.wasm_fuel + s.inference_fuel)
                .sum::<u64>()
    };
    let mut incidents = Vec::new();
    let mut note = |out: &botstadia_host::session::StepOut| {
        for (seat, t) in out.seats.iter().enumerate() {
            if let Some(why) = &t.trapped {
                incidents.push(format!(
                    "seat {seat} (entrant {}) on turn {}: {why}",
                    order[seat], out.turn
                ));
            }
        }
    };
    note(&first);
    let mut spent = fuel(&first);
    let mut finish = first.finish.clone();
    let none: Vec<Option<Vec<u8>>> = vec![None; n];
    while finish.is_none() {
        let out = match session.step(&none) {
            Ok(o) => o,
            Err(e) => return Played::void(classify(&e), order),
        };
        note(&out);
        spent += fuel(&out);
        if spent > limit {
            return Played::void(VoidReason::SafetyLimit, order);
        }
        finish = out.finish;
    }
    let finish = finish.expect("the loop ends on a finish");
    // Places by seat back to places by spec position.
    let groups: Vec<Vec<usize>> = finish
        .places
        .iter()
        .map(|g| g.iter().map(|&seat| order[seat as usize]).collect())
        .collect();
    let outcome = match Outcome::from_groups(n, &groups) {
        Ok(o) => o,
        Err(_) => return Played::void(VoidReason::EnclaveFailure, order),
    };
    match session.record() {
        Ok((archive, metrics)) => Played {
            outcome,
            seats: order,
            archive: archive.encode(),
            metrics: serde_json::to_vec(&metrics).unwrap_or_default(),
            incidents,
        },
        Err(_) => Played::void(VoidReason::EnclaveFailure, order),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated(total: u64, cores: Vec<usize>) -> Isolated {
        Isolated {
            exe: "/nonexistent".into(),
            strict: false,
            clock: crate::canary::Ticks::frozen(),
            cores: std::sync::Mutex::new(cores),
            memory: std::sync::Mutex::new(Promises {
                free: total,
                next_ticket: 0,
                serving: 0,
            }),
            freed: std::sync::Condvar::new(),
            total,
        }
    }

    #[test]
    fn a_panicked_run_gives_back_its_memory_and_its_core() {
        let i = isolated(10, vec![3]);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let p = i.promise(8);
            assert_eq!(p.core, Some(3));
            panic!("a job's panic");
        }));
        assert!(r.is_err());
        assert_eq!(i.memory.lock().unwrap().free, 10);
        assert_eq!(*i.cores.lock().unwrap(), vec![3]);
        // And the next run gets all of it.
        let p = i.promise(10);
        assert_eq!(p.memory, 10);
    }

    #[test]
    fn runs_take_memory_in_arrival_order_so_a_large_one_is_never_overtaken() {
        let i = std::sync::Arc::new(isolated(10, vec![]));
        let first = i.promise(6);
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // A large run arrives, then a small one that would fit at once (4 of
        // the 4 left). Once the large one has its 8, the small one no longer
        // fits beside it, so the order recorded is the order served (CI found
        // the two racing to record when both fit at once).
        let spawn = |memory: u64, name: &'static str| {
            let (i, order) = (i.clone(), order.clone());
            std::thread::spawn(move || {
                let _p = i.promise(memory);
                order.lock().unwrap().push(name);
            })
        };
        let large = spawn(8, "large");
        while i.memory.lock().unwrap().next_ticket < 2 {
            std::thread::yield_now();
        }
        let small = spawn(4, "small");
        while i.memory.lock().unwrap().next_ticket < 3 {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            order.lock().unwrap().is_empty(),
            "the small run waits its turn"
        );
        drop(first);
        large.join().unwrap();
        small.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["large", "small"]);
    }

    /// Found in review: an anchor-time recheck waited behind a large race
    /// for a whole session, so its anchor came late.
    #[test]
    fn a_brief_run_takes_memory_that_fits_ahead_of_a_waiting_large_one() {
        let i = std::sync::Arc::new(isolated(10, vec![]));
        let first = i.promise(6);
        let large = {
            let i = i.clone();
            std::thread::spawn(move || {
                let _p = i.promise(8);
            })
        };
        while i.memory.lock().unwrap().next_ticket < 2 {
            std::thread::yield_now();
        }
        // The large run waits for memory; a brief one that fits goes now.
        let brief = i.promise_as(2, Turn::Brief);
        assert_eq!(i.memory.lock().unwrap().free, 2);
        drop(brief);
        drop(first);
        large.join().unwrap();
        assert_eq!(i.memory.lock().unwrap().free, 10);
    }
}
