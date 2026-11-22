//! Anchoring (TRUST.md#every-session-one-flow, step 3): once a block at
//! least the anchor margin past close is included, the enclave puts every
//! session it holds in one anchor batch. The contract stores the previous
//! block's hash, which nobody could know in advance, and binds the session
//! to this key. The session runs only once that anchor is final.

use alloy_sol_types::SolCall;
use botstadia_chain::abi::Results;
use botstadia_chain::events::{Contracts, Event, decode};
use botstadia_chain::{Address, B256, Bytes};

use crate::chain_view::ChainView;
use crate::hold::Held;
use crate::submit::ChainSubmitter;

/// The contract's margin past close (seconds).
pub const ANCHOR_MARGIN_S: u64 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchored {
    pub session: B256,
    pub key: Address,
    pub anchor: B256,
    /// The block whose hash is the anchor: the one before the anchor's own
    /// transaction (`blockhash(block.number - 1)`).
    pub block: u64,
}

impl Anchored {
    /// The block holding the anchor's transaction. The binding is final
    /// only once this one is: read at `block` alone, the session is still
    /// unanchored, which looks like a rollback.
    pub fn included_in(&self) -> u64 {
        self.block + 1
    }
}

/// Whether a block far enough past `close` has been included.
pub async fn past_close(submitter: &ChainSubmitter, close: u64) -> anyhow::Result<bool> {
    let head = crate::chain_view::head_time(&submitter.providers).await?;
    Ok(head >= close + ANCHOR_MARGIN_S)
}

/// A held session's anchor item. A final carries its week's field entry, so
/// its anchor binds the week (TRUST.md#the-finals-gate); a league session
/// carries none.
pub fn item(h: &Held) -> Results::AnchorItem {
    let fin = h
        .week
        .as_ref()
        .filter(|w| w.round == botstadia_chain::entry::ROUND_FINAL);
    Results::AnchorItem {
        post: h.payload.post,
        entry: Bytes::from(h.entry.clone()),
        proof: h.proof.clone(),
        fieldEntry: fin.map_or_else(Bytes::new, |w| Bytes::from(w.field_entry.clone())),
    }
}

/// The sessions this boot has begun to run. An enclave never restarts a
/// bound session (TRUST.md#every-session-one-flow), and a key lives one
/// boot: so whatever is offered twice, or recovered twice from the chain,
/// draws one seed.
#[derive(Default)]
pub struct RunOnce(std::sync::Mutex<std::collections::HashSet<B256>>);

impl RunOnce {
    pub fn begin(&self, session: B256) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.0.lock().expect("sessions run").insert(session),
            "this boot has already begun that session: never twice"
        );
        Ok(())
    }
}

/// Whether a session anchored at `now` could still have its result land
/// before its deadline (TRUST.md: an enclave checks the current finality
/// lag before it anchors). The session runs once its anchor is final (the
/// lag) and past the minimum age after it, which runs alongside and is
/// waited before play, then takes at most the longest session the arena
/// allows; and the anchor's and the result's batches each wait up to a
/// window. The contract judges the deadline when the result is included, so
/// the result's own finality isn't needed.
pub fn room_to_anchor(now: u64, close: u64, lag_s: u64, longest_s: u64, window_s: u64) -> bool {
    let needed = lag_s.max(crate::executor::RESULT_MIN_AGE_S) + longest_s + 2 * window_s;
    now.saturating_add(needed) <= close + crate::constants::RESULT_DEADLINE_S
}

/// Send one anchor batch for every held session; returns the anchors the
/// contract accepted (a rejected entry is skipped with an event).
pub async fn anchor_batch(
    submitter: &ChainSubmitter,
    held: &[&Held],
) -> anyhow::Result<(Vec<Anchored>, Vec<(B256, String)>)> {
    let items: Vec<Results::AnchorItem> = held.iter().map(|h| item(h)).collect();
    let receipt = submitter
        .send(
            submitter.results,
            Results::anchorCall { items }.abi_encode(),
            100,
        )
        .await?;
    let contracts = Contracts {
        results: submitter.results,
        settlement: Address::ZERO,
        standing_orders: Address::ZERO,
    };
    let mut anchored = Vec::new();
    let mut rejected = Vec::new();
    for log in &receipt.logs {
        match decode(&contracts, log) {
            Some(Event::Anchored(a)) => anchored.push(Anchored {
                session: a.session,
                key: a.key,
                anchor: a.anchor,
                block: a.blockNumber,
            }),
            Some(Event::ResultsRejected(r)) => rejected.push((r.id, r.reason)),
            _ => {}
        }
    }
    Ok((anchored, rejected))
}

/// Wait until `block` is final (as two providers report it).
pub async fn wait_final<F: std::future::Future<Output = ()>>(
    view: &impl ChainView,
    block: u64,
    pause: impl Fn() -> F,
    polls: usize,
) -> anyhow::Result<()> {
    let mut last = None;
    for _ in 0..polls {
        match view.finalized().await {
            Ok(n) if n >= block => return Ok(()),
            Ok(_) => {}
            // Bound by then: a provider's bad minute is waited out.
            Err(e) => last = Some(e),
        }
        pause().await;
    }
    anyhow::bail!("block {block} isn't final yet (last error: {last:?})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_boot_runs_a_session_once_however_many_jobs_hold_it() {
        let ran = std::sync::Arc::new(RunOnce::default());
        let session = B256::repeat_byte(7);
        let jobs: Vec<_> = (0..2)
            .map(|_| {
                let ran = ran.clone();
                std::thread::spawn(move || ran.begin(session).is_ok())
            })
            .collect();
        let began = jobs
            .into_iter()
            .map(|j| j.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(began, 1, "two jobs, one run");
        assert!(ran.begin(session).is_err(), "nor after the first has ended");
        assert!(ran.begin(B256::repeat_byte(8)).is_ok());
    }

    #[test]
    fn an_anchor_is_final_with_its_transactions_block() {
        let a = Anchored {
            session: B256::ZERO,
            key: Address::ZERO,
            anchor: B256::ZERO,
            block: 41,
        };
        assert_eq!(a.included_in(), 42);
    }

    #[test]
    fn a_lagging_chain_leaves_no_room_to_anchor() {
        let close = 1_000_000;
        let deadline = crate::constants::RESULT_DEADLINE_S;
        assert!(room_to_anchor(close, close, 60, 600, 180));
        assert!(!room_to_anchor(close, close, deadline, 600, 180));
        // The minimum age runs alongside the finality wait, then it plays.
        let age = crate::executor::RESULT_MIN_AGE_S;
        let late = close + deadline - age - age / 2 - 2 * 180;
        assert!(room_to_anchor(late, close, age / 2, age / 2, 180));
        assert!(!room_to_anchor(late + 1, close, age / 2, age / 2, 180));
        assert!(!room_to_anchor(close + deadline, close, 0, 0, 0));
    }
}
