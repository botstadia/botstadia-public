//! The hold's post rules (TRUST.md#only-rated-bots-play-bettable-sessions,
//! #approving-builds): the earliest post a first bet could prove against,
//! and the arena version's age, each from post lists hashed against roots.

use botstadia_chain::entry::Entry;
use botstadia_chain::{B256, merkle};
use botstadia_enclave::chain_view::FakeChain;
use botstadia_enclave::constants::{ARENA_WAIT_S, CLOSE_MIN_AFTER_POST_S, POST_LOOKBACK_S};
use botstadia_enclave::hold::{PostList, SessionPayload, arena_posted, earliest_post};

const CLOSE: u64 = 2_000_000_000;

fn hexs(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Post `number` at `time` holding `entries`, on the chain and as a list.
fn post(chain: &FakeChain, number: u64, time: u64, entries: &[Vec<u8>]) -> PostList {
    chain
        .posts
        .borrow_mut()
        .insert(number, (merkle::root(entries), time));
    PostList {
        number,
        entries: entries.iter().map(|e| hexs(e)).collect(),
    }
}

fn session_payload(post: u64, window: Vec<PostList>) -> SessionPayload {
    let spec = botstadia_chain::spec::Spec {
        digests: vec![],
        payouts: vec![],
        arena: B256::ZERO,
        chain_id: 1,
        results: botstadia_chain::Address::ZERO,
        nonce: B256::ZERO,
        budget_version: 1,
    };
    SessionPayload {
        session_id: "0x00".into(),
        spec,
        spec_hash: "0x".into(),
        entry: "0x".into(),
        post,
        proof: vec![],
        close: CLOSE,
        arena_digest: String::new(),
        arena_id: String::new(),
        budget_version: 1,
        entrants: vec![],
        week: Default::default(),
        window,
        arena_post: None,
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn a_re_posted_session_is_judged_at_its_earliest_bettable_post() {
    let chain = FakeChain::genesis();
    let entry = b"the session".to_vec();
    let other = b"another".to_vec();
    let lookback = CLOSE - POST_LOOKBACK_S;
    let lists = vec![
        post(&chain, 10, lookback + 100, std::slice::from_ref(&other)),
        post(&chain, 11, lookback + 200, &[entry.clone(), other.clone()]),
        post(
            &chain,
            12,
            CLOSE - CLOSE_MIN_AFTER_POST_S - 50,
            std::slice::from_ref(&entry),
        ),
    ];
    post(&chain, 9, lookback - 1, std::slice::from_ref(&entry));
    let p = session_payload(12, lists);
    assert_eq!(
        block_on(earliest_post(&chain, &p, &entry)).unwrap(),
        (11, lookback + 200)
    );
}

#[test]
fn a_post_within_the_lookback_can_t_be_left_out() {
    let chain = FakeChain::genesis();
    let entry = b"the session".to_vec();
    let lookback = CLOSE - POST_LOOKBACK_S;
    // Post 10 is inside the lookback and names the session, but the api
    // leaves it out, offering only 11 on: refused.
    post(&chain, 10, lookback + 100, std::slice::from_ref(&entry));
    let lists = vec![post(
        &chain,
        11,
        lookback + 200,
        std::slice::from_ref(&entry),
    )];
    let p = session_payload(11, lists);
    let e = block_on(earliest_post(&chain, &p, &entry)).unwrap_err();
    assert!(e.to_string().contains("wasn't given"), "{e}");
}

#[test]
fn a_session_posted_too_late_for_a_bet_or_a_forged_list_is_never_held() {
    let chain = FakeChain::genesis();
    let entry = b"the session".to_vec();
    let late = CLOSE - CLOSE_MIN_AFTER_POST_S + 1;
    let lists = vec![post(&chain, 1, late, std::slice::from_ref(&entry))];
    let p = session_payload(1, lists);
    assert!(block_on(earliest_post(&chain, &p, &entry)).is_err());

    let chain = FakeChain::genesis();
    post(
        &chain,
        1,
        CLOSE - POST_LOOKBACK_S + 5,
        &[b"something else".to_vec()],
    );
    let forged = PostList {
        number: 1,
        entries: vec![hexs(&entry)],
    };
    let p = session_payload(1, vec![forged]);
    let e = block_on(earliest_post(&chain, &p, &entry)).unwrap_err();
    assert!(e.to_string().contains("isn't its root"), "{e}");
}

#[test]
fn an_arena_counts_from_the_post_that_announced_it() {
    let chain = FakeChain::genesis();
    let digest = B256::repeat_byte(7);
    let announce = Entry::Arena {
        digest,
        ladder_id: B256::repeat_byte(1),
        payload_schema: 2,
    }
    .encode();
    let list = post(&chain, 4, 1_000, &[announce]);
    assert_eq!(
        block_on(arena_posted(&chain, Some(&list), digest)).unwrap(),
        1_000
    );
    assert!(block_on(arena_posted(&chain, Some(&list), B256::repeat_byte(8))).is_err());
    assert!(block_on(arena_posted(&chain, None, digest)).is_err());
    const { assert!(ARENA_WAIT_S >= 600) };
}
