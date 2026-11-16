//! The final's gate (TRUST.md#the-finals-gate): before anchoring a week's
//! final, the enclave checks the whole week itself, so nothing but the
//! published standings version can choose its finalists, and every lever
//! over a week buys at most a refund.
//!
//! It anchors the final only if:
//!
//! - the week has a championship, isn't called off and has no final bound
//!   yet, the final closes at the field entry's committed final close, and
//!   its post came after the week-winner pool closed;
//! - every post from the field's through the final's and every finalized
//!   one is in the post lists the api couriered, each hashing to the root
//!   the enclave reads itself, so no league session can be left out;
//! - every league session in them has an accepted result on-chain (a void
//!   counts). Its full finishing order comes from its signed result, which
//!   must be signed by the key the session is bound to and agree with the
//!   first places and archive hash the contract stores;
//! - every round's sessions are exactly the standings version's pairing
//!   from the field and the outcomes before it;
//! - the league played exactly the rounds the field entry committed, or
//!   ended early only because its next round could hold no session;
//! - the final's entrants are exactly the standings version's finalists.
//!
//! Every chain read is a getter at a finalized block under the two-provider
//! rule (`chain_view`). No current ratings or chain history are needed.

use std::collections::{BTreeMap, BTreeSet};

use botstadia_chain::entry::{Entry, ROUND_FINAL};
use botstadia_chain::outcome::Outcome;
use botstadia_chain::sign::{ResultMessage, recover};
use botstadia_chain::{B256, U256, keccak256, merkle};
use botstadia_standings as standings;

use crate::chain_view::ChainView;
use crate::hold::Held;

fn bytes(hex_text: &str) -> anyhow::Result<Vec<u8>> {
    Ok(hex::decode(hex_text.trim_start_matches("0x"))?)
}

/// The arena's entrant range, from its declaration.
pub fn entrants(arena: &[u8]) -> anyhow::Result<(usize, usize)> {
    use botstadia_interface::declaration::Entrants;
    let arena = botstadia_host::LoadedArena::from_bytes(arena)?;
    Ok(match arena.declaration.entrants {
        Entrants::Exact(n) => (n as usize, n as usize),
        Entrants::Range { min, max } => (min as usize, max as usize),
    })
    .map(|(min, max)| (min.max(2), max.max(2)))
}

/// Check a held final against its whole week, for an arena of `min..=max`
/// entrants a session. An error names why the final must not be anchored.
pub async fn check(
    view: &impl ChainView,
    held: &Held,
    min: usize,
    max: usize,
) -> anyhow::Result<()> {
    let w = held
        .week
        .as_ref()
        .filter(|w| w.round == ROUND_FINAL)
        .ok_or_else(|| anyhow::anyhow!("not a final"))?;
    anyhow::ensure!(w.field.championship, "the week has no championship");
    let (called_off, bound) = view.week_state(w.id).await?;
    anyhow::ensure!(!called_off, "the week is called off");
    anyhow::ensure!(bound == B256::ZERO, "the week already has a final");
    anyhow::ensure!(
        held.payload.close == w.field.final_close,
        "the final's close isn't the week's committed final close"
    );

    let (_, final_posted) = view.post(held.payload.post).await?;
    anyhow::ensure!(
        final_posted >= w.field.pool_close,
        "the final was posted before the week-winner pool closed"
    );

    // Every post from the field's on, each against its root. Lists past the
    // finalized count can't be checked yet and are left out.
    let through = held.payload.post.max(view.post_count().await?);
    let lists: Vec<&crate::hold::PostList> = held
        .payload
        .week
        .posts
        .iter()
        .take_while(|p| p.number <= through)
        .collect();
    let numbers: Vec<u64> = lists.iter().map(|p| p.number).collect();
    let want: Vec<u64> = (w.field_post..=through).collect();
    anyhow::ensure!(
        numbers == want,
        "the post lists aren't every post from the field's ({}) through {through}",
        w.field_post,
    );
    // The league sessions: id → (round, entrant digests).
    let mut league: BTreeMap<B256, (u32, Vec<B256>)> = BTreeMap::new();
    for list in &lists {
        let entries: Vec<Vec<u8>> = list
            .entries
            .iter()
            .map(|e| bytes(e))
            .collect::<anyhow::Result<_>>()?;
        let (root, time) = view.post(list.number).await?;
        anyhow::ensure!(
            root != B256::ZERO && merkle::root(&entries) == root,
            "post {}'s list isn't its root",
            list.number
        );
        // The final first posted while the pool was open, then again later,
        // is still one posted early: its finalists could have been known.
        anyhow::ensure!(
            time >= w.field.pool_close || !entries.contains(&held.entry),
            "the final was posted before the week-winner pool closed (post {})",
            list.number
        );
        for e in &entries {
            if let Ok(Entry::Session {
                digests,
                week,
                round,
                ..
            }) = Entry::decode(e)
                && week == w.id
                && round != ROUND_FINAL
            {
                // An id posted again is the same session.
                league.entry(keccak256(e)).or_insert((round, digests));
            }
        }
    }
    let rounds_seen: BTreeSet<u32> = league.values().map(|(r, _)| *r).collect();
    let count = rounds_seen.len() as u32;
    anyhow::ensure!(count > 0, "the week has no league rounds");
    anyhow::ensure!(
        rounds_seen.iter().copied().eq(1..=count),
        "the week's rounds aren't 1 to {count}"
    );

    // Each league session's accepted result, its order from its signed result.
    let signed: BTreeMap<B256, &crate::hold::LeagueResult> = held
        .payload
        .week
        .results
        .iter()
        .filter_map(|r| Some((r.session.parse().ok()?, r)))
        .collect();
    let field = standings::Field::new(
        w.leaves
            .iter()
            .map(|l| standings::FieldBot {
                digest: l.digest.0,
                mu: l.mu,
                sigma: l.sigma,
                group: l.group.0,
            })
            .collect(),
        min,
        max,
    )
    .map_err(|e| anyhow::anyhow!("the field: {e}"))?;
    let mut by_round: Vec<Vec<standings::Played>> = vec![Vec::new(); count as usize];
    for (id, (round, digests)) in &league {
        let on_chain = view
            .result(*id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("league session {id:#x} has no accepted result"))?;
        let outcome = if on_chain.voided {
            standings::Outcome::Void
        } else {
            let r = signed
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("no signed result for league session {id:#x}"))?;
            let outcome = bytes(&r.outcome)?;
            let Outcome::Order { places } = Outcome::decode(&outcome)? else {
                anyhow::bail!("league session {id:#x}'s signed result isn't its accepted one");
            };
            let mask = places
                .iter()
                .enumerate()
                .filter(|(_, p)| **p == 0)
                .fold(U256::ZERO, |m, (i, _)| m | (U256::from(1u8) << i));
            let archive: B256 = r.archive_hash.parse()?;
            anyhow::ensure!(
                mask == on_chain.first_mask && archive == on_chain.archive_hash,
                "league session {id:#x}'s signed result isn't its accepted one"
            );
            let message = ResultMessage {
                chain_id: held.payload.spec.chain_id,
                results: held.payload.spec.results,
                session: *id,
                outcome,
                anchor: r.anchor.parse()?,
                anchor_block: r.anchor_block,
                seed: r.seed.parse()?,
                spec_hash: r.spec_hash.parse()?,
                archive_hash: archive,
            };
            let signer = recover(&message.encode(), &bytes(&r.signature)?)?;
            anyhow::ensure!(
                signer == view.bound_key(*id).await?,
                "league session {id:#x}'s result isn't signed by its bound key"
            );
            standings::Outcome::Order(places)
        };
        let mut entrants = Vec::with_capacity(digests.len());
        for d in digests {
            entrants.push(field.index_of(&d.0).ok_or_else(|| {
                anyhow::anyhow!("league session {id:#x} seats a bot outside the field")
            })?);
        }
        by_round[*round as usize - 1].push(standings::Played { entrants, outcome });
    }

    let week = standings::check_week(&field, &by_round).map_err(|f| match f {
        standings::Fault::Mispaired { round } => {
            anyhow::anyhow!("round {round} isn't the standings version's pairing")
        }
        other => anyhow::anyhow!("the week is off the standings version: {other:?}"),
    })?;
    // Exactly the rounds the field entry committed, so nobody can end the
    // league early to freeze the standings; fewer only if the next round
    // could hold no session, which ends any league.
    anyhow::ensure!(
        count <= w.field.rounds,
        "the week played {count} rounds, more than the {} it committed",
        w.field.rounds
    );
    anyhow::ensure!(
        count == w.field.rounds || week.next_round(&[]).sessions.is_empty(),
        "the week played {count} of the {} rounds it committed",
        w.field.rounds
    );
    let mut finalists = week.finalists();
    finalists.sort_unstable();
    let mut seated: Vec<usize> = held
        .payload
        .spec
        .digests
        .iter()
        .filter_map(|d| field.index_of(&d.0))
        .collect();
    seated.sort_unstable();
    anyhow::ensure!(
        !finalists.is_empty()
            && seated == finalists
            && seated.len() == held.payload.spec.digests.len(),
        "the final's entrants aren't the standings version's finalists"
    );
    Ok(())
}
