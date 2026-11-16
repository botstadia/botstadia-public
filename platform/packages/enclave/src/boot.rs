//! A boot, from a fresh SubmitKey to taking work
//! (TRUST.md#bootstrap-and-handover, TRUST.md#submitkey-lifecycle):
//!
//! 1. register on a token binding the key and the chain binding;
//! 2. mint the BundleKey under an unfilled instruction, or fetch it from a
//!    holder by attested exchange;
//! 3. tell the api it holds it, and post a fresh token binding the key, the
//!    BundleKey's public half and the instruction, for the prover;
//! 4. once the proof is published and the float has arrived, send
//!    `approveKey` itself;
//! 5. ready once the chain shows the key active.

use botstadia_attestation_core::Binding;
use botstadia_shared::api::Empty;
use botstadia_shared::platform::{
    BootProof, BootToken, FleetBootProof, FleetBootToken, FleetHolders, Holder,
};

use crate::attest::Attester;
use crate::bundle_key::{self, BundleKey, Context, Plan};
use crate::chain_view::ChainView;
use crate::client::{Machine, Transport};
use crate::submit::Submitter;

/// How long to wait, and how often to look.
pub struct Pace<F> {
    pub pause: F,
    pub polls: usize,
}

/// Register a fresh key on its token.
pub async fn register<T: Transport>(
    transport: T,
    attester: &impl Attester,
    ctx: &Context,
    now_s: u64,
) -> anyhow::Result<(Machine<T>, Vec<Holder>)> {
    let key = crate::client::fresh_key()?;
    let binding = Binding {
        submit_key: key.address().0.0,
        bundle_key: [0; 32],
        chain_id: ctx.chain_id,
        results: ctx.results.0.0,
        instruction: 0,
    };
    let token = attester.token(&[binding.nonce()], now_s)?;
    let (m, registered) = Machine::register(
        transport,
        key,
        token,
        ctx.chain_id,
        &format!("{:#x}", ctx.results),
    )
    .await?;
    Ok((m, registered.holders))
}

/// Mint or fetch the BundleKey, retrying while there's nothing to do.
#[allow(clippy::too_many_arguments)]
pub async fn bundle_key<T: Transport, Fut: std::future::Future<Output = ()>>(
    machine: &Machine<T>,
    view: &impl ChainView,
    attester: &impl Attester,
    ctx: &Context,
    mut holders: Vec<Holder>,
    now_s: impl Fn() -> u64 + Copy,
    pace: &Pace<impl Fn() -> Fut + Copy>,
    on_wait: impl Fn(&str),
) -> anyhow::Result<BundleKey> {
    for _ in 0..pace.polls {
        match bundle_key::plan(view, &holders).await? {
            Plan::Mint(n) => {
                if let Some(key) = bundle_key::mint(machine, n).await? {
                    bundle_key::report_holding(machine, &key).await?;
                    return Ok(key);
                }
            }
            Plan::Fetch(n, list) => {
                for h in &list {
                    match bundle_key::fetch(
                        machine, view, attester, ctx, h, n, now_s, pace.pause, pace.polls,
                    )
                    .await
                    {
                        Ok(key) => {
                            bundle_key::report_holding(machine, &key).await?;
                            return Ok(key);
                        }
                        Err(e) => on_wait(&format!("fetch from {} failed: {e:#}", h.address)),
                    }
                }
            }
            Plan::Wait(why) => on_wait(&why),
        }
        (pace.pause)().await;
        holders = machine.call::<FleetHolders, _, _>(Empty {}).await?;
    }
    anyhow::bail!("no BundleKey after {} tries", pace.polls)
}

/// Prove and approve the key; returns once it's active on-chain.
#[allow(clippy::too_many_arguments)]
pub async fn approve<T: Transport, Fut: std::future::Future<Output = ()>>(
    machine: &Machine<T>,
    view: &impl ChainView,
    attester: &impl Attester,
    submitter: &impl Submitter,
    ctx: &Context,
    key: &BundleKey,
    now_s: u64,
    pace: &Pace<impl Fn() -> Fut + Copy>,
) -> anyhow::Result<()> {
    let address = machine.key().address();
    let binding = Binding {
        submit_key: address.0.0,
        bundle_key: key.public,
        chain_id: ctx.chain_id,
        results: ctx.results.0.0,
        instruction: key.instruction,
    };
    let token = attester.token(&[binding.nonce()], now_s)?;
    let _: Empty = machine
        .call::<FleetBootToken, _, _>(BootToken { token })
        .await?;
    let mut proof = None;
    for _ in 0..pace.polls {
        let p: BootProof = machine.call::<FleetBootProof, _, _>(Empty {}).await?;
        if let (Some(j), Some(s)) = (p.journal, p.seal) {
            proof = Some((
                hex::decode(j.trim_start_matches("0x"))?,
                hex::decode(s.trim_start_matches("0x"))?,
            ));
            break;
        }
        (pace.pause)().await;
    }
    let (journal, seal) =
        proof.ok_or_else(|| anyhow::anyhow!("the prover never published a proof"))?;
    for _ in 0..pace.polls {
        if !view.balance(address).await?.is_zero() {
            break;
        }
        (pace.pause)().await;
    }
    anyhow::ensure!(
        !view.balance(address).await?.is_zero(),
        "the float never arrived"
    );
    submitter.approve_key(&journal, &seal).await?;
    for _ in 0..pace.polls {
        if view.key_active(address).await? {
            return Ok(());
        }
        (pace.pause)().await;
    }
    anyhow::bail!("the key isn't active on-chain after its approval")
}
