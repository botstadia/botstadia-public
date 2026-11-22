//! Warm-up jobs (TRUST.md#warm-ups): a new bot's first rating, from private
//! matches against the opponent list the api gave once at the start. Only
//! the final rating leaves the enclave; every match stays in memory.
//!
//! - The enclave checks for itself, at a finalized block: the new bot has no
//!   final rating and isn't a seed bot; each opponent is rated or a seed bot;
//!   the list holds at least the lesser of the minimum and the ladder's
//!   count; it can seat a match.
//! - It fetches every opponent at the start and rechecks each; one that
//!   can't be fetched, opened or rechecked is left out of the whole job.
//! - Each match's seed is hashed from the job's own randomness and the
//!   match number; opponents are drawn at random, whatever the results.
//! - A voided match is replaced, up to the maximum attempts. The job ends
//!   once the uncertainty falls below the threshold after the minimum
//!   matches, or at the maximum. Only the new bot's rating moves.

use botstadia_chain::abi::Results;
use botstadia_chain::entry::ladder_id;
use botstadia_chain::outcome::Outcome;
use botstadia_chain::sign::RatingMessage;
use botstadia_chain::spec::{Spec, digest_bytes};
use botstadia_chain::{Address, B256, Bytes, keccak256};
use botstadia_rating::{PRECISION, Rating, rate};
use serde::Deserialize;
use serde_json::json;

use crate::bundle_key::BundleKey;
use crate::cdn::Cdn;
use crate::chain_view::ChainView;
use crate::client::{Machine, Transport};
use crate::hold::{Held, SessionPayload};
use crate::jobs::BotPayload;
use crate::play::{self, Limits};

/// Constants of the image (TRUST.md#constants-of-the-image).
#[derive(Debug, Clone, Copy)]
pub struct Constants {
    pub min_matches: u32,
    pub max_matches: u32,
    /// The uncertainty (σ, unscaled) that ends a job early.
    pub sigma_threshold: f64,
    pub per_opponent_cap: u32,
    pub min_opponents: usize,
    pub max_attempts: u32,
}

pub const CONSTANTS: Constants = Constants {
    min_matches: 10,
    max_matches: 50,
    sigma_threshold: 3.5,
    per_opponent_cap: 8,
    min_opponents: 3,
    max_attempts: 100,
};

/// The constants this build runs. A local (mock) build may shorten the
/// match counts for the end-to-end run (`BOTSTADIA_WARMUP_MATCHES`); an
/// image can't.
pub fn constants() -> Constants {
    #[cfg(feature = "mock")]
    if let Some(n) = std::env::var("BOTSTADIA_WARMUP_MATCHES")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Constants {
            min_matches: n,
            max_matches: n,
            ..CONSTANTS
        };
    }
    CONSTANTS
}

#[derive(Debug, Clone, Deserialize)]
pub struct Opponent {
    pub bot_id: String,
    pub digest: String,
    #[serde(default)]
    pub seed_bot: bool,
    pub mu: i64,
    pub sigma: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WarmupPayload {
    pub bot_id: String,
    pub digest: String,
    pub arena_id: String,
    pub payload_schema: u32,
    pub arena_digest: Option<String>,
    pub opponents: Vec<Opponent>,
    pub opponent_list_hash: String,
    /// The post that announced the arena version.
    #[serde(default)]
    pub arena_post: Option<crate::hold::PostList>,
}

/// A job held in full: the new bot and every opponent that passed.
pub struct Prepared {
    pub payload: WarmupPayload,
    pub arena: Vec<u8>,
    pub budget_version: u32,
    pub bot: Vec<u8>,
    pub opponents: Vec<(Opponent, Vec<u8>)>,
    pub seats: (usize, usize),
}

/// Fetch, open and check everything before any match.
pub async fn prepare<T: Transport>(
    machine: &Machine<T>,
    view: &impl ChainView,
    cdn: &impl Cdn,
    key: &BundleKey,
    payload: WarmupPayload,
    c: &Constants,
) -> anyhow::Result<Result<Prepared, String>> {
    let Some(arena_digest) = payload.arena_digest.clone() else {
        return Ok(Err("no posted arena version".into()));
    };
    if view.rated_or_seed(digest_bytes(&payload.digest)?).await? {
        return Ok(Err("the bot already has a rating or is a seed bot".into()));
    }
    // A warm-up uses only an arena version posted at least the arena wait
    // ago (TRUST.md#approving-builds), by the finalized chain's time.
    let posted = crate::hold::arena_posted(
        view,
        payload.arena_post.as_ref(),
        digest_bytes(&arena_digest)?,
    )
    .await;
    let (_, now) = view.head_times().await?;
    match posted {
        Ok(t) if t + crate::constants::ARENA_WAIT_S <= now => {}
        Ok(_) => return Ok(Err("the arena version was posted too recently".into())),
        // No verdict on the bot: given back, and tried again.
        Err(e) => return Err(e.context("the arena version's post")),
    }
    let arena = cdn.arena(&payload.arena_id, &arena_digest)?;
    let budget = botstadia_host::budget::current();
    let loaded = botstadia_host::LoadedArena::from_bytes(&arena)?;
    let turn_fuel = loaded.declaration.budgets.turn_fuel;
    let entrants = loaded.declaration.entrants;
    // Unzipped and compiled in a seat's own Wasmtime process where the
    // runner isolates, as the anchor-time recheck is: the runner holds the
    // keys and parses nothing a bundle controls (BOTS.md, "Nothing a bot can
    // reach holds a key").
    let compiles = |zip: &[u8]| {
        crate::play::with_isolation(
            crate::worker::Kind::Seat.memory_cap(budget),
            |iso| match iso {
                Some(iso) => crate::keyholder::check(iso, budget.id, zip, turn_fuel).is_ok(),
                None => botstadia_host::Bundle::from_zip(zip, budget)
                    .and_then(|b| botstadia_host::seat::Program::new(b, budget, turn_fuel))
                    .is_ok(),
            },
        )
    };
    let fetch = |digest: &str, bot_id: &str| BotPayload {
        bot_id: bot_id.into(),
        digest: digest.into(),
        arena_id: payload.arena_id.clone(),
        arena_digest: None,
    };
    let bot = match crate::jobs::plaintext(
        machine,
        view,
        cdn,
        key,
        &fetch(&payload.digest, &payload.bot_id),
    )
    .await?
    {
        Ok(z) if compiles(&z) => z,
        Ok(_) => return Ok(Err("the bot doesn't compile".into())),
        Err(crate::jobs::NoPlaintext::Bundle(why) | crate::jobs::NoPlaintext::Ours(why)) => {
            return Ok(Err(why));
        }
    };
    let mut opponents = Vec::new();
    for o in &payload.opponents {
        if o.digest == payload.digest || !view.rated_or_seed(digest_bytes(&o.digest)?).await? {
            continue;
        }
        if let Ok(Ok(zip)) =
            crate::jobs::plaintext(machine, view, cdn, key, &fetch(&o.digest, &o.bot_id)).await
            && compiles(&zip)
        {
            opponents.push((o.clone(), zip));
        }
    }
    let ladder = ladder_id(&payload.arena_id, payload.payload_schema);
    let needed = (c.min_opponents as u64).min(view.ladder_count(ladder).await?) as usize;
    if opponents.len() < needed.max(1) {
        return Ok(Err(format!(
            "{} usable opponents, fewer than {needed}",
            opponents.len()
        )));
    }
    let seats_min = (entrants.min() as usize).max(2);
    if opponents.len() + 1 < seats_min {
        return Ok(Err("the list can't seat a match".into()));
    }
    let seats_max = (entrants.max() as usize).min(opponents.len() + 1);
    Ok(Ok(Prepared {
        payload,
        arena,
        budget_version: budget.id,
        bot,
        opponents,
        seats: (seats_min, seats_max),
    }))
}

fn rating(mu: i64, sigma: i64) -> Rating {
    Rating {
        mu: mu as f64 / PRECISION,
        sigma: sigma as f64 / PRECISION,
    }
}

/// The final rating, if the job ends in one.
#[derive(Debug, Clone, PartialEq)]
pub struct Final {
    pub mu: i64,
    pub sigma: i64,
    pub matches: u32,
}

/// The cap on matches per opponent for a list of `listed` opponents, each
/// match drawing `per_match` of them (TRUST.md#constants-of-the-image). A
/// list below the minimum size, only ever from a small ladder, raises it
/// just enough that the maximum can be played; a list at the minimum size
/// or above never does.
pub fn per_opponent_cap(c: &Constants, listed: usize, per_match: usize) -> u32 {
    if listed == 0 || listed >= c.min_opponents {
        return c.per_opponent_cap;
    }
    let needed = (c.max_matches as usize * per_match).div_ceil(listed);
    c.per_opponent_cap.max(needed as u32)
}

/// Play the job's matches: the final rating, if it ends in one, and how
/// many matches had a seat's process crash or a syscall refused (a possible
/// seat escape, reported once the job has ended, naming only the machine:
/// TRUST.md#how-the-enclave-talks-to-the-chain).
pub fn run(
    p: &Prepared,
    c: &Constants,
    limits: &Limits,
    job_seed: [u8; 32],
) -> (Option<Final>, u32) {
    let mut incidents = 0u32;
    let mut me = Rating::default();
    let mut played = vec![0u32; p.opponents.len()];
    let (mut matches, mut attempts) = (0u32, 0u32);
    let per_match = p.seats.0.max(2).min(p.opponents.len() + 1) - 1;
    let cap = per_opponent_cap(c, p.opponents.len(), per_match);
    while matches < c.max_matches {
        if attempts >= c.max_attempts {
            return (None, incidents);
        }
        let seed = keccak256([job_seed.as_slice(), &attempts.to_be_bytes()].concat()).0;
        attempts += 1;
        // Draw the field at random from those under the cap.
        let open: Vec<usize> = (0..p.opponents.len())
            .filter(|&i| played[i] < cap)
            .collect();
        if open.is_empty() {
            break;
        }
        let size = p.seats.0.max(2).min(open.len() + 1);
        let order = play::seats(
            &keccak256([seed.as_slice(), b"draw"].concat()).0,
            open.len(),
        );
        let drawn: Vec<usize> = order.iter().take(size - 1).map(|&k| open[k]).collect();
        // Entrants in ascending digest order, as in a session.
        let mut field: Vec<(String, Vec<u8>, Option<usize>)> =
            vec![(p.payload.digest.clone(), p.bot.clone(), None)];
        for &i in &drawn {
            field.push((
                p.opponents[i].0.digest.clone(),
                p.opponents[i].1.clone(),
                Some(i),
            ));
        }
        field.sort_by(|a, b| a.0.cmp(&b.0));
        let held = Held {
            week: None,
            payload: SessionPayload {
                session_id: String::new(),
                spec: Spec {
                    digests: vec![],
                    payouts: vec![],
                    arena: B256::ZERO,
                    chain_id: 0,
                    results: Address::ZERO,
                    nonce: B256::ZERO,
                    budget_version: p.budget_version,
                },
                spec_hash: String::new(),
                entry: String::new(),
                post: 0,
                proof: vec![],
                close: 0,
                arena_digest: String::new(),
                arena_id: String::new(),
                budget_version: p.budget_version,
                entrants: vec![],
                week: Default::default(),
                window: vec![],
                arena_post: None,
            },
            entry: vec![],
            proof: vec![],
            arena: p.arena.clone(),
            bundles: field.iter().map(|f| f.1.clone()).collect(),
        };
        let played_match = play::play_job(&held, &seed, limits);
        if played_match
            .incidents
            .iter()
            .any(|i| i.contains("SIGSYS") || i.contains("signal"))
        {
            incidents += 1;
        }
        let Outcome::Order { places } = played_match.outcome else {
            continue; // a voided match is replaced
        };
        let ratings: Vec<Rating> = field
            .iter()
            .map(|f| match f.2 {
                None => me,
                Some(i) => rating(p.opponents[i].0.mu, p.opponents[i].0.sigma),
            })
            .collect();
        let mut groups: Vec<(u8, Vec<usize>)> = Vec::new();
        for (pos, place) in places.iter().enumerate() {
            match groups.iter_mut().find(|(pl, _)| pl == place) {
                Some((_, g)) => g.push(pos),
                None => groups.push((*place, vec![pos])),
            }
        }
        groups.sort_by_key(|(pl, _)| *pl);
        let after = rate(
            &ratings,
            &groups.into_iter().map(|(_, g)| g).collect::<Vec<_>>(),
        );
        let mine = field
            .iter()
            .position(|f| f.2.is_none())
            .expect("the new bot plays every match");
        me = after[mine];
        for &i in &drawn {
            played[i] += 1;
        }
        matches += 1;
        if matches >= c.min_matches && me.sigma < c.sigma_threshold {
            break;
        }
    }
    let f = (matches >= c.min_matches).then(|| Final {
        mu: (me.mu * PRECISION).round() as i64,
        sigma: (me.sigma * PRECISION).round() as i64,
        matches,
    });
    (f, incidents)
}

/// The signed final rating, for the result batch.
pub fn rating_item(
    key: &botstadia_chain::sign::Key,
    chain_id: u64,
    results: Address,
    p: &Prepared,
    f: &Final,
) -> anyhow::Result<Results::RatingItem> {
    let message = RatingMessage {
        chain_id,
        results,
        bot: digest_bytes(&p.payload.digest)?,
        ladder: ladder_id(&p.payload.arena_id, p.payload.payload_schema),
        mu: f.mu,
        sigma: f.sigma,
        rating_version: botstadia_rating::VERSION,
        arena_digest: digest_bytes(p.payload.arena_digest.as_deref().unwrap_or(""))?,
        budget_version: p.budget_version,
        opponent_list_hash: p.payload.opponent_list_hash.parse()?,
    };
    let signature = key.sign_message(&message.encode())?;
    Ok(Results::RatingItem {
        bot: message.bot,
        ladder: message.ladder,
        mu: message.mu,
        sigma: message.sigma,
        ratingVersion: message.rating_version,
        arenaDigest: message.arena_digest,
        budgetVersion: message.budget_version,
        opponentListHash: message.opponent_list_hash,
        signature: Bytes::from(signature.to_vec()),
    })
}

/// What a finished warm-up tells the api: nothing of its matches, only how
/// many had a seat's process crash or a syscall refused.
pub fn finished(incidents: u32) -> serde_json::Value {
    json!({ "seat_incidents": incidents })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_ladders_list_raises_the_cap_just_enough_for_the_maximum() {
        let c = CONSTANTS;
        // At the minimum size or above, never raised.
        assert_eq!(per_opponent_cap(&c, c.min_opponents, 1), c.per_opponent_cap);
        assert_eq!(per_opponent_cap(&c, 7, 1), c.per_opponent_cap);
        // One opponent: every match is against it.
        assert_eq!(per_opponent_cap(&c, 1, 1), c.max_matches);
        // Two, one a match: half each, rounded up.
        assert_eq!(per_opponent_cap(&c, 2, 1), c.max_matches.div_ceil(2));
        // Two, both in every match (a three-seat race).
        assert_eq!(per_opponent_cap(&c, 2, 2), c.max_matches);
        for (listed, per_match) in [(1, 1), (2, 1), (2, 2)] {
            let cap = per_opponent_cap(&c, listed, per_match) as usize;
            assert!(listed * cap / per_match >= c.max_matches as usize);
        }
    }
}
