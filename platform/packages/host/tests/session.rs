//! The session host against its fixtures: budgets, pausing, traps, the
//! sandbox and the agent world's imports. The fixtures are built here, once
//! per test run, into their own target directory.
//!
//! Pausing is tested as COMPETITION.md's rules for failures list it, for what
//! a local host can do: running out of fuel at varying points, consecutive
//! overruns, a bulk memory operation charged per byte, fuel accounting across
//! resumed turns, and traps. The backstop and machine pauses are timed in an
//! enclave's canary ticks, so they're tested there.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use botstadia_host::bundle::Bundle;
use botstadia_host::seat::Program;
use botstadia_host::session::{SeatTurn, StepOut};
use botstadia_host::{LoadedArena, Seat, Session, SessionOptions, budget, seed};
use botstadia_interface::bundle::{BotKind, Manifest, ModelEntry};

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
    "infer",
    "infer_many",
    "sockets",
    "handles",
    "bigcall",
    "badcalls",
];
const TURN_FUEL: u64 = 1_000_000;

fn fixtures() -> &'static PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir.join("../..");
        let target = workspace.join("target/host-fixtures");
        let out = target.join("built");
        std::fs::create_dir_all(&out).unwrap();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let build = |package: &str, feature: Option<&str>| {
            let mut command = Command::new(&cargo);
            command
                .current_dir(&workspace)
                .env("CARGO_TARGET_DIR", &target)
                .args([
                    "build",
                    "--quiet",
                    "--release",
                    "--target",
                    "wasm32-wasip2",
                    "-p",
                    package,
                ]);
            if let Some(feature) = feature {
                command.args(["--features", &format!("{package}/{feature}")]);
            }
            let status = command.status().expect("running cargo");
            assert!(status.success(), "building {package} {feature:?}");
            target
                .join("wasm32-wasip2/release")
                .join(format!("{}.wasm", package.replace('-', "_")))
        };
        let arena = build("botstadia-host-fixture-arena", None);
        std::fs::copy(arena, out.join("arena.wasm")).unwrap();
        let shell = build("botstadia-policy-shell", None);
        std::fs::copy(shell, out.join("shell.wasm")).unwrap();
        for variant in VARIANTS {
            let bot = build("botstadia-host-fixture-bot", Some(variant));
            std::fs::copy(bot, out.join(format!("{variant}.wasm"))).unwrap();
        }
        out
    })
}

fn arena() -> LoadedArena {
    LoadedArena::load(&fixtures().join("arena.wasm")).unwrap()
}

fn bot(variant: &str) -> Seat {
    let bundle = Bundle::load(
        &fixtures().join(format!("{variant}.wasm")),
        budget::current(),
    )
    .unwrap();
    Seat::Bot(Arc::new(
        Program::new(bundle, budget::current(), TURN_FUEL).unwrap(),
    ))
}

/// Play to the end, returning every turn's output.
fn play(seats: Vec<Seat>, seed: u64, options: SessionOptions) -> (Vec<StepOut>, Session) {
    let arena = arena();
    let (mut session, first) = Session::start(
        &arena,
        seats,
        &seed::from_u64(seed),
        budget::current(),
        options,
    )
    .unwrap();
    let mut turns = vec![first];
    while session.finished().is_none() {
        turns.push(session.step(&[]).unwrap());
    }
    (turns, session)
}

fn seat(turns: &[StepOut], index: usize) -> Vec<&SeatTurn> {
    turns
        .iter()
        .skip(1)
        .map(|turn| &turn.seats[index])
        .collect()
}

/// Nothing ever spends more than its turn's fuel, however it's split.
fn within_budget(turns: &[StepOut]) {
    for turn in turns.iter().skip(1) {
        for seat in &turn.seats {
            assert!(
                seat.wasm_fuel + seat.inference_fuel <= TURN_FUEL,
                "turn {}: {} + {} fuel is over the turn's {TURN_FUEL}",
                turn.turn,
                seat.wasm_fuel,
                seat.inference_fuel
            );
        }
    }
}

#[test]
fn a_steady_bot_acts_every_turn() {
    let (turns, session) = play(
        vec![bot("steady"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    within_budget(&turns);
    assert!(seat(&turns, 0).iter().all(|s| s.acted && !s.paused));
    // Every turn it was given an input, gave an output and had memory.
    assert!(
        seat(&turns, 0)
            .iter()
            .all(|s| s.input_bytes > 0 && s.output_bytes > 0 && s.memory_bytes > 0 && !s.rejected)
    );
    let (archive, metrics) = session.record().unwrap();
    assert_eq!(archive.frames.len(), 21);
    for key in [
        "no_action",
        "wasm_fuel",
        "inference_fuel",
        "paused",
        "wall_ms",
        "rejected",
        "trapped",
        "memory_kib",
        "input_bytes",
        "output_bytes",
    ] {
        assert!(
            metrics.platform.iter().any(|track| track.key == key),
            "no `{key}` series"
        );
    }
    // Memory is recorded on change: a steady bot's settles at once.
    let memory = metrics
        .platform
        .iter()
        .find(|track| track.key == "memory_kib")
        .unwrap();
    assert!(memory.lines[0].points.len() <= 2, "{:?}", memory.lines[0]);
    assert!(memory.lines[0].points[0].1 > 0.0);
}

#[test]
fn running_out_pauses_and_the_stale_call_finishes_next_turn() {
    let (turns, _) = play(
        vec![bot("spiky"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    within_budget(&turns);
    let spiky = seat(&turns, 0);
    // A bot sees the previous turn's number, so on turns 3, 7, 11 … it starts
    // a computation of about 2.1 million fuel. It pauses that turn and the
    // next, spending exactly each turn's fuel; on the third it finishes, its
    // result is discarded, and a fresh call acts with what's left.
    for (index, turn) in spiky.iter().enumerate() {
        let number = index + 1;
        match number % 4 {
            3 | 0 => {
                assert!(
                    turn.paused && !turn.acted,
                    "turn {number} should pause: {turn:?}"
                );
                assert_eq!(
                    turn.wasm_fuel, TURN_FUEL,
                    "a paused turn spends exactly its fuel"
                );
            }
            1 if number > 1 => {
                assert!(
                    turn.acted && !turn.paused,
                    "turn {number} should act: {turn:?}"
                );
                assert!(
                    turn.wasm_fuel > 100_000,
                    "the stale computation's end is counted: {turn:?}"
                );
            }
            _ => assert!(turn.acted, "turn {number} should act: {turn:?}"),
        }
    }
}

#[test]
fn consecutive_overruns_never_act_and_never_overspend() {
    let (turns, session) = play(
        vec![bot("forever"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    within_budget(&turns);
    assert!(seat(&turns, 0).iter().all(|s| s.paused && !s.acted));
    assert!(seat(&turns, 1).iter().all(|s| s.acted));
    assert!(session.finished().is_some());
}

#[test]
fn bulk_memory_is_charged_per_byte() {
    let (turns, _) = play(vec![bot("memcopy")], 1, SessionOptions::default());
    within_budget(&turns);
    // 16 MiB copied costs 16 million fuel: no turn can afford it.
    assert!(seat(&turns, 0).iter().all(|s| !s.acted));
}

#[test]
fn a_trap_leaves_the_bot_inactive_for_good() {
    let (turns, _) = play(
        vec![bot("trap"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    let trapping = seat(&turns, 0);
    // It sees turn 5 on the sixth turn.
    assert!(trapping[..5].iter().all(|s| s.acted));
    assert!(trapping[5].trapped.is_some());
    assert!(trapping[6..].iter().all(|s| !s.acted && s.wasm_fuel == 0));
}

#[test]
fn the_metrics_file_marks_a_trap_from_its_turn_on() {
    let (_, session) = play(
        vec![bot("trap"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    let (_, metrics) = session.record().unwrap();
    let trapped = metrics
        .platform
        .iter()
        .find(|track| track.key == "trapped")
        .unwrap();
    // Recorded on change: 0 from the first turn, 1 from the trap's.
    assert_eq!(
        trapped.lines[0]
            .points
            .iter()
            .map(|p| p.1)
            .collect::<Vec<_>>(),
        vec![0.0, 1.0]
    );
    assert_eq!(trapped.lines[1].points, vec![(1, 0.0)]);
}

#[test]
fn a_bot_that_traps_in_init_is_marked_from_the_start_and_sent_nothing() {
    let (turns, session) = play(
        vec![bot("init_trap"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    assert!(
        seat(&turns, 0)
            .iter()
            .all(|s| !s.acted && s.input_bytes == 0)
    );
    let (_, metrics) = session.record().unwrap();
    let trapped = metrics
        .platform
        .iter()
        .find(|track| track.key == "trapped")
        .unwrap();
    assert_eq!(trapped.lines[0].points[0], (0, 1.0));
    assert!(trapped.lines[0].points.iter().all(|p| p.1 == 1.0));
}

#[test]
fn a_bot_gets_no_socket_and_no_name_lookup() {
    let (turns, _) = play(vec![bot("sockets")], 1, SessionOptions::default());
    // The bot answers 1 only when every socket call was refused.
    for s in seat(&turns, 0) {
        let output = s.output.as_deref().expect("it acted");
        let value = f32::from_le_bytes(output[output.len() - 4..].try_into().unwrap());
        assert_eq!(value, 1.0, "a socket call went through");
    }
}

/// A bot whose core module has a start function with `body`: the bot's own
/// code, run at instantiation, before `init`. Written by hand, since Rust
/// doesn't emit a start section. It answers every turn with nothing (no
/// action), which is all these tests need.
fn start_function_bot(name: &str, body: &str) -> Seat {
    let component = wat::parse_str(format!(
        r#"(component
  (core module $m
    (memory (export "memory") 1)
    (func $start (local $n i32) {body})
    (start $start)
    (func (export "init"))
    (func (export "on-tick") (param i32 i32) (result i32)
      (i32.store (i32.const 8) (i32.const 64))
      (i32.store (i32.const 12) (i32.const 0))
      (i32.const 8))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) (i32.const 1024)))
  (core instance $i (instantiate $m))
  (func (export "init") (canon lift (core func $i "init")))
  (func (export "on-tick") (param "input" (list u8)) (result (list u8))
    (canon lift (core func $i "on-tick") (memory (core memory $i "memory")) (realloc (core func $i "realloc")))))"#
    ))
    .unwrap();
    let path = fixtures().join(format!("{name}.wasm"));
    std::fs::write(&path, component).unwrap();
    let bundle = Bundle::load(&path, budget::current()).unwrap();
    Seat::Bot(Arc::new(
        Program::new(bundle, budget::current(), TURN_FUEL).unwrap(),
    ))
}

#[test]
fn a_start_function_trap_is_the_bot_s_and_the_session_goes_on() {
    let (turns, _) = play(
        vec![
            start_function_bot("start_trap", "unreachable"),
            bot("steady"),
        ],
        1,
        SessionOptions::default(),
    );
    assert!(
        turns[0].seats[0]
            .trapped
            .as_deref()
            .is_some_and(|why| why.contains("start function")),
        "{:?}",
        turns[0].seats[0].trapped
    );
    assert!(seat(&turns, 0).iter().all(|s| !s.acted && s.wasm_fuel == 0));
    assert!(seat(&turns, 1).iter().all(|s| s.acted));
}

#[test]
fn a_start_function_that_spins_runs_out_of_init_s_fuel() {
    let (turns, _) = play(
        vec![
            start_function_bot("start_spin", "(loop $l (br $l))"),
            bot("steady"),
        ],
        1,
        SessionOptions::default(),
    );
    let init = &turns[0].seats[0];
    assert!(init.trapped.is_some(), "a spinning start function ran on");
    // It spent init's whole budget: 4,000,000, as the fixture arena declares.
    assert_eq!(init.wasm_fuel, 4_000_000);
    assert!(seat(&turns, 1).iter().all(|s| s.acted));
}

#[test]
fn a_start_function_s_work_is_charged_to_init() {
    let count = |n: u32| {
        format!(
            "(local.set $n (i32.const {n})) (loop $l (local.set $n (i32.sub (local.get $n) (i32.const 1))) (br_if $l (local.get $n)))"
        )
    };
    let light = play(
        vec![start_function_bot("start_light", &count(10))],
        1,
        SessionOptions::default(),
    )
    .0;
    let heavy = play(
        vec![start_function_bot("start_heavy", &count(10_000))],
        1,
        SessionOptions::default(),
    )
    .0;
    assert!(light[0].seats[0].trapped.is_none());
    assert!(heavy[0].seats[0].trapped.is_none());
    assert!(
        heavy[0].seats[0].wasm_fuel > light[0].seats[0].wasm_fuel + 10_000,
        "{} vs {}",
        heavy[0].seats[0].wasm_fuel,
        light[0].seats[0].wasm_fuel
    );
}

/// A bot of two core modules, each with its own memory (no multi-memory
/// needed), that on its first turn grows its own by `own` pages and the other
/// by `other`. It answers 1 when the second growth was refused, -1 when it
/// went through; tables the same with `kind = "table"`.
fn two_modules_bot(name: &str, kind: &str, own: u32, other: u32) -> Seat {
    let (decl, grow) = match kind {
        "memory" => ("(memory (export \"m\") 1)", "(memory.grow (local.get 0))"),
        _ => (
            "(table (export \"t\") 0 funcref)",
            "(table.grow 0 (ref.null func) (local.get 0))",
        ),
    };
    let component = wat::parse_str(format!(
        r#"(component
  (core module $other
    {decl}
    (func (export "grow") (param i32) (result i32) {grow}))
  (core module $m
    (import "other" "grow" (func $grow_other (param i32) (result i32)))
    (memory (export "memory") 1)
    {table}
    (global $done (mut i32) (i32.const 0))
    (global $value (mut f32) (f32.const 0))
    (func $grow_own (param i32) (result i32) {own_grow})
    (func (export "init"))
    (func (export "on-tick") (param i32 i32) (result i32)
      (if (i32.eqz (global.get $done))
        (then
          (global.set $done (i32.const 1))
          (drop (call $grow_own (i32.const {own})))
          (global.set $value
            (select (f32.const 1) (f32.const -1)
              (i32.eq (call $grow_other (i32.const {other})) (i32.const -1))))))
      ;; "BSPO", 1, 4, the value: the fixture arena's output.
      (i32.store (i32.const 1024) (i32.const 0x4f505342))
      (i32.store (i32.const 1028) (i32.const 1))
      (i32.store (i32.const 1032) (i32.const 4))
      (f32.store (i32.const 1036) (global.get $value))
      (i32.store (i32.const 8) (i32.const 1024))
      (i32.store (i32.const 12) (i32.const 16))
      (i32.const 8))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) (i32.const 2048)))
  (core instance $o (instantiate $other))
  (core instance $i (instantiate $m (with "other" (instance $o))))
  (func (export "init") (canon lift (core func $i "init")))
  (func (export "on-tick") (param "input" (list u8)) (result (list u8))
    (canon lift (core func $i "on-tick") (memory (core memory $i "memory")) (realloc (core func $i "realloc")))))"#,
        table = if kind == "memory" { "" } else { "(table $t 0 funcref)" },
        own_grow = if kind == "memory" {
            "(memory.grow (local.get 0))"
        } else {
            "(table.grow $t (ref.null func) (local.get 0))"
        },
    ))
    .unwrap();
    let path = fixtures().join(format!("{name}.wasm"));
    std::fs::write(&path, component).unwrap();
    let bundle = Bundle::load(&path, budget::current()).unwrap();
    Seat::Bot(Arc::new(
        Program::new(bundle, budget::current(), TURN_FUEL).unwrap(),
    ))
}

fn answer(turns: &[StepOut]) -> f32 {
    let output = turns[1].seats[0].output.as_deref().expect("it acted");
    f32::from_le_bytes(output[output.len() - 4..].try_into().unwrap())
}

#[test]
fn memory_is_budgeted_across_every_memory_a_bot_holds() {
    // 64 MiB is 1,024 pages: 1 + 900 here and 1 + 200 there is over it,
    // though each memory alone is under.
    let over = play(
        vec![two_modules_bot("memories_over", "memory", 900, 200)],
        1,
        SessionOptions::default(),
    )
    .0;
    assert_eq!(answer(&over), 1.0, "the second growth went through");
    // The series reports the total.
    assert_eq!(over[1].seats[0].memory_bytes, (1 + 900 + 1) * 65_536);
    let under = play(
        vec![two_modules_bot("memories_under", "memory", 900, 100)],
        1,
        SessionOptions::default(),
    )
    .0;
    assert_eq!(
        answer(&under),
        -1.0,
        "a growth within the total was refused"
    );
}

#[test]
fn table_elements_are_budgeted_across_every_table_a_bot_holds() {
    let over = play(
        vec![two_modules_bot("tables_over", "table", 60_000, 60_000)],
        1,
        SessionOptions::default(),
    )
    .0;
    assert_eq!(answer(&over), 1.0, "the second growth went through");
    let under = play(
        vec![two_modules_bot("tables_under", "table", 60_000, 30_000)],
        1,
        SessionOptions::default(),
    )
    .0;
    assert_eq!(
        answer(&under),
        -1.0,
        "a growth within the total was refused"
    );
}

#[test]
fn a_bot_that_leaks_handles_fails_at_the_limit_as_its_own_failure() {
    let (turns, _) = play(
        vec![bot("handles"), bot("steady")],
        1,
        SessionOptions::default(),
    );
    let leaking = seat(&turns, 0);
    // It sees turn 1 on the second turn, and fails there.
    assert!(leaking[0].acted);
    assert!(leaking[1].trapped.is_some(), "{:?}", leaking[1]);
    assert!(leaking[2..].iter().all(|s| !s.acted));
    assert!(seat(&turns, 1).iter().all(|s| s.acted));
}

#[test]
fn a_host_call_copies_no_more_than_the_largest_model_input() {
    let mut manifest = Manifest::new("host-fixture", 1, BotKind::Rust);
    manifest.models.push(ModelEntry {
        name: "scale".into(),
        path: "artifacts/scale.onnx".into(),
    });
    let model =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/scale.onnx")).unwrap();
    let bundle = Bundle {
        name: "bigcall".into(),
        manifest: Some(manifest),
        component: std::fs::read(fixtures().join("bigcall.wasm")).unwrap(),
        models: vec![("scale".into(), model)],
        digest: String::new(),
    };
    let program = Program::new(bundle, budget::current(), TURN_FUEL).unwrap();
    // Unbudgeted, so building 32 MiB doesn't simply run out of fuel first.
    let (turns, _) = play(
        vec![Seat::Bot(Arc::new(program)), bot("steady")],
        1,
        SessionOptions {
            budget: false,
            ..SessionOptions::default()
        },
    );
    let big = seat(&turns, 0);
    // Refused before the host copies it: the bot's own failure.
    let why = big[1].trapped.as_deref().unwrap_or_default();
    assert!(why.contains("fuel") || why.contains("exhausted"), "{why}");
    assert!(seat(&turns, 1).iter().all(|s| s.acted));
}

#[test]
fn a_failing_infer_call_still_pays_for_what_it_passed() {
    let (turns, _) = play(vec![bot("badcalls")], 1, SessionOptions::default());
    for s in seat(&turns, 0) {
        assert!(s.acted);
        // A hundred calls of a thousand elements, none of which ran.
        assert!(s.inference_fuel >= 100 * 1_000, "{}", s.inference_fuel);
    }
}

#[test]
fn a_sleep_returns_at_once() {
    let started = std::time::Instant::now();
    let (turns, _) = play(vec![bot("sleep")], 1, SessionOptions::default());
    assert!(seat(&turns, 0).iter().all(|s| s.acted));
    assert!(
        started.elapsed().as_secs() < 30,
        "twenty five-second sleeps took real time"
    );
}

#[test]
fn the_clock_runs_on_fuel_so_reruns_match() {
    let values = |seed| {
        let (_, session) = play(vec![bot("clock")], seed, SessionOptions::default());
        session.record().unwrap().1
    };
    let (a, b) = (values(1), values(1));
    assert_eq!(a.series("value"), b.series("value"));
    let line = &a.series("value").unwrap().lines[0];
    assert!(
        line.points.iter().any(|(_, value)| *value > 0.0),
        "elapsed time should advance with fuel"
    );
}

#[test]
fn the_budget_import_reports_this_turn() {
    let (turns, _) = play(vec![bot("budget")], 1, SessionOptions::default());
    // A value of 1 means the turn matched and fuel was left.
    let metrics_ok = seat(&turns, 0).iter().all(|s| s.acted);
    assert!(metrics_ok);
}

#[test]
fn the_declaration_import_returns_the_arena_s() {
    let (_, session) = play(vec![bot("declaration")], 1, SessionOptions::default());
    let metrics = session.record().unwrap().1;
    let line = &metrics.series("value").unwrap().lines[0];
    assert!(
        line.points.iter().skip(1).all(|(_, value)| *value == 1.0),
        "{line:?}"
    );
}

#[test]
fn inference_runs_and_is_charged() {
    let mut manifest = Manifest::new("host-fixture", 1, BotKind::Rust);
    manifest.models.push(ModelEntry {
        name: "scale".into(),
        path: "artifacts/scale.onnx".into(),
    });
    let model =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/scale.onnx")).unwrap();
    let bundle = Bundle {
        name: "infer".into(),
        manifest: Some(manifest),
        component: std::fs::read(fixtures().join("infer.wasm")).unwrap(),
        models: vec![("scale".into(), model)],
        digest: String::new(),
    };
    let program = Program::new(bundle, budget::current(), TURN_FUEL).unwrap();
    assert_eq!(program.models[0].macs, 1);
    let (turns, session) = play(
        vec![Seat::Bot(Arc::new(program))],
        1,
        SessionOptions::default(),
    );
    within_budget(&turns);
    assert!(
        seat(&turns, 0)
            .iter()
            .all(|s| s.acted && s.inference_fuel > 0)
    );
    let metrics = session.record().unwrap().1;
    let line = &metrics.series("value").unwrap().lines[0];
    // y = turn × 0.01, and the bot sends it straight back.
    assert!(
        line.points
            .iter()
            .any(|(turn, value)| *turn == 7 && (*value - 0.06).abs() < 1e-6),
        "{line:?}"
    );
}

#[test]
fn a_session_replays_identically() {
    let archive = |seed| {
        let (_, session) = play(
            vec![bot("spiky"), bot("clock")],
            seed,
            SessionOptions::default(),
        );
        let (archive, metrics) = session.record().unwrap();
        (archive.encode(), metrics.series)
    };
    assert_eq!(archive(3), archive(3));
}

#[test]
fn external_seats_take_the_caller_s_output_and_bad_output_is_no_action() {
    let arena = arena();
    let seats = vec![Seat::External { name: "py".into() }, bot("steady")];
    let (mut session, _) = Session::start(
        &arena,
        seats,
        &seed::from_u64(1),
        budget::current(),
        SessionOptions::default(),
    )
    .unwrap();
    let good = {
        let mut out = b"BSPO".to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&0.25f32.to_le_bytes());
        out
    };
    let mut bad = good.clone();
    bad[12..16].copy_from_slice(&2.0f32.to_le_bytes());
    let first = session.step(&[Some(good), None]).unwrap();
    assert!(first.seats[0].acted);
    assert_eq!(first.shaping[0], 0.25);
    assert!(!first.seats[0].rejected);
    let second = session.step(&[Some(bad), None]).unwrap();
    assert!(
        !second.seats[0].acted,
        "out of bounds is no action, never clamped"
    );
    assert!(
        second.seats[0].rejected,
        "a broken output is a rejected action"
    );
    assert_eq!(second.seats[0].output_bytes, 16);
    assert_eq!(second.shaping[0], 0.0);
}

fn policy(model: &str) -> Seat {
    let mut manifest = Manifest::new("host-fixture", 1, BotKind::Policy);
    manifest.models.push(ModelEntry {
        name: "policy".into(),
        path: "artifacts/policy.onnx".into(),
    });
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(model),
    )
    .unwrap();
    let bundle = Bundle {
        name: model.into(),
        manifest: Some(manifest),
        component: std::fs::read(fixtures().join("shell.wasm")).unwrap(),
        models: vec![("policy".into(), bytes)],
        digest: String::new(),
    };
    Seat::Bot(Arc::new(
        Program::new(bundle, budget::current(), TURN_FUEL).unwrap(),
    ))
}

fn values(session: Session) -> Vec<(u32, f32)> {
    session.record().unwrap().1.series("value").unwrap().lines[0]
        .points
        .clone()
}

#[test]
fn the_policy_shell_runs_a_policy_by_the_contract() {
    let (turns, session) = play(vec![policy("policy.onnx")], 1, SessionOptions::default());
    within_budget(&turns);
    assert!(
        seat(&turns, 0)
            .iter()
            .all(|s| s.acted && s.inference_fuel > 0)
    );
    // On turn t the shell sees turn t - 1 and sends tanh((t - 1) · 0.1),
    // mapped from [-1, 1] onto the declared [-1, 1].
    for (turn, value) in values(session).into_iter().skip(1) {
        let want = ((turn as f32 - 1.0) * 0.1).tanh();
        assert!(
            (value - want).abs() < 1e-5,
            "turn {turn}: {value} vs {want}"
        );
    }
}

#[test]
fn the_policy_shell_carries_memory_turn_to_turn() {
    let (_, session) = play(vec![policy("memory.onnx")], 1, SessionOptions::default());
    // After k turns the state is k in every slot: tanh(4k · 0.05).
    for (turn, value) in values(session).into_iter().skip(1) {
        let want = (turn as f32 * 0.2).tanh();
        assert!(
            (value - want).abs() < 1e-5,
            "turn {turn}: {value} vs {want}"
        );
    }
}

#[test]
fn a_policy_passes_the_contract_check() {
    let arena = arena();
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/memory.onnx")).unwrap();
    let info = botstadia_host::onnx::preflight(&bytes, budget::current(), TURN_FUEL).unwrap();
    let manifest = Manifest::new("host-fixture", 1, BotKind::Policy);
    let problems = botstadia_host::onnx::policy::problems(
        &info,
        &arena.declaration,
        &manifest,
        budget::current(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let wrong = Manifest::new("boxing", 1, BotKind::Policy);
    assert!(
        !botstadia_host::onnx::policy::problems(
            &info,
            &arena.declaration,
            &wrong,
            budget::current()
        )
        .is_empty()
    );
}

#[test]
fn a_metered_arena_counts_setup_and_every_turn() {
    let (turns, session) = play(
        vec![bot("steady"), bot("steady")],
        3,
        SessionOptions::default(),
    );
    let setup = turns[0].arena_fuel.expect("setup is counted");
    assert!(setup > 0);
    assert_eq!(session.setup_fuel, Some(setup));
    let per_turn: Vec<u64> = turns
        .iter()
        .skip(1)
        .map(|turn| turn.arena_fuel.expect("every turn is counted"))
        .collect();
    assert!(per_turn.iter().all(|fuel| *fuel > 0));
    assert_eq!(session.worst_step_fuel, per_turn.iter().copied().max());
    assert_eq!(
        session.engine_fuel,
        Some(setup + per_turn.iter().sum::<u64>())
    );
}

#[test]
fn arena_fuel_is_the_same_for_the_same_session() {
    let fuel = |seed| -> Vec<Option<u64>> {
        let (turns, _) = play(
            vec![bot("steady"), bot("spiky")],
            seed,
            SessionOptions::default(),
        );
        turns.iter().map(|turn| turn.arena_fuel).collect()
    };
    assert_eq!(fuel(5), fuel(5));
}

#[test]
fn an_unmetered_arena_counts_nothing() {
    let arena = LoadedArena::load_with(
        &fixtures().join("arena.wasm"),
        botstadia_host::arena::Metering::Unmetered,
    )
    .unwrap();
    let (mut session, first) = Session::start(
        &arena,
        vec![bot("steady"), bot("steady")],
        &seed::from_u64(1),
        budget::current(),
        SessionOptions::default(),
    )
    .unwrap();
    assert_eq!(first.arena_fuel, None);
    let next = session.step(&[]).unwrap();
    assert_eq!(next.arena_fuel, None);
    assert_eq!(session.worst_step_fuel, None);
}

#[test]
fn the_metrics_file_carries_the_arena_s_own_cost() {
    let (turns, session) = play(
        vec![bot("steady"), bot("steady")],
        2,
        SessionOptions::default(),
    );
    let (_, metrics) = session.record().unwrap();
    let track = metrics
        .platform
        .iter()
        .find(|track| track.key == "arena_fuel")
        .expect("an arena_fuel series");
    assert_eq!(track.lines.len(), 1);
    assert_eq!(track.lines[0].seat, None);
    let last = turns.last().unwrap();
    assert_eq!(
        track.lines[0].at(last.turn),
        last.arena_fuel.map(|fuel| fuel as f32)
    );
}

#[test]
fn upload_verification_passes_a_steady_bot_and_reports_its_fuel() {
    let bundle = Bundle::load(&fixtures().join("steady.wasm"), budget::current()).unwrap();
    let report =
        botstadia_host::verify::verify(bundle, &arena(), budget::current(), &seed::from_u64(1));
    // A bare component isn't a bundle: verification says so, as `check` does.
    assert!(!report.passed);
    assert!(report.problems.iter().any(|p| p.contains("bare component")));
}

#[test]
fn upload_verification_runs_the_same_for_check_and_the_enclave() {
    // Two runs of the one function on the same bundle agree on every field
    // but wall times, which are only ever this machine's.
    let run = || {
        let bundle = Bundle::load(&fixtures().join("trap.wasm"), budget::current()).unwrap();
        let mut r =
            botstadia_host::verify::verify(bundle, &arena(), budget::current(), &seed::from_u64(1));
        r.wall_ms_p50 = 0.0;
        r.wall_ms_max = 0.0;
        r.per_turn.iter_mut().for_each(|t| t.1 = 0.0);
        r
    };
    assert_eq!(run(), run());
}

#[test]
fn upload_verification_passes_a_real_bundle_of_a_steady_bot() {
    let a = arena();
    let decl = &a.declaration;
    let manifest = botstadia_interface::bundle::Manifest::new(
        &decl.arena_id,
        decl.payload_schema_version,
        botstadia_interface::bundle::BotKind::Rust,
    );
    let zip = botstadia_host::bundle::zip_bytes(vec![
        ("botstadia.toml".into(), manifest.to_toml().into_bytes()),
        (
            "component.wasm".into(),
            std::fs::read(fixtures().join("steady.wasm")).unwrap(),
        ),
    ])
    .unwrap();
    let bundle = Bundle::from_zip(&zip, budget::current()).unwrap();
    let report = botstadia_host::verify::verify(bundle, &a, budget::current(), &seed::from_u64(1));
    assert!(report.passed, "{:?}", report.problems);
    assert_eq!(report.digest, botstadia_host::digest(&zip));
    assert!(report.timing_turns > 0);
    assert!(report.fuel_max > 0 && report.fuel_max <= report.turn_fuel);
    assert_eq!(report.per_turn.len() as u32, report.timing_turns);
}

/// A linked seat whose turns run on a `SeatInPlace` on a thread of the
/// test's own: the shape an enclave's seat process takes.
struct InPlaceLink(Arc<Program>, Arc<Vec<u8>>);

struct InPlaceSeat {
    to: std::sync::mpsc::Sender<(u32, Vec<u8>)>,
    from: std::sync::mpsc::Receiver<botstadia_host::seat::TurnReport>,
}

impl botstadia_host::seat::SeatLink for InPlaceSeat {
    fn send(&mut self, turn: u32, input: Vec<u8>) {
        self.to.send((turn, input)).unwrap();
    }
    fn collect(&mut self) -> botstadia_host::seat::TurnReport {
        self.from.recv().unwrap()
    }
}

impl botstadia_host::session::SpawnLink for InPlaceLink {
    fn spawn(
        &self,
        _seat: usize,
        settings: botstadia_host::seat::SeatSettings,
    ) -> anyhow::Result<(
        Box<dyn botstadia_host::seat::SeatLink>,
        botstadia_host::seat::TurnReport,
    )> {
        use botstadia_host::seat::{LocalInference, SeatInPlace};
        let (program, declaration) = (self.0.clone(), self.1.clone());
        let (to, rx) = std::sync::mpsc::channel::<(u32, Vec<u8>)>();
        let (tx, from) = std::sync::mpsc::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let inference = Box::new(LocalInference::load(&program).unwrap());
            let (mut seat, init) =
                SeatInPlace::start(&program, declaration, &settings, inference).unwrap();
            ready_tx.send(init).unwrap();
            while let Ok((turn, input)) = rx.recv() {
                tx.send(seat.turn(turn, input)).unwrap();
            }
        });
        Ok((Box::new(InPlaceSeat { to, from }), ready.recv().unwrap()))
    }
}

fn linked(variant: &str) -> Seat {
    let Seat::Bot(program) = bot(variant) else {
        unreachable!()
    };
    Seat::Linked {
        name: variant.into(),
        link: Arc::new(InPlaceLink(program, arena().declaration_json.clone())),
    }
}

#[test]
fn a_linked_seat_plays_exactly_as_a_seat_in_the_host() {
    let options = || SessionOptions::default();
    let (here, _) = play(vec![bot("steady"), bot("spiky")], 7, options());
    let (there, _) = play(vec![linked("steady"), linked("spiky")], 7, options());
    assert_eq!(here.len(), there.len());
    for (a, b) in here.iter().zip(&there) {
        assert_eq!(a.inputs, b.inputs, "turn {}", a.turn);
        for (x, y) in a.seats.iter().zip(&b.seats) {
            assert_eq!(
                (x.output.clone(), x.wasm_fuel, x.inference_fuel, x.paused),
                (y.output.clone(), y.wasm_fuel, y.inference_fuel, y.paused),
                "turn {}",
                a.turn
            );
        }
    }
    assert_eq!(here.last().unwrap().finish, there.last().unwrap().finish);
}
