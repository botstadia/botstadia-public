//! Result batches against a fake chain that holds any transaction over a
//! size, as Base's sequencer does under data-availability pressure.

use std::cell::RefCell;
use std::collections::HashMap;

use alloy_sol_types::SolEvent;
use botstadia_chain::abi::Results;
use botstadia_chain::rpc::{Log, Receipt};
use botstadia_chain::{Address, B256, Bytes, keccak256};
use botstadia_enclave::batch::{BatchLimits, Batcher, Entry};
use botstadia_enclave::submit::ChainTx;

#[derive(Default)]
struct Fake {
    hold_over: usize,
    reject: Option<B256>,
    /// Sessions the contract anchors in a batch that lands.
    anchors: Vec<B256>,
    sent: RefCell<Vec<(u64, usize)>>,
    landed: RefCell<HashMap<B256, Receipt>>,
    nonce: RefCell<u64>,
    /// Sends refused outright before any lands (a provider outage, or a key
    /// the contract no longer takes).
    refuse: RefCell<u32>,
}

impl ChainTx for Fake {
    async fn next_nonce(&self) -> anyhow::Result<u64> {
        Ok(*self.nonce.borrow())
    }
    async fn send_at(
        &self,
        nonce: u64,
        to: Address,
        data: Vec<u8>,
        level: u32,
    ) -> anyhow::Result<B256> {
        if *self.refuse.borrow() > 0 {
            *self.refuse.borrow_mut() -= 1;
            anyhow::bail!("execution reverted: KeyNotActive");
        }
        let hash =
            keccak256([data.as_slice(), &nonce.to_be_bytes(), &level.to_be_bytes()].concat());
        self.sent.borrow_mut().push((nonce, data.len()));
        if data.len() <= self.hold_over {
            let mut logs = Vec::new();
            if let Some(id) = self.reject {
                let e = Results::Rejected {
                    id,
                    reason: "already resolved".into(),
                };
                let d = e.encode_log_data();
                logs.push(Log {
                    address: to,
                    topics: d.topics().to_vec(),
                    data: d.data.clone(),
                    block_number: Some(7),
                    transaction_hash: Some(hash),
                    log_index: Some(0),
                });
            }
            for (i, session) in self.anchors.iter().enumerate() {
                let e = Results::Anchored {
                    session: *session,
                    key: Address::repeat_byte(9),
                    anchor: B256::repeat_byte(i as u8 + 1),
                    blockNumber: 6,
                    specHash: B256::ZERO,
                };
                let d = e.encode_log_data();
                logs.push(Log {
                    address: to,
                    topics: d.topics().to_vec(),
                    data: d.data.clone(),
                    block_number: Some(7),
                    transaction_hash: Some(hash),
                    log_index: Some(1 + i as u64),
                });
            }
            self.landed.borrow_mut().insert(
                hash,
                Receipt {
                    transaction_hash: hash,
                    block_number: 7,
                    status: 1,
                    gas_used: 1,
                    logs,
                },
            );
            *self.nonce.borrow_mut() = nonce + 1;
        }
        Ok(hash)
    }
    async fn receipt(&self, hash: B256) -> anyhow::Result<Option<Receipt>> {
        Ok(self.landed.borrow().get(&hash).cloned())
    }
}

fn rating(i: u8) -> Entry {
    Entry::Rating(Results::RatingItem {
        bot: B256::repeat_byte(i),
        ladder: B256::ZERO,
        mu: 1,
        sigma: 1,
        ratingVersion: 1,
        arenaDigest: B256::ZERO,
        budgetVersion: 1,
        opponentListHash: B256::ZERO,
        signature: Bytes::from(vec![i; 65]),
    })
}

fn limits() -> BatchLimits {
    BatchLimits {
        window_ms: 1000,
        size: 40,
        age_ms: 10_000,
        hold_polls: 2,
        give_up_ms: 60_000,
    }
}

#[tokio::test]
async fn a_held_batch_is_replaced_by_a_smaller_one_at_the_same_nonce_until_all_land() {
    // Holds anything over two ratings' worth.
    let fake = Fake {
        hold_over: 1_000,
        ..Default::default()
    };
    let b = Batcher::new(Address::repeat_byte(5), limits());
    let rx: Vec<_> = (1..=8).map(|i| b.queue(rating(i), 0)).collect();
    assert_eq!(b.held(2500), (8, 2));
    assert!(b.due(1, true) && !b.due(1, false));
    let landed = b.flush(&fake, 0, tokio::task::yield_now).await.unwrap();
    assert_eq!(landed, 8);
    for r in rx {
        assert_eq!(r.try_recv().unwrap(), Ok(7));
    }
    let sent = fake.sent.borrow();
    assert_eq!(
        sent[0].0, sent[1].0,
        "the replacement takes the held send's nonce"
    );
    assert!(sent[1].1 < sent[0].1, "and is smaller");
    assert_eq!(b.held(0).0, 0);
}

#[tokio::test]
async fn a_rejected_entry_is_told_why_and_the_rest_land() {
    let fake = Fake {
        hold_over: usize::MAX,
        reject: Some(B256::repeat_byte(2)),
        ..Default::default()
    };
    let b = Batcher::new(Address::repeat_byte(5), limits());
    let one = b.queue(rating(1), 0);
    let two = b.queue(rating(2), 0);
    b.flush(&fake, 0, tokio::task::yield_now).await.unwrap();
    assert_eq!(one.try_recv().unwrap(), Ok(7));
    assert_eq!(two.try_recv().unwrap(), Err("already resolved".into()));
}

#[test]
fn a_batch_is_due_at_its_size_or_its_age_whatever_the_window() {
    let b = Batcher::new(
        Address::ZERO,
        BatchLimits {
            window_ms: 1000,
            size: 2,
            age_ms: 500,
            hold_polls: 1,
            give_up_ms: 60_000,
        },
    );
    let _a = b.queue(rating(1), 0);
    assert!(!b.due(100, false));
    assert!(b.due(600, false), "the age limit");
    let _b = b.queue(rating(2), 600);
    assert!(b.due(601, false), "the size limit");
}

fn anchor(i: u8) -> Entry {
    Entry::Anchor {
        session: B256::repeat_byte(i),
        item: Results::AnchorItem {
            post: 1,
            entry: Bytes::from(vec![i; 40]),
            proof: vec![],
            fieldEntry: Bytes::new(),
        },
    }
}

/// Anchors go out as one anchor batch, and each session learns its anchor
/// from the batch's events; one the contract didn't anchor is refused.
#[tokio::test]
async fn anchors_land_in_one_batch_with_their_anchors() {
    use alloy_sol_types::SolCall;
    let fake = Fake {
        hold_over: usize::MAX,
        anchors: vec![B256::repeat_byte(1), B256::repeat_byte(2)],
        ..Fake::default()
    };
    let b = Batcher::new(Address::repeat_byte(5), limits());
    let r1 = b.queue(anchor(1), 0);
    let r2 = b.queue(anchor(2), 0);
    let r3 = b.queue(anchor(3), 0);
    b.flush(&fake, 0, || async {}).await.unwrap();
    assert_eq!(fake.sent.borrow().len(), 1, "one batch");
    assert_eq!(r1.recv().unwrap(), Ok(7));
    assert_eq!(r2.recv().unwrap(), Ok(7));
    assert!(r3.recv().unwrap().is_err());
    assert_eq!(
        b.anchor_of(B256::repeat_byte(2)),
        Some((B256::repeat_byte(2), 6))
    );
    assert_eq!(b.anchor_of(B256::repeat_byte(3)), None);
    let _ = Results::anchorCall::SELECTOR;
}

/// Found on the testnet: a failed send dropped its whole batch, so a
/// provider outage of a moment lost every result in it.
#[tokio::test]
async fn a_failed_send_keeps_its_batch_for_the_next_window() {
    let fake = Fake {
        hold_over: usize::MAX,
        refuse: RefCell::new(2),
        ..Default::default()
    };
    let b = Batcher::new(Address::repeat_byte(5), limits());
    let one = b.queue(rating(1), 0);
    let two = b.queue(rating(2), 0);
    for _ in 0..2 {
        assert!(b.flush(&fake, 0, tokio::task::yield_now).await.is_err());
        assert_eq!(b.held(0).0, 2, "kept, in order");
        assert!(one.try_recv().is_err() && two.try_recv().is_err());
    }
    assert_eq!(b.flush(&fake, 0, tokio::task::yield_now).await.unwrap(), 2);
    assert_eq!(one.try_recv().unwrap(), Ok(7));
    assert_eq!(two.try_recv().unwrap(), Ok(7));
}

/// A revoked key's sends always fail: its jobs end, told why.
#[tokio::test]
async fn an_entry_that_never_sends_ends_its_job_with_the_reason() {
    let fake = Fake {
        hold_over: usize::MAX,
        refuse: RefCell::new(u32::MAX),
        ..Default::default()
    };
    let b = Batcher::new(Address::repeat_byte(5), limits());
    let one = b.queue(rating(1), 0);
    // Kept trying until it could no longer land.
    assert!(
        b.flush(&fake, 59_999, tokio::task::yield_now)
            .await
            .is_err()
    );
    assert_eq!(b.held(0).0, 1);
    assert!(
        b.flush(&fake, 60_000, tokio::task::yield_now)
            .await
            .is_err()
    );
    assert_eq!(b.held(0).0, 0);
    let why = one.try_recv().unwrap().unwrap_err();
    assert!(why.contains("KeyNotActive"), "{why}");
}

#[tokio::test]
async fn a_failed_batch_waits_for_its_window() {
    let fake = Fake {
        hold_over: 100_000,
        refuse: RefCell::new(1),
        ..Default::default()
    };
    let limits = BatchLimits {
        window_ms: 1_000,
        size: 40,
        age_ms: 500,
        hold_polls: 2,
        give_up_ms: 60_000,
    };
    let b = Batcher::new(Address::repeat_byte(5), limits);
    let _rx = b.queue(rating(1), 0);
    assert!(b.due(600, false), "its age");
    assert!(b.flush(&fake, 600, tokio::task::yield_now).await.is_err());
    assert!(!b.due(850, false), "never again on the loop's next turn");
    assert!(b.due(1_000, true), "at the boundary");
    // The runner's boundary is consumed by a failing heartbeat: the next
    // window sends without one (a batch can never stall, and the api is
    // never in a result's path).
    assert!(b.due(1_001, false), "the next window, boundary or not");
}

#[tokio::test]
async fn an_old_failure_never_sends_a_new_entry_at_once() {
    let fake = Fake {
        hold_over: 100_000,
        refuse: RefCell::new(1),
        ..Default::default()
    };
    let limits = BatchLimits {
        window_ms: 1_000,
        size: 40,
        age_ms: 500,
        hold_polls: 2,
        give_up_ms: 100,
    };
    let b = Batcher::new(Address::repeat_byte(5), limits);
    let _rx = b.queue(rating(1), 0);
    // Given up: the queue empties, the failure stands.
    assert!(b.flush(&fake, 600, tokio::task::yield_now).await.is_err());
    assert_eq!(b.held(600).0, 0);
    let _rx = b.queue(rating(2), 5_100);
    assert!(
        !b.due(5_100, false),
        "a send now would say when the session ended"
    );
    assert!(b.due(5_100, true), "its window does");
}
