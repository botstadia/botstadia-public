//! Phase 1's limits through the isolated seat (TESTNET T4): every host
//! fixture bot (a runaway loop, traps, memory and tables past their totals,
//! sockets, handle leaks, oversized host calls, bad calls, heavy start
//! functions) plays to the same outcome in processes of its own as in the
//! runner's process. On Linux with BOTSTADIA_STRICT=1, every worker is
//! confined and filtered.
//!
//! Needs the host's fixtures, which its own tests build
//! (`cargo test -p botstadia-host --test session` writes
//! target/host-fixtures/built; BOTSTADIA_HOST_FIXTURES overrides).

use std::path::PathBuf;

use botstadia_enclave::hold::{Held, SessionPayload};
use botstadia_enclave::play::{self, LIMITS};

const VARIANTS: &[&str] = &[
    "steady",
    "spiky",
    "forever",
    "trap",
    "init_trap",
    "memcopy",
    "sleep",
    "clock",
    "budget",
    "declaration",
    "sockets",
    "handles",
    "bigcall",
    "badcalls",
    "memories_over",
    "memories_under",
    "start_heavy",
    "start_light",
    "start_spin",
    "start_trap",
    "tables_over",
    "tables_under",
];

fn fixtures() -> PathBuf {
    std::env::var("BOTSTADIA_HOST_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/host-fixtures/built")
        })
}

/// A fixture as a bundle: its component with a plain Rust bot's manifest.
fn zip(variant: &str) -> Vec<u8> {
    use botstadia_interface::bundle::{BotKind, Manifest};
    let budget = botstadia_host::budget::version(1).unwrap();
    let mut bundle =
        botstadia_host::Bundle::load(&fixtures().join(format!("{variant}.wasm")), budget).unwrap();
    bundle.manifest = Some(Manifest::new("fixture", 1, BotKind::Rust));
    bundle.zip().unwrap()
}

fn held(variant: &str) -> Held {
    let spec = botstadia_chain::spec::Spec {
        digests: vec![],
        payouts: vec![],
        arena: botstadia_chain::B256::ZERO,
        chain_id: 1,
        results: botstadia_chain::Address::ZERO,
        nonce: botstadia_chain::B256::ZERO,
        budget_version: 1,
    };
    Held {
        payload: SessionPayload {
            session_id: "0x00".into(),
            spec,
            spec_hash: "0x".into(),
            entry: "0x".into(),
            post: 0,
            proof: vec![],
            close: 0,
            arena_digest: String::new(),
            arena_id: String::new(),
            budget_version: 1,
            entrants: vec![],
            week: Default::default(),
            window: vec![],
            arena_post: None,
        },
        week: None,
        entry: vec![],
        proof: vec![],
        arena: std::fs::read(fixtures().join("arena.wasm")).unwrap(),
        bundles: vec![zip(variant), zip("steady")],
    }
}

#[test]
#[ignore = "needs the host's built fixtures: run after the host's session tests"]
fn every_limit_fixture_plays_the_same_isolated() {
    let canary = botstadia_enclave::canary::Canary::start(None).unwrap();
    let iso = botstadia_enclave::keyholder::Isolation {
        spawner: botstadia_enclave::worker::Spawner {
            exe: env!("CARGO_BIN_EXE_botstadia-runner").into(),
            core: None,
            strict: std::env::var("BOTSTADIA_STRICT").is_ok_and(|v| v == "1"),
        },
        clock: canary.clock(),
    };
    let mut differ = Vec::new();
    for variant in VARIANTS {
        if !fixtures().join(format!("{variant}.wasm")).exists() {
            continue;
        }
        let h = held(variant);
        let seed = [5u8; 32];
        let here = play::play(&h, &seed, &LIMITS);
        let there = botstadia_enclave::keyholder::play(&h, &seed, &iso);
        eprintln!("{variant}: {:?} / {:?}", here.outcome, there.outcome);
        if here.outcome != there.outcome {
            differ.push(format!(
                "{variant}: in-process {:?}, isolated {:?}",
                here.outcome, there.outcome
            ));
        }
    }
    assert!(differ.is_empty(), "{differ:#?}");
}

/// A fixture with the host's `scale` model, as `infer` and `infer_many` use.
fn with_model(variant: &str) -> Vec<u8> {
    use botstadia_interface::bundle::{BotKind, Manifest, ModelEntry};
    let budget = botstadia_host::budget::version(1).unwrap();
    let mut bundle =
        botstadia_host::Bundle::load(&fixtures().join(format!("{variant}.wasm")), budget).unwrap();
    let mut manifest = Manifest::new("fixture", 1, BotKind::Rust);
    manifest.models.push(ModelEntry {
        name: "scale".into(),
        path: "artifacts/scale.onnx".into(),
    });
    bundle.manifest = Some(manifest);
    bundle.models = vec![(
        "scale".into(),
        std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../host/fixtures/scale.onnx"),
        )
        .unwrap(),
    )];
    bundle.zip().unwrap()
}

/// What one `infer` call costs a seat through its processes (BOTS.md,
/// budgets): `infer_many` (400 calls a turn) against `infer` (one), both
/// isolated and in process, so the difference is the calls' own time. Run on
/// the enclave's CPU class to set the per-call charge:
/// `cargo test -p botstadia-enclave --test limits infer_call_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement: needs the host's built fixtures and an idle machine"]
fn infer_call_cost() {
    let canary = botstadia_enclave::canary::Canary::start(None).unwrap();
    let iso = botstadia_enclave::keyholder::Isolation {
        spawner: botstadia_enclave::worker::Spawner {
            exe: env!("CARGO_BIN_EXE_botstadia-runner").into(),
            core: None,
            strict: std::env::var("BOTSTADIA_STRICT").is_ok_and(|v| v == "1"),
        },
        clock: canary.clock(),
    };
    let held_of = |variant: &str| {
        let mut h = held("steady");
        h.bundles = vec![with_model(variant), zip("steady")];
        h
    };
    let (one, many) = (held_of("infer"), held_of("infer_many"));
    let seed = [5u8; 32];
    // The median turn of the infer seat, as the seat is timed (its own
    // processes' time, recorded per turn in the metrics' `wall_ms`), so no
    // session's start or compile is in it.
    let turn_ms = |played: &botstadia_enclave::play::Played| {
        let metrics =
            botstadia_interface::metrics::Metrics::from_json(&played.metrics).expect("metrics");
        let seat = played
            .seats
            .iter()
            .position(|&p| p == 0)
            .expect("the infer seat") as u32;
        let line = metrics
            .platform
            .iter()
            .find(|t| t.key == "wall_ms")
            .and_then(|t| t.lines.iter().find(|l| l.seat == Some(seat)))
            .expect("wall_ms")
            .clone();
        let mut turns = line.dense(metrics.turns);
        turns.retain(|ms| *ms > 0.0);
        turns.sort_by(f32::total_cmp);
        turns[turns.len() / 2] as f64
    };
    let median_of = |runs: Vec<f64>| {
        let mut runs = runs;
        runs.sort_by(f64::total_cmp);
        runs[runs.len() / 2]
    };
    let isolated = |h| {
        median_of(
            (0..5)
                .map(|_| turn_ms(&botstadia_enclave::keyholder::play(h, &seed, &iso)))
                .collect(),
        )
    };
    let here = |h| {
        median_of(
            (0..5)
                .map(|_| turn_ms(&play::play(h, &seed, &LIMITS)))
                .collect(),
        )
    };
    // 399 more calls a turn.
    let per_call = |many: f64, one: f64| (many - one) / 399.0 * 1000.0;
    let (iso_many, iso_one) = (isolated(&many), isolated(&one));
    let (here_many, here_one) = (here(&many), here(&one));
    eprintln!(
        "infer per call: isolated {:.1} µs (turn {iso_many:.1} ms against {iso_one:.1}), in process {:.1} µs (turn {here_many:.1} ms against {here_one:.1})",
        per_call(iso_many, iso_one),
        per_call(here_many, here_one),
    );
    let played = botstadia_enclave::keyholder::play(&many, &seed, &iso);
    assert!(
        matches!(
            played.outcome,
            botstadia_chain::outcome::Outcome::Order { .. }
        ),
        "{:?}",
        played.outcome
    );
}
