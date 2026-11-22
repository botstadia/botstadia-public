//! Result batches (TRUST.md#submitting-results): one transaction per window
//! carrying every result and final rating the enclave holds.
//!
//! - A batch goes when its window ends, when it reaches the size limit, or
//!   when its oldest entry reaches the age limit, whatever the gas price.
//! - One not included soon after a send priced to land is replaced at the
//!   same nonce by a smaller one, the rest following in the next, down to
//!   one entry.
//! - The contract skips an entry it won't take with a rejection event; the
//!   rest land, and each entry's job learns which.
//! - A job waits for its entry's inclusion: a session uploads its archive
//!   only then, a warm-up finishes only then.
//! - A send no provider takes, or that reverts, keeps its batch at the front
//!   for the next window, until its entries could no longer land.

use std::sync::{Mutex, mpsc};

use alloy_sol_types::SolCall;
use botstadia_chain::abi::Results;
use botstadia_chain::events::{Contracts, Event, decode};
use botstadia_chain::{Address, B256};

use crate::submit::ChainTx;

pub enum Entry {
    Result(Results::ResultItem),
    Rating(Results::RatingItem),
    /// A session's anchor, in an anchor batch of its own (one per enclave
    /// per window, TRUST.md#how-the-enclave-talks-to-the-chain). An anchor
    /// batcher is fed nothing else.
    Anchor {
        session: B256,
        item: Results::AnchorItem,
    },
}

impl Entry {
    /// What a rejection names: a session id or a bot digest.
    fn id(&self) -> B256 {
        match self {
            Self::Result(r) => r.session,
            Self::Rating(r) => r.bot,
            Self::Anchor { session, .. } => *session,
        }
    }
}

/// An entry's fate: included in a block, or skipped with a reason.
pub type Landed = Result<u64, String>;

struct Pending {
    entry: Entry,
    queued_at: u64,
    done: mpsc::Sender<Landed>,
}

#[derive(Debug, Clone, Copy)]
pub struct BatchLimits {
    pub window_ms: u64,
    pub size: usize,
    pub age_ms: u64,
    /// Receipt polls before a send counts as held.
    pub hold_polls: usize,
    /// How long an entry whose sends fail outright is kept trying: the
    /// result deadline, past which the contract can't take it anyway. A key
    /// the contract no longer takes fails every send, so its jobs end then.
    pub give_up_ms: u64,
}

pub struct Batcher {
    queue: Mutex<Vec<Pending>>,
    pub limits: BatchLimits,
    pub results: Address,
    /// Each anchored session's anchor hash and block, from its batch's
    /// `Anchored` event (an anchor batcher only).
    anchored: Mutex<std::collections::HashMap<B256, (B256, u64)>>,
    /// The last flush failed: the next waits for its window, or a failure
    /// would be sent again on every turn of the runner's loop.
    /// Which window, plus one; zero for none.
    failed: std::sync::atomic::AtomicU64,
}

/// Replacements at one nonce before a batch waits for its next window: about
/// a minute, and fourteen times the first fee.
const MAX_LEVELS: u32 = 12;

impl Batcher {
    /// A batcher of results and ratings, or (fed only [`Entry::Anchor`]) of
    /// anchors.
    pub fn new(results: Address, limits: BatchLimits) -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            limits,
            results,
            anchored: Mutex::new(Default::default()),
            failed: Default::default(),
        }
    }

    /// The anchor a landed anchor batch gave `session`: its hash and block.
    pub fn anchor_of(&self, session: B256) -> Option<(B256, u64)> {
        self.anchored
            .lock()
            .expect("anchors")
            .get(&session)
            .copied()
    }

    /// Hold an entry for the next batch; the receiver says how it landed.
    pub fn queue(&self, entry: Entry, now: u64) -> mpsc::Receiver<Landed> {
        let (tx, rx) = mpsc::channel();
        self.queue.lock().expect("batch queue").push(Pending {
            entry,
            queued_at: now,
            done: tx,
        });
        rx
    }

    /// For the heartbeat: how many are held, and the oldest's age in whole
    /// windows (never anything about what they say).
    pub fn held(&self, now: u64) -> (usize, u64) {
        let q = self.queue.lock().expect("batch queue");
        let oldest = q.iter().map(|p| p.queued_at).min().unwrap_or(now);
        (
            q.len(),
            now.saturating_sub(oldest) / self.limits.window_ms.max(1),
        )
    }

    /// Whether a batch is due now.
    pub fn due(&self, now: u64, boundary: bool) -> bool {
        let q = self.queue.lock().expect("batch queue");
        let failed = self.failed.load(std::sync::atomic::Ordering::Relaxed);
        // Never on the api's word alone: a failing heartbeat eats the
        // boundary, and the next window still sends.
        let this_window = now / self.limits.window_ms.max(1) + 1;
        let may = failed == 0 || this_window > failed;
        !q.is_empty()
            && (boundary
                || (may
                    && (q.len() >= self.limits.size
                        || q.iter()
                            .any(|p| now.saturating_sub(p.queued_at) >= self.limits.age_ms))))
    }

    /// Send what's held, replacing a held send at the same nonce with a
    /// smaller one until everything has landed. Returns entries landed.
    pub async fn flush<F: std::future::Future<Output = ()>>(
        &self,
        tx: &impl ChainTx,
        now: u64,
        pause: impl Fn() -> F,
    ) -> anyhow::Result<usize> {
        let mut landed = 0;
        self.failed.store(0, std::sync::atomic::Ordering::Relaxed);
        loop {
            let mut batch: Vec<Pending> = {
                let mut q = self.queue.lock().expect("batch queue");
                let n = q.len().min(self.limits.size);
                q.drain(..n).collect()
            };
            if batch.is_empty() {
                return Ok(landed);
            }
            let receipt = match self.send(tx, &mut batch, &pause).await {
                Ok(r) => r,
                Err(e) => {
                    // Never dropped: back at the front for the next window,
                    // until it could no longer land (TRUST.md#submitting-
                    // results: retried until the batch lands).
                    let mut q = self.queue.lock().expect("batch queue");
                    let mut back = Vec::new();
                    for p in batch {
                        if now.saturating_sub(p.queued_at) >= self.limits.give_up_ms {
                            let _ = p.done.send(Err(format!("not sent: {e:#}")));
                        } else {
                            back.push(p);
                        }
                    }
                    q.splice(0..0, back);
                    self.failed.store(
                        now / self.limits.window_ms.max(1) + 1,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    return Err(e);
                }
            };
            let contracts = Contracts {
                results: self.results,
                settlement: Address::ZERO,
                standing_orders: Address::ZERO,
            };
            let mut rejected: Vec<(B256, String)> = Vec::new();
            for l in &receipt.logs {
                match decode(&contracts, l) {
                    Some(Event::ResultsRejected(r)) => rejected.push((r.id, r.reason)),
                    Some(Event::Anchored(a)) => {
                        self.anchored
                            .lock()
                            .expect("anchors")
                            .insert(a.session, (a.anchor, a.blockNumber));
                    }
                    _ => {}
                }
            }
            for p in batch {
                let anchor_missing = matches!(p.entry, Entry::Anchor { session, .. } if self.anchor_of(session).is_none());
                let fate = match rejected.iter().find(|(id, _)| *id == p.entry.id()) {
                    Some((_, why)) => Err(why.clone()),
                    None if anchor_missing => Err("no anchor in its batch".into()),
                    None => Ok(receipt.block_number),
                };
                let _ = p.done.send(fate);
                landed += 1;
            }
        }
    }

    /// One batch to a receipt: replaced at the same nonce by a smaller one
    /// while held, the rest back in the queue. Errs if no provider takes it
    /// or it reverts.
    async fn send<F: std::future::Future<Output = ()>>(
        &self,
        tx: &impl ChainTx,
        batch: &mut Vec<Pending>,
        pause: &impl Fn() -> F,
    ) -> anyhow::Result<botstadia_chain::rpc::Receipt> {
        let nonce = tx.next_nonce().await?;
        let mut level = 0;
        loop {
            let data = encode(batch);
            let hash = tx.send_at(nonce, self.results, data, level).await?;
            for _ in 0..self.limits.hold_polls {
                if let Some(r) = tx.receipt(hash).await? {
                    anyhow::ensure!(r.succeeded(), "a batch reverted");
                    return Ok(r);
                }
                pause().await;
            }
            // Held: a smaller batch at the same nonce, the rest next.
            anyhow::ensure!(level < MAX_LEVELS, "held through every replacement");
            level += 1;
            if batch.len() > 1 {
                let rest = batch.split_off(batch.len().div_ceil(2));
                let mut q = self.queue.lock().expect("batch queue");
                for (i, p) in rest.into_iter().enumerate() {
                    q.insert(i, p);
                }
            }
        }
    }
}

fn encode(batch: &[Pending]) -> Vec<u8> {
    let mut items = Vec::new();
    let mut ratings = Vec::new();
    let mut anchors = Vec::new();
    for p in batch {
        match &p.entry {
            Entry::Result(r) => items.push(r.clone()),
            Entry::Rating(r) => ratings.push(r.clone()),
            Entry::Anchor { item, .. } => anchors.push(item.clone()),
        }
    }
    if !anchors.is_empty() {
        return Results::anchorCall { items: anchors }.abi_encode();
    }
    Results::submitCall {
        items,
        ratingItems: ratings,
    }
    .abi_encode()
}
