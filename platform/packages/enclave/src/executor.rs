//! The runner's jobs end to end, each on its own thread with its own small
//! runtime: a session from hold to archive, a warm-up to its landed rating,
//! a verification, a rekey check.

use std::sync::Arc;
use std::time::Duration;

use botstadia_chain::abi::Results;
use botstadia_chain::outcome::Outcome as ChainOutcome;
use botstadia_chain::rpc::{BlockTag, Client, Http};
use botstadia_chain::sign::ResultMessage;
use botstadia_chain::{Address, B256};
use botstadia_shared::api::Empty;
use botstadia_shared::platform::{ArchiveUpload, FleetArchive, JobOffer};
use botstadia_upload::seal::Sealed;
use serde_json::{Value, json};

use crate::anchor::{Anchored, past_close, room_to_anchor, wait_final};
use crate::batch::{Batcher, Entry, Landed};
use crate::bundle_key::BundleKey;
use crate::cdn::AnyCdn;
use crate::chain_view::{ChainView, TwoProviders};
use crate::client::{Http as ApiHttp, Machine};
use crate::hold::{SessionPayload, hold};
use crate::play;
use crate::runner::{Executor, Outcome};
use crate::settings::Settings;
use crate::submit::ChainSubmitter;
use crate::warmup;

pub use crate::constants::RESULT_MIN_AGE_S;

pub struct JobExecutor {
    pub machine: Arc<Machine<ApiHttp>>,
    pub settings: Settings,
    pub bundle_key: BundleKey,
    pub batcher: Arc<Batcher>,
    /// Anchors, one batch per window (TRUST.md#how-the-enclave-talks-to-
    /// the-chain).
    pub anchors: Arc<Batcher>,
}

fn sleep(ms: u64) -> tokio::time::Sleep {
    tokio::time::sleep(Duration::from_millis(ms))
}

fn failed(why: impl std::fmt::Display) -> Outcome {
    Outcome {
        outcome: "failed".into(),
        result: json!({"why": why.to_string()}),
    }
}

impl JobExecutor {
    fn submitter(&self) -> ChainSubmitter {
        ChainSubmitter {
            providers: self
                .settings
                .rpc_urls
                .iter()
                .map(|u| Client::new(Http::new(u.clone())))
                .collect(),
            key: self.machine.key().clone(),
            chain_id: self.settings.chain_id,
            results: self.settings.results,
            verifier: self.settings.verifier,
            guest_image_id: self.settings.guest_image_id,
        }
    }

    async fn view(&self) -> anyhow::Result<TwoProviders> {
        TwoProviders::connect(
            &self.settings.rpc_urls,
            self.settings.chain_id,
            self.settings.results,
        )
        .await
    }

    fn cdn(&self) -> AnyCdn {
        AnyCdn::new(&self.settings.cdn)
    }

    /// Wait for an entry's batch, polling (the batcher runs on the runner's
    /// own loop).
    async fn landed(rx: std::sync::mpsc::Receiver<Landed>) -> anyhow::Result<u64> {
        loop {
            match rx.try_recv() {
                Ok(Ok(block)) => return Ok(block),
                Ok(Err(why)) => anyhow::bail!("the contract skipped it: {why}"),
                Err(std::sync::mpsc::TryRecvError::Empty) => sleep(250).await,
                Err(e) => anyhow::bail!("{e}"),
            }
        }
    }

    async fn session(&self, payload: &Value) -> anyhow::Result<Outcome> {
        let view = self.view().await?;
        let sub = self.submitter();
        let p: SessionPayload = serde_json::from_value(payload.clone())?;
        let spec_hash = p.spec.hash();
        let held = match hold(&self.machine, &view, &self.cdn(), &self.bundle_key, p).await {
            Ok(h) => h,
            // Never anchored: nothing is bound, no outcome exists. Give it
            // back: what it waits on (a post not final yet, a bundle) may
            // come, and it can be re-offered until its anchor.
            Err(e) => {
                eprintln!("session not held yet: {e:#}");
                return Ok(given_back(format!("not held yet: {e:#}")));
            }
        };
        let session: B256 = held.payload.session_id.parse()?;
        let me = self.machine.key().address();
        let mut held = held;
        loop {
            while !past_close(&sub, held.payload.close).await? {
                sleep(500).await;
            }
            // Never an anchor whose result couldn't land in time: the
            // finality lag now, the longest session, the result's age.
            let (latest, finalized) = view.head_times().await?;
            let longest = crate::keyholder::longest_session_s(&held.arena, held.bundles.len())?;
            if !room_to_anchor(
                latest,
                held.payload.close,
                latest.saturating_sub(finalized),
                longest,
                self.settings.window_ms / 1000,
            ) {
                eprintln!("not anchoring {session:#x}: the finality lag leaves its result no room");
                return Ok(given_back(
                    "not anchoring: the finality lag leaves its result no room".into(),
                ));
            }
            // In this window's anchor batch. A send that errors may still
            // have landed: given back, the api ends it failed if its anchor
            // is indexed, and re-offers it otherwise.
            let rx = self.anchors.queue(
                Entry::Anchor {
                    session,
                    item: crate::anchor::item(&held),
                },
                now_ms(),
            );
            if let Err(why) = Self::landed(rx).await {
                // Not anchored at all (never sent, or skipped before its
                // anchor): another machine may still anchor it in time.
                // Bound to this key, though its batch's receipt was lost:
                // the anchor hash isn't on-chain to run it from, so it ends.
                return Ok(match view.bound_key(session).await {
                    Ok(k) if k == Address::ZERO => {
                        given_back(format!("anchor not landed: {why:#}"))
                    }
                    _ => failed(format!("anchor refused: {why:#}")),
                });
            }
            let Some((anchor, block)) = self.anchors.anchor_of(session) else {
                return Ok(failed("anchor refused: no anchor in its batch"));
            };
            let a = Anchored {
                session,
                key: me,
                anchor,
                block,
            };
            wait_final(&view, a.included_in(), || sleep(500), 7200).await?;
            // Final and still ours: run it. Rolled back: nothing has run, so
            // every rule it was anchored on is checked again against the
            // chain as it is now before it's resubmitted.
            if view.bound_key(session).await? == me {
                // From the anchor on, the session is this key's alone: a
                // failure is reported as one, in claim order, never given
                // back.
                return match self.anchored(&view, &sub, &held, a, spec_hash).await {
                    Ok(o) => Ok(o),
                    Err(e) => Ok(failed(format!("after its anchor: {e:#}"))),
                };
            }
            eprintln!("the anchor of {session:#x} was rolled back: rechecking it");
            held = match hold(
                &self.machine,
                &view,
                &self.cdn(),
                &self.bundle_key,
                held.payload.clone(),
            )
            .await
            {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("session not held after its rollback: {e:#}");
                    return Ok(given_back(format!("not held after its rollback: {e:#}")));
                }
            };
        }
    }

    /// Run, sign and submit an anchored session, then upload its archive.
    async fn anchored(
        &self,
        view: &TwoProviders,
        sub: &ChainSubmitter,
        held: &crate::hold::Held,
        a: crate::anchor::Anchored,
        spec_hash: B256,
    ) -> anyhow::Result<Outcome> {
        wait_final(view, a.included_in(), || sleep(500), 7200).await?;
        let seed = play::seed(a.session, a.anchor, play::fresh()?);
        let played = tokio::task::block_in_place(|| play::play_job(held, &seed, &play::LIMITS));
        let message = ResultMessage {
            chain_id: self.settings.chain_id,
            results: self.settings.results,
            session: a.session,
            outcome: played.outcome.encode(),
            anchor: a.anchor,
            anchor_block: a.block,
            seed: B256::from(seed),
            spec_hash,
            archive_hash: played.archive_hash(),
        };
        let signature = self.machine.key().sign_message(&message.encode())?;
        // Held until the contract's minimum anchor age, by block time.
        let provider = &sub.providers[0];
        let anchor_time = provider.block(BlockTag::Number(a.block)).await?.timestamp;
        while provider.block(BlockTag::Latest).await?.timestamp < anchor_time + RESULT_MIN_AGE_S {
            sleep(500).await;
        }
        let rx = self.batcher.queue(
            Entry::Result(Results::ResultItem {
                session: a.session,
                anchor: a.anchor,
                anchorBlock: a.block,
                specHash: spec_hash,
                outcome: played.outcome.encode().into(),
                seed: B256::from(seed),
                archiveHash: played.archive_hash(),
                signature,
            }),
            now_ms(),
        );
        if let Err(e) = Self::landed(rx).await {
            // A send that failed but landed, resent and skipped "already
            // resolved": only the bound key's result is ever accepted, so an
            // accepted result is ours, and its archive still goes up.
            if view.result(a.session).await?.is_none() || view.bound_key(a.session).await? != a.key
            {
                return Err(e);
            }
        }
        // Only now the archive, once the api has indexed the result.
        if !played.archive.is_empty() {
            let upload = ArchiveUpload {
                session_id: format!("{:#x}", a.session),
                archive: Sealed::b64(&played.archive),
                metrics: Sealed::b64(&played.metrics),
            };
            let mut uploaded = false;
            for _ in 0..600 {
                let r: anyhow::Result<Empty> = self
                    .machine
                    .call::<FleetArchive, _, _>(upload.clone())
                    .await;
                if r.is_ok() {
                    uploaded = true;
                    break;
                }
                sleep(1000).await;
            }
            if !uploaded {
                return Ok(Outcome {
                    outcome: "done".into(),
                    result: json!({"incidents": played.incidents, "archive": "lost"}),
                });
            }
        }
        let void = matches!(played.outcome, ChainOutcome::Void { .. });
        Ok(Outcome {
            outcome: "done".into(),
            result: json!({"incidents": played.incidents, "void": void}),
        })
    }

    async fn warmup(&self, payload: &Value) -> anyhow::Result<Outcome> {
        let view = self.view().await?;
        let c = warmup::constants();
        let p: warmup::WarmupPayload = serde_json::from_value(payload.clone())?;
        let prepared =
            match warmup::prepare(&self.machine, &view, &self.cdn(), &self.bundle_key, p, &c)
                .await?
            {
                Ok(p) => p,
                Err(why) => return Ok(failed(why)),
            };
        let (f, incidents) = tokio::task::block_in_place(|| {
            warmup::run(
                &prepared,
                &c,
                &play::LIMITS,
                play::fresh().unwrap_or([0; 32]),
            )
        });
        let Some(f) = f else {
            return Ok(Outcome {
                outcome: "failed".into(),
                result: json!({"why": "past the maximum attempts", "seat_incidents": incidents}),
            });
        };
        let item = warmup::rating_item(
            self.machine.key(),
            self.settings.chain_id,
            self.settings.results,
            &prepared,
            &f,
        )?;
        // A rating already set (a send that failed but landed, resent) is
        // this job's end all the same: a bot is rated once.
        if let Err(e) = Self::landed(self.batcher.queue(Entry::Rating(item), now_ms())).await
            && !format!("{e:#}").contains("already rated")
        {
            return Err(e);
        }
        Ok(Outcome {
            outcome: "done".into(),
            result: warmup::finished(incidents),
        })
    }

    async fn run_async(&self, offer: &JobOffer) -> anyhow::Result<Outcome> {
        let payload: Value = serde_json::from_str(&offer.payload)?;
        match offer.kind.as_str() {
            "session" => self.session(&payload).await,
            "warmup" => self.warmup(&payload).await,
            "verify" => {
                crate::jobs::verify(
                    &self.machine,
                    &self.view().await?,
                    &self.cdn(),
                    &self.bundle_key,
                    &payload,
                )
                .await
            }
            "rekey" => crate::jobs::rekey(&self.machine, &self.bundle_key, &payload).await,
            other => Ok(failed(format!("unknown job kind {other}"))),
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Executor for JobExecutor {
    fn run(&self, offer: &JobOffer) -> Outcome {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => return failed(e),
        };
        match rt.block_on(self.run_async(offer)) {
            Ok(o) => o,
            // A machine-side failure before any outcome: give the job back.
            Err(e) => {
                eprintln!("job {} ({}) failed: {e:#}", offer.id, offer.kind);
                Outcome {
                    outcome: "release".into(),
                    result: json!({}),
                }
            }
        }
    }
}

/// A session given back unanchored, with the runner's reason, which the api
/// logs and alerts with: a production runner shows nothing else (found on
/// the testnet, a session given back each window with no reason).
fn given_back(why: String) -> Outcome {
    Outcome {
        outcome: "release".into(),
        result: json!({"why": why.chars().take(500).collect::<String>()}),
    }
}
