//! The runner's step loop (TRUST.md#how-the-enclave-talks-to-the-chain).
//!
//! Everything the host could read timing from happens only at batch-window
//! boundaries: the heartbeat, asking for work, and reporting finished jobs.
//! So a session's end time, which can hint at its outcome, never shows
//! between boundaries. Jobs run on their own threads; [`Runner::step`] is
//! called on a fixed cadence and does the boundary work when one is crossed.
//!
//! Draining stops asking for work, lets every held job finish, reports them
//! in the order they were claimed, and deregisters. A job given back before
//! its anchor (a session's release) is reported at the next boundary, out of
//! turn: it has no outcome to hint at, and another machine must get it before
//! close. A job's report that fails is tried again at the next boundary, and
//! every claim lists the jobs held, so a claim whose answer was lost is
//! offered again.

use std::sync::mpsc;

use botstadia_shared::api::Empty;
use botstadia_shared::error::ApiError;
use botstadia_shared::platform::{
    ClaimAsk, FleetClaim, FleetDeregister, FleetFinish, FleetHeartbeat, JobDone, JobOffer,
};
use serde_json::{Value, json};

use crate::client::{Machine, Transport};

/// What a job came to: `done`, `failed`, `fault` or `release`, and its
/// result JSON for the api (an upload report, a rekey verdict, a reason).
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub outcome: String,
    pub result: Value,
}

/// Runs one job to its end, on its own thread. Sessions, warm-ups,
/// verification and rekey checks each get one.
pub trait Executor: Send + Sync + 'static {
    fn run(&self, offer: &JobOffer) -> Outcome;
}

/// What a job that panicked reports: no verdict, since the panic is ours.
/// A session is given back (the api ends it failed if it was anchored, and
/// offers it again if not); any other job is a fault, queued again and
/// parked on its second (BOTS.md#bundles).
fn panicked(offer: &JobOffer) -> Outcome {
    Outcome {
        outcome: if offer.kind == "session" {
            "release"
        } else {
            "fault"
        }
        .into(),
        result: json!({"why": botstadia_shared::platform::JOB_PANICKED}),
    }
}

struct Held {
    offer: JobOffer,
    done: mpsc::Receiver<Outcome>,
    finished: Option<Outcome>,
}

/// What one step did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Step {
    pub boundary: bool,
    pub claimed: Option<String>,
    pub reported: Vec<String>,
    pub deregistered: bool,
}

pub struct Runner<T, E> {
    pub machine: std::sync::Arc<Machine<T>>,
    executor: std::sync::Arc<E>,
    window_ms: u64,
    slots: usize,
    kinds: Vec<String>,
    last_window: Option<u64>,
    held: Vec<Held>,
    draining: bool,
    gone: bool,
    /// Whether it may take work: its SubmitKey approved and funded
    /// (TRUST.md#submitkey-lifecycle).
    pub ready: bool,
    /// The rest of the heartbeat, kept current by the other parts (the
    /// BundleKey, approval, balance, held results, providers' health).
    pub status: serde_json::Map<String, Value>,
}

impl<T: Transport, E: Executor> Runner<T, E> {
    pub fn new(
        machine: std::sync::Arc<Machine<T>>,
        executor: E,
        window_ms: u64,
        slots: usize,
        kinds: &[&str],
    ) -> Self {
        Self {
            machine,
            executor: std::sync::Arc::new(executor),
            window_ms: window_ms.max(1),
            slots: slots.max(1),
            kinds: kinds.iter().map(|k| k.to_string()).collect(),
            last_window: None,
            held: Vec::new(),
            draining: false,
            gone: false,
            ready: false,
            status: serde_json::Map::new(),
        }
    }

    /// Stop taking work; [`step`](Self::step) finishes the rest and
    /// deregisters.
    pub fn drain(&mut self) {
        self.draining = true;
    }

    /// Drain holding nothing, and with `nothing_else_held` (no result
    /// waiting for its batch): the next [`step`](Self::step) deregisters at
    /// once instead of at the next boundary. A machine's stop grants a
    /// couple of minutes, often less than a window (found on the testnet:
    /// stopped at 10:00:25, powered off before the 10:03 boundary, never
    /// deregistered, so taken for an unexpected stop). With nothing held,
    /// no end time is revealed by not waiting.
    pub fn drain_idle(&mut self, nothing_else_held: bool) {
        self.draining = true;
        if nothing_else_held && self.held.is_empty() {
            self.last_window = None;
        }
    }

    pub fn is_gone(&self) -> bool {
        self.gone
    }

    pub fn held(&self) -> usize {
        self.held.len()
    }

    fn heartbeat_body(&self, now: u64) -> Value {
        let mut body = self.status.clone();
        // Load and queue as of this boundary only.
        body.insert("load".into(), json!(self.held.len()));
        body.insert("slots".into(), json!(self.slots));
        body.insert("draining".into(), json!(self.draining));
        body.insert("boundary".into(), json!(now - now % self.window_ms));
        Value::Object(body)
    }

    /// Report the finished job at `i`. The api's answer settles it: not
    /// found means it was already applied (an earlier answer was lost), and
    /// a refusal of the report itself won't change by trying again. Any
    /// other error leaves it to try at the next boundary.
    async fn report(&self, i: usize) -> anyhow::Result<()> {
        let h = &self.held[i];
        let o = h.finished.as_ref().expect("finished");
        let sent: anyhow::Result<Empty> = self
            .machine
            .call::<FleetFinish, _, _>(JobDone {
                id: h.offer.id.clone(),
                outcome: o.outcome.clone(),
                result: o.result.to_string(),
            })
            .await;
        match sent {
            Ok(_) => Ok(()),
            Err(e) => match e.downcast_ref::<ApiError>() {
                Some(ApiError::NotFound | ApiError::Validation { .. }) => {
                    eprintln!("runner: report of {} settled: {e:#}", h.offer.id);
                    Ok(())
                }
                _ => Err(e),
            },
        }
    }

    pub async fn step(&mut self, now: u64) -> anyhow::Result<Step> {
        let mut out = Step::default();
        if self.gone {
            return Ok(out);
        }
        // Collect what finished; nothing is said about it until a boundary.
        // A job whose thread died without sending (a panic past the guard
        // below) is finished too: held forever, it would leak its slot and
        // stall every report behind it.
        for h in &mut self.held {
            if h.finished.is_none() {
                match h.done.try_recv() {
                    Ok(o) => h.finished = Some(o),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        eprintln!("runner: job {} stopped without an outcome", h.offer.id);
                        h.finished = Some(panicked(&h.offer));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }
        let window = now / self.window_ms;
        if self.last_window == Some(window) {
            return Ok(out);
        }
        self.last_window = Some(window);
        out.boundary = true;

        // A session given back before its anchor reveals no outcome, so it's
        // reported at once, letting another machine take it before close.
        // Every other finished job is reported in claim order, stopping at
        // the first still running, so no end time shows out of turn. A job
        // leaves `held` only once its report is through, so a failed report
        // is tried again at the next boundary.
        let mut i = 0;
        while i < self.held.len() {
            let h = &self.held[i];
            if h.offer.kind == "session"
                && h.finished.as_ref().is_some_and(|o| o.outcome == "release")
            {
                self.report(i).await?;
                out.reported.push(self.held.remove(i).offer.id);
            } else {
                i += 1;
            }
        }
        while self.held.first().is_some_and(|h| h.finished.is_some()) {
            self.report(0).await?;
            out.reported.push(self.held.remove(0).offer.id);
        }

        if self.draining && self.held.is_empty() {
            let _: Empty = self.machine.call::<FleetDeregister, _, _>(Empty {}).await?;
            self.gone = true;
            out.deregistered = true;
            return Ok(out);
        }

        let _: Empty = self
            .machine
            .call::<FleetHeartbeat, _, _>(self.heartbeat_body(now))
            .await?;

        if self.ready && !self.draining && self.held.len() < self.slots {
            let offer: Option<JobOffer> = self
                .machine
                .call::<FleetClaim, _, _>(ClaimAsk {
                    kinds: self.kinds.clone(),
                    held: self.held.iter().map(|h| h.offer.id.clone()).collect(),
                })
                .await?;
            if let Some(offer) = offer {
                let (tx, rx) = mpsc::channel();
                let executor = self.executor.clone();
                let job = offer.clone();
                std::thread::spawn(move || {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        executor.run(&job)
                    }))
                    .unwrap_or_else(|_| {
                        eprintln!("runner: job {} panicked", job.id);
                        panicked(&job)
                    });
                    let _ = tx.send(outcome);
                });
                out.claimed = Some(offer.id.clone());
                self.held.push(Held {
                    offer,
                    done: rx,
                    finished: None,
                });
            }
        }
        Ok(out)
    }
}
