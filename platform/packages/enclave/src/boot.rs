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

/// Polls between sends of an approval not yet seen active.
const APPROVE_AGAIN_POLLS: usize = 30;

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
    // Nothing here ends on one failed read: the launch never restarts a
    // container that exits.
    for _ in 0..pace.polls {
        match bundle_key::plan(view, &holders).await {
            Ok(Plan::Mint(n)) => match bundle_key::mint(machine, n).await {
                Ok(Some(key)) => {
                    report(machine, &key, pace).await?;
                    return Ok(key);
                }
                Ok(None) => {}
                Err(e) => on_wait(&format!("mint: {e:#}")),
            },
            Ok(Plan::Fetch(n, list)) => {
                for h in &list {
                    // Ten minutes each: a holder that died is still listed,
                    // and an older one checking us through a provider that
                    // hangs takes minutes. A shorter wait throws away its
                    // late reply, every time.
                    let polls = pace.polls.min(600);
                    match bundle_key::fetch(
                        machine, view, attester, ctx, h, n, now_s, pace.pause, polls,
                    )
                    .await
                    {
                        Ok(key) => {
                            report(machine, &key, pace).await?;
                            return Ok(key);
                        }
                        Err(e) => on_wait(&format!("fetch from {} failed: {e:#}", h.address)),
                    }
                }
            }
            Ok(Plan::Wait(why)) => on_wait(&why),
            Err(e) => on_wait(&format!("the plan: {e:#}")),
        }
        (pace.pause)().await;
        match machine.call::<FleetHolders, _, _>(Empty {}).await {
            Ok(listed) => holders = listed,
            Err(e) => on_wait(&format!("the holders: {e:#}")),
        }
    }
    anyhow::bail!("no BundleKey after {} tries", pace.polls)
}

/// Tell the api, until it hears.
async fn report<T: Transport, Fut: std::future::Future<Output = ()>>(
    machine: &Machine<T>,
    key: &BundleKey,
    pace: &Pace<impl Fn() -> Fut + Copy>,
) -> anyhow::Result<()> {
    let mut tries = 0;
    while let Err(e) = bundle_key::report_holding(machine, key).await {
        tries += 1;
        anyhow::ensure!(tries < pace.polls, "never reported holding: {e:#}");
        (pace.pause)().await;
    }
    Ok(())
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
    let mut tries = 0;
    let token = loop {
        match attester.token(&[binding.nonce()], now_s) {
            Ok(token) => break token,
            Err(e) => {
                tries += 1;
                anyhow::ensure!(tries < pace.polls, "no boot token: {e:#}");
                (pace.pause)().await;
            }
        }
    };
    let mut tries = 0;
    while let Err(e) = machine
        .call::<FleetBootToken, _, _>(BootToken {
            token: token.clone(),
        })
        .await
    {
        tries += 1;
        anyhow::ensure!(
            tries < pace.polls,
            "the boot token never reached the api: {e:#}"
        );
        (pace.pause)().await;
    }
    let mut proof = None;
    for _ in 0..pace.polls {
        if let Ok(BootProof {
            journal: Some(j),
            seal: Some(s),
            ..
        }) = machine.call::<FleetBootProof, _, _>(Empty {}).await
        {
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
    // Seen once is arrived: asking again could meet a backend a block behind.
    let mut funded = false;
    for _ in 0..pace.polls {
        if matches!(view.balance(address).await, Ok(float) if !float.is_zero()) {
            funded = true;
            break;
        }
        (pace.pause)().await;
    }
    anyhow::ensure!(funded, "the float never arrived");
    // The chain says whether it landed, never the send's answer: sent again
    // until the key is active, and a send that had landed only fails to
    // estimate ("key known").
    let mut last = None;
    for i in 0..pace.polls {
        if i % APPROVE_AGAIN_POLLS == 0
            && let Err(e) = submitter.approve_key(&journal, &seal).await
        {
            last = Some(e);
        }
        if matches!(view.key_active(address).await, Ok(true)) {
            return Ok(());
        }
        (pace.pause)().await;
    }
    anyhow::bail!("the key isn't active on-chain after its approval (last send: {last:?})")
}
