//! The runner's jobs end to end, each on its own thread with its own small
//! runtime: a session from hold to archive, a warm-up to its landed rating,
//! a verification, a rekey check.

use std::sync::Arc;
use std::time::Duration;

use botstadia_chain::abi::Results;
use botstadia_chain::outcome::Outcome as ChainOutcome;
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
use crate::chain_view::{ChainView, TwoProviders, client, serving};
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
    pub ran: crate::anchor::RunOnce,
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

// How often a waiting job asks the chain. Free RPC tiers pay for each ask,
// and none of these waits is decided by seconds.
/// For a close to pass: the anchor's batch window is minutes.
const CLOSE_POLL_MS: u64 = 3_000;
/// For finality, which takes about twenty minutes.
const FINAL_POLL_MS: u64 = 10_000;
/// Until the result deadline: finality may lag for hours, and giving up
/// ends a bound session with no result.
const FINAL_POLLS: usize = (crate::constants::RESULT_DEADLINE_S * 1000 / FINAL_POLL_MS) as usize;
/// Between tries of a bound session's read.
const RETRY_MS: u64 = 10_000;
/// Reads, that far apart, before a send that errored counts as not landed.
const LANDED_LOOKS: usize = 6;
/// Searches for a lost anchor's block at the quick pace, each about 25
/// reads; after them, one every ten minutes to the deadline.
const SEARCHES: usize = 30;
const SEARCH_SLOW_MS: u64 = 600_000;

/// A bound session's read, retried to the deadline: giving up ends it with
/// no result, and nobody runs it again (TRUST.md#every-session-one-flow).
macro_rules! until_deadline {
    ($until:expr, $read:expr) => {
        loop {
            match $read {
                Ok(v) => break Ok(v),
                Err(e) if now_ms() >= $until => break Err(e),
                Err(e) => {
                    eprintln!("a bound session's read failed, trying again: {e:#}");
                    sleep(RETRY_MS).await;
                }
            }
        }
    };
}
/// For the anchor's minimum age, fifteen minutes.
const AGE_POLL_MS: u64 = 5_000;

impl JobExecutor {
    fn submitter(&self, urls: &[String]) -> ChainSubmitter {
        ChainSubmitter {
            providers: urls.iter().map(|u| client(u)).collect(),
            key: self.machine.key().clone(),
            chain_id: self.settings.chain_id,
            results: self.settings.results,
            verifier: self.settings.verifier,
            guest_image_id: self.settings.guest_image_id,
        }
    }

    /// The providers serving the binding's chain now: checked for each job.
    async fn urls(&self) -> anyhow::Result<Vec<String>> {
        serving(&self.settings.rpc_urls, self.settings.chain_id).await
    }

    async fn view(&self) -> anyhow::Result<TwoProviders> {
        Ok(Self::view_of(&self.urls().await?, self.settings.results))
    }

    fn view_of(urls: &[String], results: Address) -> TwoProviders {
        TwoProviders::new(urls.iter().map(|u| client(u)).collect(), results)
    }

    /// After an anchor batch that errored or was skipped, the chain's word:
    /// a send whose answer was lost may have bound the session to this key.
    /// `None`: not anchored at all.
    async fn anchor_landed(
        view: &TwoProviders,
        session: B256,
        spec_hash: B256,
        until: u64,
    ) -> anyhow::Result<Option<(Address, Option<(B256, u64)>)>> {
        let (mut unbound, mut searches) = (0, 0);
        loop {
            let (key, time) = until_deadline!(until, view.anchor_included(session).await)?;
            if key != Address::ZERO {
                match view.lost_anchor(session, time, spec_hash).await {
                    Ok(at) => return Ok(Some((key, Some(at)))),
                    Err(e) => {
                        searches += 1;
                        // Each reads the binding again: a reorg moves it.
                        anyhow::ensure!(now_ms() < until, "no anchor found: {e:#}");
                        eprintln!("the lost anchor isn't found yet: {e:#}");
                        // Bound: never ended by a bad few minutes, only asked less.
                        let wait = if searches < SEARCHES {
                            RETRY_MS
                        } else {
                            SEARCH_SLOW_MS
                        };
                        sleep(wait).await;
                        continue;
                    }
                }
            }
            unbound += 1;
            if unbound >= LANDED_LOOKS {
                return Ok(None);
            }
            sleep(RETRY_MS).await;
        }
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
        let urls = self.urls().await?;
        let view = Self::view_of(&urls, self.settings.results);
        let sub = self.submitter(&urls);
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
            let mut unread = 0u32;
            loop {
                match past_close(&sub, held.payload.close).await {
                    Ok(true) => break,
                    Ok(false) => unread = 0,
                    Err(e) => {
                        unread += 1;
                        // Unanchored still: five minutes unread gives it back.
                        if unread >= 100 {
                            return Ok(given_back(format!("no head to anchor by: {e:#}")));
                        }
                    }
                }
                sleep(CLOSE_POLL_MS).await;
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
            let until = (held.payload.close + crate::constants::RESULT_DEADLINE_S) * 1000;
            let sent = Self::landed(rx).await;
            let (anchor, block) = match (sent, self.anchors.anchor_of(session)) {
                (Ok(_), Some(at)) => at,
                (sent, _) => {
                    let why = sent
                        .err()
                        .map_or("no anchor in its batch".into(), |e| format!("{e:#}"));
                    match Self::anchor_landed(&view, session, spec_hash, until).await {
                        Ok(None) => return Ok(given_back(format!("anchor not landed: {why}"))),
                        Ok(Some((key, Some(at)))) if key == me => at,
                        Ok(Some(_)) => return Ok(failed(format!("anchor refused: {why}"))),
                        Err(e) => return Ok(failed(format!("anchor unknown: {why}: {e:#}"))),
                    }
                }
            };
            let a = Anchored {
                session,
                key: me,
                anchor,
                block,
            };
            // Bound from here: nothing gives it back.
            if let Err(e) =
                wait_final(&view, a.included_in(), || sleep(FINAL_POLL_MS), FINAL_POLLS).await
            {
                return Ok(failed(format!("after its anchor: {e:#}")));
            }
            // Final and still ours: run it. Rolled back: nothing has run, so
            // every rule it was anchored on is checked again against the
            // chain as it is now before it's resubmitted.
            let bound = match until_deadline!(until, view.bound_key(session).await) {
                Ok(bound) => bound,
                Err(e) => return Ok(failed(format!("after its anchor: {e:#}"))),
            };
            if bound == me {
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
        wait_final(view, a.included_in(), || sleep(FINAL_POLL_MS), FINAL_POLLS).await?;
        // The contract's minimum age, by its own anchor time, waited out
        // before play: so nothing is read when a session ends, which the
        // providers' logs would show.
        let until = (held.payload.close + crate::constants::RESULT_DEADLINE_S) * 1000;
        let due = until_deadline!(until, view.anchor_time(a.session).await)? + RESULT_MIN_AGE_S;
        while until_deadline!(until, crate::chain_view::head_time(&sub.providers).await)? < due {
            anyhow::ensure!(now_ms() < until, "the anchor's minimum age never passed");
            sleep(AGE_POLL_MS).await;
        }
        // The last thing before a seed exists: recovering a duplicate's
        // binding from the chain must never mean a second run.
        self.ran.begin(a.session)?;
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
            // accepted result is ours, and its archive still goes up. Asked
            // as included, since finality is twenty minutes behind it.
            let mut ours = false;
            for _ in 0..LANDED_LOOKS * 5 {
                if let Ok(true) = view.result_included(a.session).await
                    && let Ok((key, _)) = view.anchor_included(a.session).await
                {
                    ours = key == a.key;
                    break;
                }
                sleep(RETRY_MS).await;
            }
            if !ours {
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
