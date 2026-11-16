//! Playing sessions on the real racing arena and seed bots from the CDN tree
//! (`BOTSTADIA_CDN`, set by `task chain:test-sdk`; ignored otherwise).

use botstadia_chain::outcome::{Outcome, VoidReason};
use botstadia_chain::spec::{Spec, digest_bytes};
use botstadia_chain::{Address, B256};
use botstadia_enclave::cdn::{Cdn, DirCdn};
use botstadia_enclave::hold::{Held, SessionPayload};
use botstadia_enclave::play::{self, LIMITS, Limits};
use serde_json::{Value, json};

fn held(n: usize) -> Held {
    held_of(n, &["rust"])
}

/// `n` entrants from the racing seed bots of `kinds`, policy bots first,
/// against the arena's newest version (the one they're built for).
fn held_of(n: usize, kinds: &[&str]) -> Held {
    let dir = std::path::PathBuf::from(
        std::env::var("BOTSTADIA_CDN").expect("BOTSTADIA_CDN is set by task chain:test-sdk"),
    );
    let cdn = DirCdn(dir.clone());
    let latest: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("bot/catalog/racing/latest.json")).unwrap(),
    )
    .unwrap();
    let mut digests: Vec<String> = latest["bots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| kinds.contains(&b["kind"].as_str().unwrap_or("")) && b["superseded"] != true)
        .map(|b| b["digest"].as_str().unwrap().to_string())
        .collect();
    digests.sort_by_key(|d| {
        !latest["bots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["kind"] == "policy" && b["digest"] == d.as_str())
    });
    while digests.len() < n {
        digests.push(digests[0].clone());
    }
    digests.truncate(n);
    let arenas: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("arena/catalog/racing/latest.json")).unwrap(),
    )
    .unwrap();
    let arena_digest = arenas["versions"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let spec = Spec {
        digests: digests.iter().map(|d| digest_bytes(d).unwrap()).collect(),
        payouts: vec![Address::ZERO; n],
        arena: digest_bytes(&arena_digest).unwrap(),
        chain_id: 31337,
        results: Address::ZERO,
        nonce: B256::ZERO,
        budget_version: 1,
    };
    let payload: SessionPayload = serde_json::from_value(json!({
        "session_id": "0x00", "spec": spec, "spec_hash": "0x", "entry": "0x", "post": 1, "proof": [], "close": 0,
        "arena_digest": arena_digest, "budget_version": 1,
        "entrants": digests.iter().enumerate().map(|(i, d)| json!({"position": i, "bot_id": "b", "digest": d})).collect::<Vec<_>>(),
        "week": {"id": "0x00", "round": 1, "field_entry": "0x", "leaves": []},
    }))
    .unwrap();
    Held {
        payload,
        week: None,
        entry: vec![],
        proof: vec![],
        arena: cdn.arena("racing", &arena_digest).unwrap(),
        bundles: digests
            .iter()
            .map(|d| cdn.seed_bot("racing", d).unwrap())
            .collect(),
    }
}

/// The seating crate's `seats` stream is the runner's own labelled stream, so
/// the published draw is the one the enclave makes.
#[test]
fn the_published_seating_uses_the_runners_streams() {
    for label in ["seats", "arena", "seat-0"] {
        let s = [9u8; 32];
        assert_eq!(
            botstadia_seating::derive(&s, label),
            botstadia_host::seed::derive(&s, label)
        );
    }
}

#[test]
fn seats_are_a_shuffle_fixed_by_the_seed() {
    let s = play::seed(B256::repeat_byte(1), B256::repeat_byte(2), [3; 32]);
    let a = play::seats(&s, 6);
    assert_eq!(a, play::seats(&s, 6));
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(sorted, (0..6).collect::<Vec<_>>());
    let other = play::seed(B256::repeat_byte(1), B256::repeat_byte(2), [4; 32]);
    assert_ne!(
        play::seed(B256::repeat_byte(1), B256::repeat_byte(2), [3; 32]),
        other,
        "fresh randomness changes the seed"
    );
}

#[test]
fn an_arena_fault_voids_as_a_trap_and_anything_else_as_an_enclave_failure() {
    let fault = anyhow::Error::new(botstadia_host::session::ArenaFault {
        turn: 3,
        message: "boom".into(),
    });
    assert_eq!(play::classify(&fault), VoidReason::ArenaTrap);
    assert_eq!(
        play::classify(&fault.context("stepping")),
        VoidReason::ArenaTrap
    );
    assert_eq!(
        play::classify(&anyhow::anyhow!("disk full")),
        VoidReason::EnclaveFailure
    );
    // A seat that couldn't start before any bot code ran is ours, by name.
    let unavailable = anyhow::Error::new(botstadia_host::session::EntrantUnavailable {
        seat: 1,
        message: "linking".into(),
    });
    assert_eq!(
        play::classify(&unavailable.context("starting")),
        VoidReason::EntrantUnavailable
    );
}

#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn a_race_plays_to_a_finishing_order_with_its_archive() {
    let h = held(3);
    let seed = play::seed(
        B256::repeat_byte(1),
        B256::repeat_byte(2),
        play::fresh().unwrap(),
    );
    let p = play::play(&h, &seed, &LIMITS);
    let Outcome::Order { places } = &p.outcome else {
        panic!("{:?}", p.outcome)
    };
    assert_eq!(places.len(), 3);
    assert!(!p.archive.is_empty() && !p.metrics.is_empty());
    assert_ne!(p.archive_hash(), B256::ZERO);
}

#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn an_entrant_that_does_not_load_voids_entrant_unavailable() {
    let mut h = held(2);
    h.bundles[1] = b"not a zip".to_vec();
    let p = play::play(&h, &[7; 32], &LIMITS);
    assert_eq!(
        p.outcome,
        Outcome::Void {
            reason: VoidReason::EntrantUnavailable
        }
    );
}

#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn passing_the_safety_limit_voids() {
    let h = held(2);
    let tiny = Limits {
        arena_step_fuel: 1,
        arena_setup_fuel: 1,
        backstop: 0,
    };
    let p = play::play(&h, &[7; 32], &tiny);
    assert_eq!(
        p.outcome,
        Outcome::Void {
            reason: VoidReason::SafetyLimit
        }
    );
}

#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn an_arena_that_fails_voids_arena_trap() {
    let mut h = held(2);
    // An engine that won't load or run is the arena's fault, never a bot's.
    h.arena.truncate(h.arena.len() / 2);
    let p = play::play(&h, &[7; 32], &LIMITS);
    assert_eq!(
        p.outcome,
        Outcome::Void {
            reason: VoidReason::ArenaTrap
        }
    );
}

fn isolation() -> (
    botstadia_enclave::canary::Canary,
    botstadia_enclave::keyholder::Isolation,
) {
    let canary = botstadia_enclave::canary::Canary::start(None).unwrap();
    let iso = botstadia_enclave::keyholder::Isolation {
        spawner: botstadia_enclave::worker::Spawner {
            exe: env!("CARGO_BIN_EXE_botstadia-runner").into(),
            core: None,
            // BOTSTADIA_STRICT=1 (on Linux): every worker confined and filtered.
            strict: std::env::var("BOTSTADIA_STRICT").is_ok_and(|v| v == "1"),
        },
        clock: canary.clock(),
    };
    (canary, iso)
}

/// Every seat and the arena in processes of their own play the session the
/// runner plays in-process: the same places from the same seed. (The
/// archives differ only in the seats' names: the isolated runner names a
/// seat by its bot, never by reading its bundle.)
#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn an_isolated_session_plays_exactly_as_one_in_process() {
    let h = held(3);
    let seed = [7u8; 32];
    let here = play::play(&h, &seed, &LIMITS);
    let (_canary, iso) = isolation();
    let there = botstadia_enclave::keyholder::play(&h, &seed, &iso);
    assert_eq!(here.outcome, there.outcome);
    assert_eq!(here.seats, there.seats);
    assert!(!there.archive.is_empty() && !there.metrics.is_empty());
}

/// A session seating the policy seed bot, whose ONNX model runs in an
/// inference worker of its own under the strictest filter, plays isolated
/// exactly as in process.
#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn an_isolated_session_with_a_policy_bot_plays_exactly_as_one_in_process() {
    let h = held_of(3, &["rust", "policy"]);
    let seed = [7u8; 32];
    let here = play::play(&h, &seed, &LIMITS);
    let (_canary, iso) = isolation();
    let there = botstadia_enclave::keyholder::play(&h, &seed, &iso);
    assert_eq!(here.outcome, there.outcome);
    assert_eq!(here.seats, there.seats);
    assert!(
        !matches!(there.outcome, Outcome::Void { .. }),
        "{:?}",
        there.outcome
    );
}

/// Every racing seed bot, the policy bot's ONNX model among them, against
/// the arena's newest version, verifies isolated as it does in process.
/// With BOTSTADIA_STRICT=1 on Linux each worker is confined and filtered as
/// in an image: on TDX the policy bot's verification ended on a refused
/// syscall, which no rust bot here had reached.
#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn every_racing_seed_bot_verifies_isolated_as_in_process() {
    let dir = std::path::PathBuf::from(std::env::var("BOTSTADIA_CDN").unwrap());
    let cdn = DirCdn(dir.clone());
    let read = |p: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(p)).unwrap()).unwrap()
    };
    let arena_digest = read("arena/catalog/racing/latest.json")["versions"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let arena_bytes = cdn.arena("racing", &arena_digest).unwrap();
    let arena = botstadia_host::LoadedArena::from_bytes(&arena_bytes).unwrap();
    let (_canary, iso) = isolation();
    let budget = botstadia_host::budget::current();
    for b in read("bot/catalog/racing/latest.json")["bots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["superseded"] != true)
    {
        let digest = b["digest"].as_str().unwrap();
        let zip = cdn.seed_bot("racing", digest).unwrap();
        let isolated =
            botstadia_enclave::keyholder::verify(&iso, budget.id, &zip, &arena_bytes, &[3; 32])
                .unwrap_or_else(|e| panic!("{} isolated: {e}", b["name"]));
        let here = botstadia_host::verify::verify(
            botstadia_host::Bundle::from_zip(&zip, budget).unwrap(),
            &arena,
            budget,
            &[3; 32],
        );
        assert_eq!(isolated.passed, here.passed, "{}", b["name"]);
        assert_eq!(isolated.problems, here.problems, "{}", b["name"]);
    }
}

/// The anchor-time recheck and verification, each in a worker of its own,
/// agree with the in-process path: a seed bot passes both, junk fails.
#[test]
#[ignore = "needs the CDN tree: run by task chain:test-sdk"]
fn isolated_recheck_and_verification_agree_with_in_process() {
    let h = held(1);
    let (_canary, iso) = isolation();
    let arena = botstadia_host::LoadedArena::from_bytes(&h.arena).unwrap();
    let turn_fuel = arena.declaration.budgets.turn_fuel;
    assert_eq!(
        botstadia_enclave::keyholder::check(&iso, 1, &h.bundles[0], turn_fuel),
        Ok(())
    );
    assert!(botstadia_enclave::keyholder::check(&iso, 1, b"not a zip", turn_fuel).is_err());
    let report =
        botstadia_enclave::keyholder::verify(&iso, 1, &h.bundles[0], &h.arena, &[3; 32]).unwrap();
    let budget = botstadia_host::budget::version(1).unwrap();
    let here = botstadia_host::verify::verify(
        botstadia_host::Bundle::from_zip(&h.bundles[0], budget).unwrap(),
        &arena,
        budget,
        &[3; 32],
    );
    assert_eq!(report.passed, here.passed);
    assert_eq!(report.digest, here.digest);
    assert_eq!(report.timing_turns, here.timing_turns);
    assert!(botstadia_enclave::keyholder::verify(&iso, 1, b"junk", &h.arena, &[3; 32]).is_err());
}
