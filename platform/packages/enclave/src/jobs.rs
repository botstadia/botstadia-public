//! Verification and rekey checks (TRUST.md#bundle-encryption, BOTS.md
//! bundles). Neither has an outcome, so both report at once.
//!
//! - **Verification** decrypts the upload (or takes a seed bot's plaintext
//!   from the CDN), checks its digest, and runs the upload checks against
//!   the arena by digest: compile, instantiate, the ONNX preflight and the
//!   timing run, seating only this bundle. The report is signed with the
//!   SubmitKey and published on the bot's profile; a failure's reason goes
//!   to the builder only.
//! - **A rekey check** opens the pending rewrap with the BundleKey and checks
//!   the plaintext is the bot's digest, so a rekey can't swap the bundle.

use botstadia_shared::platform::{BundleAsk, BundleFetched, FleetBundle};
use botstadia_upload::seal::{from_b64, open_with_bundle_key};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::bundle_key::BundleKey;
use crate::cdn::Cdn;
use crate::chain_view::ChainView;
use crate::client::{Machine, Transport};
use crate::runner::Outcome;

/// A verification or rekey job's payload (`work::offer`).
#[derive(Debug, Clone, Deserialize)]
pub struct BotPayload {
    pub bot_id: String,
    pub digest: String,
    #[serde(default)]
    pub arena_digest: Option<String>,
    /// The bot's arena's slug, where the CDN keeps its files.
    #[serde(default)]
    pub arena_id: String,
}

/// A fault of ours (BOTS.md#bundles): no verdict, queued again in its place.
fn fault(why: &str) -> Outcome {
    Outcome {
        outcome: "fault".into(),
        result: json!({"why": why}),
    }
}

/// Why a bot's plaintext isn't to hand: the bundle's own failing (a
/// verdict), or ours (a seed bot's file the CDN wouldn't serve), which is
/// tried again.
#[derive(Debug)]
pub enum NoPlaintext {
    Bundle(String),
    Ours(String),
}

fn release() -> Outcome {
    Outcome {
        outcome: "release".into(),
        result: json!({}),
    }
}

/// The bot's plaintext: a seed bot's from the CDN, any other decrypted.
pub async fn plaintext<T: Transport>(
    machine: &Machine<T>,
    view: &impl ChainView,
    cdn: &impl Cdn,
    key: &BundleKey,
    p: &BotPayload,
) -> anyhow::Result<Result<Vec<u8>, NoPlaintext>> {
    let digest = botstadia_chain::spec::digest_bytes(&p.digest)?;
    if view.seed_bot(digest).await? {
        return Ok(cdn
            .seed_bot(&p.arena_id, &p.digest)
            .map_err(|e| NoPlaintext::Ours(format!("{e:#}"))));
    }
    let fetched: BundleFetched = machine
        .call::<FleetBundle, _, _>(BundleAsk {
            bot_id: p.bot_id.clone(),
        })
        .await?;
    if fetched.instruction != key.instruction {
        return Ok(Err(NoPlaintext::Bundle(
            "the bundle is wrapped to another BundleKey".into(),
        )));
    }
    Ok(open_with_bundle_key(
        &key.secret,
        &p.digest,
        &from_b64(&fetched.wrapped_bundle_key)?,
        &from_b64(&fetched.ciphertext)?,
    )
    .map_err(|_| NoPlaintext::Bundle("the bundle doesn't open to its digest".into())))
}

/// Run a verification job.
pub async fn verify<T: Transport>(
    machine: &Machine<T>,
    view: &impl ChainView,
    cdn: &impl Cdn,
    key: &BundleKey,
    payload: &Value,
) -> anyhow::Result<Outcome> {
    let p: BotPayload = serde_json::from_value(payload.clone())?;
    // Only against an arena whose digest is in a post.
    let Some(arena_digest) = &p.arena_digest else {
        return Ok(release());
    };
    let budget = botstadia_host::budget::current();
    // A failure of the bundle's own is a verdict, signed like any report
    // (the api accepts no other); one of ours is none.
    let failing = |why: String| botstadia_host::verify::Report {
        digest: p.digest.clone(),
        arena_digest: arena_digest.clone(),
        budget_version: budget.id,
        passed: false,
        problems: vec![why],
        ..Default::default()
    };
    let report = match plaintext(machine, view, cdn, key, &p).await? {
        Ok(zip) => {
            let arena_bytes = cdn.arena(&p.arena_id, arena_digest)?;
            // A fresh seed for each timing run, so no bot can recognise one.
            let seed = crate::play::fresh()?;
            // In a worker of its own where the runner isolates.
            let memory = crate::worker::Kind::Verify.memory_cap(budget);
            let verdict = crate::play::with_isolation(memory, |iso| match iso {
                Some(iso) => crate::keyholder::verify(iso, budget.id, &zip, &arena_bytes, &seed),
                None => {
                    use crate::keyholder::VerifyError;
                    let arena = botstadia_host::LoadedArena::from_bytes(&arena_bytes)
                        .map_err(|e| VerifyError::Fault(format!("{e:#}")))?;
                    botstadia_host::Bundle::from_zip(&zip, budget)
                        .map(|bundle| botstadia_host::verify::verify(bundle, &arena, budget, &seed))
                        .map_err(|e| VerifyError::Bundle(format!("the bundle doesn't load: {e:#}")))
                }
            });
            match verdict {
                Ok(r) => r,
                Err(crate::keyholder::VerifyError::Bundle(why)) => failing(why),
                Err(crate::keyholder::VerifyError::Fault(why)) => return Ok(fault(&why)),
            }
        }
        Err(NoPlaintext::Bundle(why)) => failing(why),
        Err(NoPlaintext::Ours(why)) => {
            eprintln!("verify {}: {why}", p.bot_id);
            return Ok(Outcome {
                outcome: "release".into(),
                result: json!({"why": why}),
            });
        }
    };
    let mut body = serde_json::to_value(&report)?;
    body["bot_id"] = Value::from(p.bot_id.clone());
    // The api checks the signature over the report as it re-serializes what
    // it parsed, and parsing doesn't round-trip every float: sign the form
    // that re-serializes to itself.
    let body = canonical(body)?;
    let signature = machine
        .key()
        .sign_message(body.to_string().as_bytes())?
        .to_string();
    let why = report.problems.join("; ");
    Ok(Outcome {
        outcome: if report.passed { "done" } else { "failed" }.into(),
        result: json!({"report": body, "signature": signature, "why": why}),
    })
}

/// Run a rekey check.
pub async fn rekey<T: Transport>(
    machine: &Machine<T>,
    key: &BundleKey,
    payload: &Value,
) -> anyhow::Result<Outcome> {
    let p: BotPayload = serde_json::from_value(payload.clone())?;
    let fetched: BundleFetched = machine
        .call::<FleetBundle, _, _>(BundleAsk {
            bot_id: p.bot_id.clone(),
        })
        .await?;
    let Some(pending) = fetched.pending_wrapped_key else {
        return Ok(Outcome {
            outcome: "failed".into(),
            result: json!({"digest_matches": false, "why": "no rekey pending"}),
        });
    };
    let matches = open_with_bundle_key(
        &key.secret,
        &p.digest,
        &from_b64(&pending)?,
        &from_b64(&fetched.ciphertext)?,
    )
    .is_ok();
    Ok(Outcome {
        outcome: if matches { "done" } else { "failed" }.into(),
        result: json!({"digest_matches": matches}),
    })
}

/// A JSON value in the form its own parse gives back unchanged.
pub fn canonical(mut v: Value) -> anyhow::Result<Value> {
    for _ in 0..4 {
        let text = v.to_string();
        let back: Value = serde_json::from_str(&text)?;
        let again = back.to_string();
        if again == text {
            return Ok(back);
        }
        v = back;
    }
    anyhow::bail!("the report doesn't settle under JSON round trips")
}
