//! `botstadia-runner`: the session runner. In an enclave image (built
//! without `mock`) its tokens come from Confidential Space's launcher; on
//! the local target it's a native process with mock attestation, which
//! `task local:e2e` starts. Its settings come from the environment
//! (`settings::Settings`).
//!
//! It boots (register, mint or fetch the BundleKey, approve), then steps on
//! a fixed cadence: the runner's boundary work, answering handovers, and the
//! result batch when it's due. Ctrl-C or SIGTERM drains it.

use std::sync::Arc;
use std::time::Duration;

use botstadia_chain::rpc::{BlockTag, Client, Http};
use botstadia_enclave::batch::{BatchLimits, Batcher};
use botstadia_enclave::boot::{self, Pace};
use botstadia_enclave::bundle_key::{self, Context, Holding};
use botstadia_enclave::chain_view::TwoProviders;
use botstadia_enclave::client::Http as ApiHttp;
use botstadia_enclave::executor::{JobExecutor, now_ms};
use botstadia_enclave::runner::Runner;
use botstadia_enclave::settings::Settings;
use botstadia_enclave::submit::ChainSubmitter;

/// Where tokens come from: the mock CA locally, the launcher in an image.
#[cfg(feature = "mock")]
fn attester() -> botstadia_enclave::attest::MockAttester {
    botstadia_enclave::attest::MockAttester
}

#[cfg(not(feature = "mock"))]
fn attester() -> botstadia_enclave::attest::Launcher {
    botstadia_enclave::attest::Launcher::default()
}

/// The flat-CPU idle loop on `core`: it runs whenever nothing of a job's
/// does there, so the host sees every job core busy all the time and no
/// session's end (TRUST.md: CPU use stays flat through to its batch).
fn flat_cpu(core: usize) {
    let _ = std::thread::Builder::new()
        .name(format!("idle {core}"))
        .spawn(move || {
            let _ = botstadia_sandbox::pin(&[core]);
            if botstadia_sandbox::idle_priority().is_err() {
                return;
            }
            let mut x = 0u64;
            loop {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        });
}

fn log(line: &str) {
    eprintln!("runner: {line}");
}

fn main() -> anyhow::Result<()> {
    // A worker (a seat's or an arena's process) runs no async runtime: its
    // syscall filter allows no threads it didn't start itself.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("worker") {
        return botstadia_enclave::worker::main(&args[2..]);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let mut s = Settings::from_env()?;
    // Cores (BOTS.md, one seat at a time per core): the first is the
    // canary's, every other holds one job at a time. In an image every job
    // plays isolated and the runner's capacity is its job cores; locally
    // isolation is opt-in (BOTSTADIA_ISOLATE=1) and capacity is
    // BOTSTADIA_SLOTS.
    let cores = botstadia_sandbox::cores();
    // One seat at a time per physical core (BOTS.md): with SMT on, two
    // "cores" here would be one core's two threads, so an image refuses to
    // serve rather than seat two jobs on one core.
    if cfg!(botstadia_image) && botstadia_sandbox::smt_active() {
        anyhow::bail!("SMT is on: this machine must run with one thread per core");
    }
    let mut measured: Option<(f64, f64)> = None;
    let isolate =
        cfg!(botstadia_image) || std::env::var("BOTSTADIA_ISOLATE").is_ok_and(|v| v == "1");
    if isolate {
        let (canary_core, job_cores) = match cores.split_first() {
            Some((c, rest)) if !rest.is_empty() => (Some(*c), rest.to_vec()),
            _ => (None, cores.clone()),
        };
        let canary = Box::leak(Box::new(botstadia_enclave::canary::Canary::start(
            canary_core,
        )?));
        // This machine against the image's calibration, for the record (the
        // heartbeat carries it; a debug image logs it).
        match botstadia_enclave::canary::measure(&canary.clock(), 3.0) {
            Ok((ticks, fuel)) => {
                log(&format!(
                    "calibration: {ticks:.0} canary ticks/s (image: {}), {fuel:.3e} fuel/s (image: {:.3e})",
                    botstadia_enclave::canary::calibration::TICKS_PER_SECOND,
                    botstadia_enclave::canary::calibration::FUEL_PER_SECOND as f64
                ));
                measured = Some((ticks, fuel));
            }
            Err(e) => log(&format!("calibration failed: {e:#}")),
        }
        if cfg!(botstadia_image) {
            s.slots = job_cores.len().max(1);
            for &core in &job_cores {
                flat_cpu(core);
            }
        }
        botstadia_enclave::play::isolate_jobs(
            std::env::current_exe()?,
            cfg!(botstadia_image),
            canary.clock(),
            job_cores,
        );
        log(&format!(
            "isolating jobs: {} slots, canary on {canary_core:?}",
            s.slots
        ));
    }
    let ctx = Context {
        chain_id: s.chain_id,
        results: s.results,
        project: s.project.clone(),
    };
    let head = Client::new(Http::new(s.rpc_urls[0].clone()));
    // Tokens and every timing rule run on block time.
    let block_time = || {
        let head = &head;
        async move {
            head.block(BlockTag::Latest)
                .await
                .map(|b| b.timestamp)
                .unwrap_or(0)
        }
    };
    let now_s = block_time().await;
    let attester = attester();
    let (mut machine, holders) =
        boot::register(ApiHttp::new(s.api_url.clone()), &attester, &ctx, now_s).await?;
    // The way back if the api's nonce and ours ever part: register again on
    // a fresh token, issued at the newest block time we've seen.
    let seen = Arc::new(std::sync::atomic::AtomicU64::new(now_s));
    {
        let seen = seen.clone();
        let binding = botstadia_attestation_core::Binding {
            submit_key: machine.key().address().0.0,
            bundle_key: [0; 32],
            chain_id: s.chain_id,
            results: s.results.0.0,
            instruction: 0,
        };
        machine.set_reregister(Box::new(move || {
            use botstadia_enclave::attest::Attester;
            crate::attester().token(
                &[binding.nonce()],
                seen.load(std::sync::atomic::Ordering::Relaxed),
            )
        }));
    }
    log(&format!("registered {}", machine.address()));
    // An image's providers' keys come from the api, never its launch, whose
    // environment its public token states. The hosts stay the image's own.
    if let Some(chain) = s.keyed_chain {
        use botstadia_shared::api::Empty;
        use botstadia_shared::platform::{FleetRpcKeys, RpcKeys};
        let keys: RpcKeys = machine.call::<FleetRpcKeys, _, _>(Empty {}).await?;
        s.rpc_urls = botstadia_enclave::constants::rpc_urls(chain, &keys.keys)?;
    }
    let view = TwoProviders::connect(&s.rpc_urls, s.chain_id, s.results).await?;
    // Block time moves on while the BundleKey is fetched (a handover can take
    // a while), and a peer's token is issued at its own newest block time: so
    // `seen` follows the chain from here, not just from the main loop.
    {
        let seen = seen.clone();
        let url = s.rpc_urls[0].clone();
        tokio::spawn(async move {
            let head = Client::new(Http::new(url));
            loop {
                if let Ok(b) = head.block(BlockTag::Latest).await {
                    seen.fetch_max(b.timestamp, std::sync::atomic::Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }
    let pace = Pace {
        pause: || tokio::time::sleep(Duration::from_secs(1)),
        polls: 3600,
    };
    let key = boot::bundle_key(
        &machine,
        &view,
        &attester,
        &ctx,
        holders,
        || seen.load(std::sync::atomic::Ordering::Relaxed),
        &pace,
        log,
    )
    .await?;
    log(&format!(
        "holding the BundleKey under instruction {}",
        key.instruction
    ));
    let submitter = ChainSubmitter {
        providers: s
            .rpc_urls
            .iter()
            .map(|u| Client::new(Http::new(u.clone())))
            .collect(),
        key: machine.key().clone(),
        chain_id: s.chain_id,
        results: s.results,
        verifier: s.verifier,
        guest_image_id: s.guest_image_id,
    };
    boot::approve(
        &machine,
        &view,
        &attester,
        &submitter,
        &ctx,
        &key,
        block_time().await,
        &pace,
    )
    .await?;
    log("approved and funded: taking work");

    let machine = Arc::new(machine);
    let batcher = Arc::new(Batcher::new(
        s.results,
        BatchLimits {
            window_ms: s.window_ms,
            size: 40,
            age_ms: s.window_ms * 10,
            hold_polls: 20,
            give_up_ms: botstadia_enclave::constants::RESULT_DEADLINE_S * 1000,
        },
    ));
    // Anchors: one batch per window, never held past it.
    let anchors = Arc::new(Batcher::new(
        s.results,
        BatchLimits {
            window_ms: s.window_ms,
            size: 40,
            age_ms: s.window_ms,
            hold_polls: 20,
            // An anchor not sent within its window goes back to its job,
            // which ends it rather than anchor late.
            give_up_ms: s.window_ms,
        },
    ));
    let executor = JobExecutor {
        machine: machine.clone(),
        settings: s.clone(),
        bundle_key: key.clone(),
        batcher: batcher.clone(),
        anchors: anchors.clone(),
    };
    let mut runner = Runner::new(
        machine.clone(),
        executor,
        s.window_ms,
        s.slots,
        &["session", "warmup", "verify", "rekey"],
    );
    runner.ready = true;
    runner.status.insert(
        "binding".into(),
        format!("{}:{:#x}", s.chain_id, s.results).into(),
    );
    runner
        .status
        .insert("bundle_key_instruction".into(), key.instruction.into());
    runner
        .status
        .insert("submit_key".into(), machine.address().into());
    if let Some((ticks, fuel)) = measured {
        runner.status.insert(
            "calibration".into(),
            serde_json::json!({
                "canary_ticks_per_s": ticks.round(),
                "fuel_per_s": fuel.round(),
                // Whether seats are timed by their own CPU time too
                // (keyholder::seat_time), or by the canary alone.
                "seat_cpu_time": botstadia_enclave::keyholder::cpu_ns(std::process::id()).is_some(),
            }),
        );
    }
    // On GCP (an image): who this machine is, and its maintenance notices.
    let on_gcp = cfg!(botstadia_image);
    if on_gcp && let Some((name, zone)) = botstadia_enclave::gce::identity() {
        runner.status.insert("instance".into(), name.into());
        runner.status.insert("zone".into(), zone.into());
    }
    let mut checked_maintenance = 0u64;
    let mut holding = Holding::default();
    let mut draining = false;
    let mut stop = Box::pin(async {
        let term = async {
            #[cfg(unix)]
            if let Ok(mut t) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                t.recv().await;
                return;
            }
            std::future::pending::<()>().await
        };
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term => {} }
    });
    loop {
        let now = now_ms();
        seen.fetch_max(block_time().await, std::sync::atomic::Ordering::Relaxed);
        if on_gcp && now >= checked_maintenance + 30_000 {
            checked_maintenance = now;
            let noticed = tokio::task::spawn_blocking(botstadia_enclave::gce::maintenance_noticed)
                .await
                .unwrap_or(None);
            match noticed {
                Some(notice) if !runner.status.contains_key("maintenance_at") => {
                    log("maintenance noticed: asking for a second machine");
                    runner.status.insert("maintenance_at".into(), now.into());
                    runner
                        .status
                        .insert("maintenance_notice".into(), notice.into());
                }
                Some(_) => {}
                None => {
                    runner.status.remove("maintenance_at");
                    runner.status.remove("maintenance_notice");
                }
            }
        }
        let (held, oldest) = batcher.held(now);
        runner.status.insert("held_results".into(), held.into());
        runner
            .status
            .insert("held_age_windows".into(), oldest.into());
        match runner.step(now).await {
            Ok(step) => {
                if let Some(id) = &step.claimed {
                    log(&format!("claimed {id}"));
                }
                if anchors.due(now, step.boundary)
                    && let Err(e) = anchors
                        .flush(&submitter, now, || {
                            tokio::time::sleep(Duration::from_millis(250))
                        })
                        .await
                {
                    log(&format!("anchor batch failed, retrying next window: {e:#}"));
                }
                if batcher.due(now, step.boundary)
                    && let Err(e) = batcher
                        .flush(&submitter, now, || {
                            tokio::time::sleep(Duration::from_millis(250))
                        })
                        .await
                {
                    log(&format!("batch failed, retrying next window: {e:#}"));
                }
            }
            Err(e) => log(&format!("step failed: {e:#}")),
        }
        if let Err(e) = bundle_key::serve(
            &machine,
            &view,
            &attester,
            &ctx,
            &key,
            &mut holding,
            block_time().await,
        )
        .await
        {
            log(&format!("handover failed: {e:#}"));
        }
        if runner.is_gone() {
            log("drained and deregistered");
            return Ok(());
        }
        if draining {
            tokio::time::sleep(Duration::from_millis(250)).await;
        } else {
            tokio::select! {
                _ = &mut stop => {
                    log("draining");
                    runner.drain_idle(batcher.held(now_ms()).0 == 0);
                    draining = true;
                }
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }
}
