//! The BundleKey (TRUST.md#keeping-the-bundlekey-alive): minted only under a
//! new-key instruction with no holder, otherwise fetched from a holder by
//! mutual attested exchange through the api's relay, which only carries
//! tokens and ciphertext.
//!
//! The exchange, for a requester R and a holder H:
//!
//! 1. R → H `challenge`: R's fresh challenge `c_r`.
//! 2. H → R `challenge`: H's own challenge `c_h`, H's binding (its SubmitKey,
//!    its BundleKey's public half, the chain binding, the instruction), and a
//!    token whose nonce is that binding's handover nonce over `c_r`.
//! 3. R checks H's token itself: the signature chain, our project, the root
//!    and image usable on-chain, the nonce, the sender, and that the binding
//!    is its own chain binding. Then R → H `request`: R's binding, naming a
//!    fresh X25519 key it receives on, and a token over `c_h`.
//! 4. H checks R's the same way, seals the BundleKey to the key R's token
//!    attests under the handover label, and signs the reply with its
//!    SubmitKey: `reply`.
//! 5. R checks the signature is H's attested key, opens the BundleKey, and
//!    accepts it only if it's the key the chain holds for the instruction.

use std::collections::HashMap;

use botstadia_attestation_core::{Binding, verify_token};
use botstadia_chain::sign::recover;
use botstadia_chain::{Address, B256};
use botstadia_shared::api::Empty;
use botstadia_shared::platform::{
    FleetHolding, FleetMint, FleetRelayInbox, FleetRelaySend, Holder, HoldingKey, MintAnswer,
    MintAsk, RelayMessage, RelaySend,
};
use botstadia_upload::seal::{LABEL_HANDOVER, unwrap, wrap, x25519_generate, x25519_public};
use serde_json::{Value, json};

use crate::attest::Attester;
use crate::chain_view::ChainView;
use crate::client::{Machine, Transport};

#[derive(Clone)]
pub struct BundleKey {
    pub secret: [u8; 32],
    pub public: [u8; 32],
    pub instruction: u64,
}

impl std::fmt::Debug for BundleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BundleKey(0x{}, instruction {})",
            hex::encode(self.public),
            self.instruction
        )
    }
}

/// The chain binding and project every peer must share.
#[derive(Debug, Clone)]
pub struct Context {
    pub chain_id: u64,
    pub results: Address,
    pub project: String,
}

fn hex32(v: &Value) -> anyhow::Result<[u8; 32]> {
    let b = hex::decode(v.as_str().unwrap_or("").trim_start_matches("0x"))?;
    b.try_into()
        .map_err(|_| anyhow::anyhow!("expected 32 bytes"))
}

fn binding_json(b: &Binding) -> Value {
    json!({
        "submit_key": format!("0x{}", hex::encode(b.submit_key)),
        "bundle_key": format!("0x{}", hex::encode(b.bundle_key)),
        "chain_id": b.chain_id,
        "results": format!("0x{}", hex::encode(b.results)),
        "instruction": b.instruction,
    })
}

fn binding_from(v: &Value) -> anyhow::Result<Binding> {
    let addr = |k: &str| -> anyhow::Result<[u8; 20]> {
        let b = hex::decode(v[k].as_str().unwrap_or("").trim_start_matches("0x"))?;
        b.try_into()
            .map_err(|_| anyhow::anyhow!("expected 20 bytes"))
    };
    Ok(Binding {
        submit_key: addr("submit_key")?,
        bundle_key: hex32(&v["bundle_key"])?,
        chain_id: v["chain_id"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("no chain id"))?,
        results: addr("results")?,
        instruction: v["instruction"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("no instruction"))?,
    })
}

fn random32() -> anyhow::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(b)
}

/// Check a peer's token and binding (step 3 and 4): everything from the
/// token and the chain, nothing on the api's say-so.
pub async fn check_peer(
    view: &impl ChainView,
    ctx: &Context,
    from: &str,
    token: &str,
    binding: &Binding,
    challenge: &[u8; 32],
    now_s: u64,
) -> anyhow::Result<()> {
    let t = verify_token(token, &[], now_s)
        .map_err(|e| anyhow::anyhow!("the peer's token doesn't verify: {e:?}"))?;
    anyhow::ensure!(
        t.nonces.contains(&binding.handover_nonce(challenge)),
        "the peer's token doesn't answer our challenge"
    );
    anyhow::ensure!(
        format!("0x{}", hex::encode(binding.submit_key)) == from.to_lowercase(),
        "the token binds another key than the sender's"
    );
    anyhow::ensure!(
        binding.chain_id == ctx.chain_id && binding.results == ctx.results.0.0,
        "another chain binding"
    );
    anyhow::ensure!(t.project == ctx.project, "another project");
    anyhow::ensure!(
        view.root_usable(B256::from(t.root_hash)).await?,
        "the peer's root isn't usable on-chain"
    );
    anyhow::ensure!(
        view.image_usable(B256::from(t.image_digest)).await?,
        "the peer's image isn't usable on-chain"
    );
    Ok(())
}

/// What to do at boot (TRUST.md#minting-a-bundlekey).
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Ask holders of the newest instruction in effect.
    Fetch(u64, Vec<Holder>),
    /// No holder and the instruction unfilled: mint, if the api confirms.
    Mint(u64),
    /// Retry later, and alert.
    Wait(String),
}

pub async fn plan(view: &impl ChainView, holders: &[Holder]) -> anyhow::Result<Plan> {
    let Some((n, held)) = view.newest_instruction().await? else {
        return Ok(Plan::Wait("no new-key instruction in effect".into()));
    };
    let of_n: Vec<Holder> = holders
        .iter()
        .filter(|h| h.instruction == n)
        .cloned()
        .collect();
    if !of_n.is_empty() {
        return Ok(Plan::Fetch(n, of_n));
    }
    if held == B256::ZERO {
        return Ok(Plan::Mint(n));
    }
    Ok(Plan::Wait(format!(
        "instruction {n} is filled but no holder can be found"
    )))
}

/// Mint under `instruction` if the api confirms no other minter.
pub async fn mint<T: Transport>(
    machine: &Machine<T>,
    instruction: u64,
) -> anyhow::Result<Option<BundleKey>> {
    let answer: MintAnswer = machine
        .call::<FleetMint, _, _>(MintAsk { instruction })
        .await?;
    if !answer.mint {
        return Ok(None);
    }
    let (secret, public) = x25519_generate()?;
    Ok(Some(BundleKey {
        secret,
        public,
        instruction,
    }))
}

/// Tell the api it holds the key (step 4 of bootstrap).
pub async fn report_holding<T: Transport>(
    machine: &Machine<T>,
    key: &BundleKey,
) -> anyhow::Result<()> {
    let _: Empty = machine
        .call::<FleetHolding, _, _>(HoldingKey {
            bundle_key_pub: format!("0x{}", hex::encode(key.public)),
            instruction: key.instruction,
        })
        .await?;
    Ok(())
}

async fn send<T: Transport>(
    machine: &Machine<T>,
    to: &str,
    kind: &str,
    body: Value,
) -> anyhow::Result<()> {
    let _: Empty = machine
        .call::<FleetRelaySend, _, _>(RelaySend {
            to: to.into(),
            kind: kind.into(),
            body: body.to_string(),
        })
        .await?;
    Ok(())
}

async fn inbox<T: Transport>(machine: &Machine<T>) -> anyhow::Result<Vec<RelayMessage>> {
    machine
        .call::<FleetRelayInbox, _, _>(botstadia_shared::api::Empty {})
        .await
}

fn reply_message(wrapped: &str, to: &str) -> String {
    format!("botstadia-handover-reply-v1\n{wrapped}\n{to}")
}

/// Fetch the key from one holder (the requester's side). `pause` runs
/// between inbox polls; `polls` bounds the wait.
#[allow(clippy::too_many_arguments)]
pub async fn fetch<T: Transport, F: std::future::Future<Output = ()>>(
    machine: &Machine<T>,
    view: &impl ChainView,
    attester: &impl Attester,
    ctx: &Context,
    holder: &Holder,
    instruction: u64,
    now_s: impl Fn() -> u64,
    pause: impl Fn() -> F,
    polls: usize,
) -> anyhow::Result<BundleKey> {
    let c_r = random32()?;
    send(
        machine,
        &holder.address,
        "challenge",
        json!({"challenge": format!("0x{}", hex::encode(c_r))}),
    )
    .await?;
    let (eph_secret, eph_public) = x25519_generate()?;
    let mut holder_binding: Option<Binding> = None;
    for _ in 0..polls {
        for m in inbox(machine).await? {
            if m.from.to_lowercase() != holder.address.to_lowercase() {
                continue;
            }
            let body: Value = serde_json::from_str(&m.body).unwrap_or_default();
            match m.kind.as_str() {
                "challenge" if holder_binding.is_none() => {
                    let binding = binding_from(&body["binding"])?;
                    check_peer(
                        view,
                        ctx,
                        &m.from,
                        body["token"].as_str().unwrap_or(""),
                        &binding,
                        &c_r,
                        now_s(),
                    )
                    .await?;
                    anyhow::ensure!(
                        binding.instruction == instruction,
                        "the holder's key is for another instruction"
                    );
                    let c_h = hex32(&body["challenge"])?;
                    let mine = Binding {
                        submit_key: machine.key().address().0.0,
                        bundle_key: eph_public,
                        chain_id: ctx.chain_id,
                        results: ctx.results.0.0,
                        instruction,
                    };
                    let token = attester.token(&[mine.handover_nonce(&c_h)], now_s())?;
                    send(
                        machine,
                        &m.from,
                        "request",
                        json!({"binding": binding_json(&mine), "token": token}),
                    )
                    .await?;
                    holder_binding = Some(binding);
                }
                "reply" => {
                    let binding = holder_binding
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("a reply before the exchange"))?;
                    let wrapped = body["wrapped"].as_str().unwrap_or("");
                    let to = body["to"].as_str().unwrap_or("");
                    anyhow::ensure!(
                        to == format!("0x{}", hex::encode(eph_public)),
                        "the reply is sealed to another key"
                    );
                    let sig: botstadia_chain::Bytes =
                        body["signature"].as_str().unwrap_or("").parse()?;
                    let signer = recover(reply_message(wrapped, to).as_bytes(), &sig)?;
                    anyhow::ensure!(
                        signer.0.0 == binding.submit_key,
                        "the reply isn't signed by the holder's attested key"
                    );
                    let secret: [u8; 32] = unwrap(
                        &eph_secret,
                        LABEL_HANDOVER,
                        &binding.submit_key,
                        &hex::decode(wrapped.trim_start_matches("0x"))?,
                    )?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("a BundleKey is 32 bytes"))?;
                    let public = x25519_public(&secret);
                    anyhow::ensure!(
                        public == binding.bundle_key,
                        "the key isn't the one the holder's token attests"
                    );
                    let held = view.held_key(instruction).await?;
                    anyhow::ensure!(
                        B256::from(public) == held,
                        "the key isn't the one the chain holds for instruction {instruction}"
                    );
                    return Ok(BundleKey {
                        secret,
                        public,
                        instruction,
                    });
                }
                "refusal" => anyhow::bail!("the holder refused: {}", body["reason"]),
                _ => {}
            }
        }
        pause().await;
    }
    anyhow::bail!("the holder didn't answer")
}

/// A holder's side: challenges it has issued, by requester.
#[derive(Default)]
pub struct Holding {
    challenges: HashMap<String, [u8; 32]>,
}

/// Answer every handover message waiting (the holder's side). Returns how
/// many keys it handed over.
pub async fn serve<T: Transport>(
    machine: &Machine<T>,
    view: &impl ChainView,
    attester: &impl Attester,
    ctx: &Context,
    key: &BundleKey,
    state: &mut Holding,
    now_s: u64,
) -> anyhow::Result<usize> {
    let mut handed = 0;
    let mine = Binding {
        submit_key: machine.key().address().0.0,
        bundle_key: key.public,
        chain_id: ctx.chain_id,
        results: ctx.results.0.0,
        instruction: key.instruction,
    };
    for m in inbox(machine).await? {
        let body: Value = serde_json::from_str(&m.body).unwrap_or_default();
        let from = m.from.to_lowercase();
        let outcome: anyhow::Result<()> = async {
            match m.kind.as_str() {
                "challenge" => {
                    let c_r = hex32(&body["challenge"])?;
                    let c_h = random32()?;
                    let token = attester.token(&[mine.handover_nonce(&c_r)], now_s)?;
                    state.challenges.insert(from.clone(), c_h);
                    send(machine, &m.from, "challenge", json!({"challenge": format!("0x{}", hex::encode(c_h)), "binding": binding_json(&mine), "token": token})).await
                }
                "request" => {
                    let c_h = state.challenges.remove(&from).ok_or_else(|| anyhow::anyhow!("no challenge of ours outstanding"))?;
                    let theirs = binding_from(&body["binding"])?;
                    check_peer(view, ctx, &m.from, body["token"].as_str().unwrap_or(""), &theirs, &c_h, now_s).await?;
                    anyhow::ensure!(theirs.instruction == key.instruction, "asked for another instruction's key");
                    // Sealed to the key the requester's token attests, bound
                    // to our attested SubmitKey.
                    let wrapped = format!("0x{}", hex::encode(wrap(&theirs.bundle_key, LABEL_HANDOVER, &mine.submit_key, &key.secret)?));
                    let to = format!("0x{}", hex::encode(theirs.bundle_key));
                    let signature = machine.key().sign_message(reply_message(&wrapped, &to).as_bytes())?.to_string();
                    send(machine, &m.from, "reply", json!({"wrapped": wrapped, "to": to, "signature": signature})).await?;
                    handed += 1;
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        .await;
        if let Err(e) = outcome {
            let _ = send(
                machine,
                &m.from,
                "refusal",
                json!({"reason": e.to_string()}),
            )
            .await;
        }
    }
    Ok(handed)
}
