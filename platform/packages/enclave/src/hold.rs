//! The pre-anchor hold (TRUST.md#every-session-one-flow, step 3): the
//! enclave anchors only a session it already holds in full. From the anchor
//! on it needs nothing from the api but somewhere to upload the archive, so
//! neither a withheld bundle nor an api outage can void a bound session.
//!
//! It checks, before anchoring:
//! - the spec against the payload's hash and the post entry, entrants in
//!   strictly ascending digest order;
//! - the entry against the post's root, read from the chain itself;
//! - its week: the field entry against its post's root, the field's leaves
//!   against the entry's root, the spec under the field's arena digest and
//!   budget version, every entrant in the field, and the field's standings
//!   and rating versions this image's own;
//! - for a week's final, the final's gate (`gate.rs`);
//! - the earliest post naming the session that a first bet could prove
//!   against, from every post's list in the lookback, each hashed against
//!   its root;
//! - every entrant rated or a named seed bot by that post;
//! - the arena posted at least the arena wait (an image constant) before
//!   that post and before its week's field;
//! - the arena by its digest from the CDN;
//! - each seed bot's plaintext from the CDN, each other entrant decrypted
//!   with the BundleKey, all by digest;
//! - each entrant statically, under the session's budget version.

use anyhow::Context;
use botstadia_chain::entry::{self, Entry, FieldEntry, FieldLeaf, ROUND_FINAL};
use botstadia_chain::merkle;
use botstadia_chain::spec::{Spec, digest_string};
use botstadia_chain::{B256, keccak256};
use botstadia_shared::platform::{BundleAsk, BundleFetched, FleetBundle};
use botstadia_upload::seal::{from_b64, open_with_bundle_key};
use serde::Deserialize;

use crate::bundle_key::BundleKey;
use crate::cdn::Cdn;
use crate::chain_view::ChainView;
use crate::client::{Machine, Transport};

#[derive(Debug, Clone, Deserialize)]
pub struct EntrantRef {
    pub position: u64,
    pub bot_id: String,
    pub digest: String,
}

/// One post's full list, which the enclave hashes against the root it reads.
#[derive(Debug, Clone, Deserialize)]
pub struct PostList {
    pub number: u64,
    pub entries: Vec<String>,
}

/// A league session's signed result, from its result and anchor events, for
/// the final's gate: the enclave checks it against the chain itself.
#[derive(Debug, Clone, Deserialize)]
pub struct LeagueResult {
    pub session: String,
    pub outcome: String,
    pub seed: String,
    pub archive_hash: String,
    pub signature: String,
    pub anchor: String,
    pub anchor_block: u64,
    pub spec_hash: String,
}

/// What a session's job carries about its week (`work::week_payload`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WeekPayload {
    pub id: String,
    pub round: u32,
    pub field_entry: String,
    /// Every leaf of the field, in order.
    pub leaves: Vec<String>,
    /// For the final: every post list from the field's post on, through at
    /// least the final's and every post finalized.
    #[serde(default)]
    pub posts: Vec<PostList>,
    /// For the final: every league session's signed result.
    #[serde(default)]
    pub results: Vec<LeagueResult>,
}

/// A session's week, checked.
#[derive(Debug, Clone)]
pub struct HeldWeek {
    pub id: B256,
    pub round: u32,
    /// The post that first listed the week's field entry.
    pub field_post: u64,
    pub field_entry: Vec<u8>,
    pub field: FieldEntry,
    pub leaves: Vec<FieldLeaf>,
}

fn bytes(hex_text: &str) -> anyhow::Result<Vec<u8>> {
    Ok(hex::decode(hex_text.trim_start_matches("0x"))?)
}

/// A session job's payload, as the api couriers it (`work::offer`).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionPayload {
    pub session_id: String,
    pub spec: Spec,
    pub spec_hash: String,
    pub entry: String,
    pub post: u64,
    pub proof: Vec<String>,
    pub close: u64,
    pub arena_digest: String,
    /// The arena's slug, where the CDN keeps its files (checked by digest).
    #[serde(default)]
    pub arena_id: String,
    pub budget_version: u32,
    pub entrants: Vec<EntrantRef>,
    pub week: WeekPayload,
    /// Every post's list from the first whose block time is within the
    /// lookback of close through `post`: the enclave finds the earliest one
    /// naming the session that a first bet could prove against.
    #[serde(default)]
    pub window: Vec<PostList>,
    /// The post that announced the session's arena version.
    #[serde(default)]
    pub arena_post: Option<PostList>,
}

/// A session held in full, ready to anchor.
pub struct Held {
    pub payload: SessionPayload,
    /// Its week, checked. A warm-up's private matches have none.
    pub week: Option<HeldWeek>,
    pub entry: Vec<u8>,
    pub proof: Vec<B256>,
    pub arena: Vec<u8>,
    /// Plaintext bundle ZIPs in spec order.
    pub bundles: Vec<Vec<u8>>,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Held({})", self.payload.session_id)
    }
}

/// Why a session can't be held: a refusal names it, and the job is released
/// or reported, never anchored.
pub async fn hold<T: Transport>(
    machine: &Machine<T>,
    view: &impl ChainView,
    cdn: &impl Cdn,
    key: &BundleKey,
    payload: SessionPayload,
) -> anyhow::Result<Held> {
    let spec = &payload.spec;
    spec.check().context("its spec")?;
    anyhow::ensure!(
        format!("{:#x}", spec.hash()) == payload.spec_hash.to_lowercase(),
        "the spec doesn't match its hash"
    );
    let entry = hex::decode(payload.entry.trim_start_matches("0x"))?;
    let (week_id, round) = match Entry::decode(&entry).context("the session's entry")? {
        Entry::Session {
            spec_hash,
            digests,
            close,
            week,
            round,
        } => {
            anyhow::ensure!(
                spec_hash == spec.hash(),
                "the post entry names another spec"
            );
            anyhow::ensure!(
                digests == spec.digests,
                "the post entry names other entrants"
            );
            anyhow::ensure!(close == payload.close, "the post entry names another close");
            (week, round)
        }
        _ => anyhow::bail!("the entry isn't a session"),
    };
    anyhow::ensure!(
        format!("{:#x}", botstadia_chain::keccak256(&entry)) == payload.session_id.to_lowercase(),
        "the session id isn't its entry's hash"
    );
    let proof: Vec<B256> = payload
        .proof
        .iter()
        .map(|p| p.parse())
        .collect::<Result<_, _>>()?;
    let (root, _) = view
        .post(payload.post)
        .await
        .with_context(|| format!("its post {}", payload.post))?;
    anyhow::ensure!(
        root != B256::ZERO,
        "post {} isn't on the chain",
        payload.post
    );
    anyhow::ensure!(
        merkle::verify(&proof, root, &entry),
        "the entry isn't in post {}",
        payload.post
    );
    let (earliest, earliest_time) = earliest_post(view, &payload, &entry)
        .await
        .context("the session's earliest post")?;
    let week = hold_week(view, &payload, week_id, round)
        .await
        .context("its week")?;
    for d in &spec.digests {
        anyhow::ensure!(
            week.leaves.iter().any(|l| l.digest == *d),
            "entrant {} isn't in its week's field",
            digest_string(d)
        );
    }
    anyhow::ensure!(
        spec.arena == week.field.arena,
        "the session's arena isn't its week's"
    );
    anyhow::ensure!(
        spec.budget_version == week.field.budget_version,
        "the session's budget version isn't its week's"
    );
    for d in &spec.digests {
        anyhow::ensure!(
            view.rated_or_seed_by_post(*d, earliest).await?,
            "entrant {} was neither rated nor a seed bot by post {earliest}",
            digest_string(d)
        );
    }

    let arena_digest = digest_string(&spec.arena);
    anyhow::ensure!(
        arena_digest == payload.arena_digest,
        "the payload names another arena"
    );
    let arena_time = arena_posted(view, payload.arena_post.as_ref(), spec.arena).await?;
    let wait = crate::constants::ARENA_WAIT_S;
    anyhow::ensure!(
        arena_time + wait <= earliest_time,
        "the arena was posted less than {wait} s before post {earliest}"
    );
    let (_, field_time) = view.post(week.field_post).await?;
    anyhow::ensure!(
        arena_time + wait <= field_time,
        "the arena was posted less than {wait} s before its week's field"
    );
    let arena = cdn.arena(&payload.arena_id, &arena_digest)?;

    let mut bundles = Vec::with_capacity(spec.digests.len());
    for (i, d) in spec.digests.iter().enumerate() {
        let digest = digest_string(d);
        let entrant = payload
            .entrants
            .iter()
            .find(|e| e.position == i as u64)
            .ok_or_else(|| anyhow::anyhow!("no entrant at position {i}"))?;
        anyhow::ensure!(entrant.digest == digest, "entrant {i} isn't the spec's");
        let zip = if view.seed_bot(*d).await? {
            cdn.seed_bot(&payload.arena_id, &digest)?
        } else {
            let fetched: BundleFetched = machine
                .call::<FleetBundle, _, _>(BundleAsk {
                    bot_id: entrant.bot_id.clone(),
                })
                .await?;
            anyhow::ensure!(
                fetched.digest == digest,
                "the api couriered another bundle for entrant {i}"
            );
            anyhow::ensure!(
                fetched.instruction == key.instruction,
                "entrant {i} is wrapped to another BundleKey"
            );
            open_with_bundle_key(
                &key.secret,
                &digest,
                &from_b64(&fetched.wrapped_bundle_key)?,
                &from_b64(&fetched.ciphertext)?,
            )?
        };
        bundles.push(zip);
    }
    let held = Held {
        payload,
        week: Some(week),
        entry,
        proof,
        arena,
        bundles,
    };
    recheck(&held)?;
    if held.week.as_ref().is_some_and(|w| w.round == ROUND_FINAL) {
        let (min, max) = crate::gate::entrants(&held.arena)?;
        crate::gate::check(view, &held, min, max).await?;
    }
    Ok(held)
}

/// A post's list, checked against the root the chain holds for it: its
/// entries and its block time.
pub async fn checked_list(
    view: &impl ChainView,
    list: &PostList,
) -> anyhow::Result<(Vec<Vec<u8>>, u64)> {
    let entries: Vec<Vec<u8>> = list
        .entries
        .iter()
        .map(|e| bytes(e))
        .collect::<anyhow::Result<_>>()?;
    let (root, time) = view
        .post(list.number)
        .await
        .with_context(|| format!("post {}", list.number))?;
    anyhow::ensure!(
        root != B256::ZERO && merkle::root(&entries) == root,
        "post {}'s list isn't its root",
        list.number
    );
    Ok((entries, time))
}

/// The earliest post naming the session that a first bet could prove
/// against (TRUST.md#only-rated-bots-play-bettable-sessions): its number and
/// block time. Its block time is at least the contract's floor and at most
/// the lookback before close, and every post from the lookback's start is
/// read, so none can be left out.
pub async fn earliest_post(
    view: &impl ChainView,
    payload: &SessionPayload,
    entry: &[u8],
) -> anyhow::Result<(u64, u64)> {
    use crate::constants::{CLOSE_MIN_AFTER_POST_S, POST_LOOKBACK_S};
    let from = payload.close.saturating_sub(POST_LOOKBACK_S);
    let until = payload.close.saturating_sub(CLOSE_MIN_AFTER_POST_S);
    let first = payload
        .window
        .first()
        .map(|l| l.number)
        .ok_or_else(|| anyhow::anyhow!("no posts to find the session's earliest in"))?;
    let numbers: Vec<u64> = payload.window.iter().map(|l| l.number).collect();
    anyhow::ensure!(
        numbers == (first..=payload.post).collect::<Vec<_>>(),
        "the posts aren't every one from {first} through {}",
        payload.post
    );
    if first > 1 {
        let (_, before) = view.post(first - 1).await?;
        anyhow::ensure!(
            before < from,
            "post {} is within the lookback but wasn't given",
            first - 1
        );
    }
    for list in &payload.window {
        let (entries, time) = checked_list(view, list).await?;
        if (from..=until).contains(&time) && entries.iter().any(|e| e == entry) {
            return Ok((list.number, time));
        }
    }
    anyhow::bail!("no post a first bet could prove against names the session")
}

/// When the arena version `digest` was announced: its post's list, checked
/// against its root, holds the arena's entry.
pub async fn arena_posted(
    view: &impl ChainView,
    list: Option<&PostList>,
    digest: B256,
) -> anyhow::Result<u64> {
    let list = list.ok_or_else(|| anyhow::anyhow!("no post announcing the arena"))?;
    let (entries, time) = checked_list(view, list).await?;
    anyhow::ensure!(
        entries
            .iter()
            .any(|e| matches!(Entry::decode(e), Ok(Entry::Arena { digest: d, .. }) if d == digest)),
        "post {} doesn't announce the arena",
        list.number
    );
    Ok(time)
}

/// A session's week: its field entry the one the chain recorded for the
/// week (the first one posted), the field's leaves against the entry's root,
/// and the versions this image runs.
async fn hold_week(
    view: &impl ChainView,
    payload: &SessionPayload,
    week_id: B256,
    round: u32,
) -> anyhow::Result<HeldWeek> {
    let w = &payload.week;
    anyhow::ensure!(
        w.id.parse::<B256>()? == week_id && w.round == round,
        "the payload names another week or round"
    );
    let field_entry = bytes(&w.field_entry)?;
    let (field_hash, field_post) = view
        .week_field(week_id)
        .await
        .context("the week's field on-chain")?;
    anyhow::ensure!(
        field_hash != B256::ZERO,
        "the week's field entry isn't posted yet"
    );
    anyhow::ensure!(
        keccak256(&field_entry) == field_hash,
        "the field entry isn't the one the chain recorded for the week"
    );
    // A week's sessions run only under its field entry, so never one posted
    // before it: the final's gate reads from the field's post on.
    anyhow::ensure!(
        payload.post >= field_post,
        "the session was posted before its week's field entry"
    );
    let Entry::Field(field) = Entry::decode(&field_entry).context("the field entry")? else {
        anyhow::bail!("the week's field entry isn't one");
    };
    anyhow::ensure!(field.week == week_id, "the field entry names another week");
    anyhow::ensure!(
        field.standings_version == botstadia_standings::VERSION,
        "this image doesn't run standings version {}",
        field.standings_version
    );
    anyhow::ensure!(
        field.rating_version == botstadia_rating::VERSION,
        "this image doesn't run rating version {}",
        field.rating_version
    );
    let leaves = w
        .leaves
        .iter()
        .map(|l| FieldLeaf::decode(&bytes(l)?))
        .collect::<anyhow::Result<Vec<_>>>()
        .context("the field's leaves")?;
    anyhow::ensure!(
        entry::field_root(&leaves)? == field.field_root,
        "the field's leaves aren't its entry's root"
    );
    Ok(HeldWeek {
        id: week_id,
        round,
        field_post,
        field_entry,
        field,
        leaves,
    })
}

/// The static recheck: every bundle loads and compiles under the session's
/// budget version, against the arena's declared turn fuel.
pub fn recheck(held: &Held) -> anyhow::Result<()> {
    let budget = botstadia_host::budget::version(held.payload.budget_version).ok_or_else(|| {
        anyhow::anyhow!(
            "this image doesn't run budget version {}",
            held.payload.budget_version
        )
    })?;
    let arena = botstadia_host::LoadedArena::from_bytes(&held.arena)?;
    let turn_fuel = arena.declaration.budgets.turn_fuel;
    // In a seat's own Wasmtime process where the runner isolates.
    // One entrant's check at a time, ahead of any queue: a late recheck is a
    // late anchor.
    crate::play::with_isolation_as(
        crate::worker::Kind::Seat.memory_cap(budget),
        crate::play::Turn::Brief,
        |iso| {
            for (i, zip) in held.bundles.iter().enumerate() {
                match iso {
                    Some(iso) => crate::keyholder::check(iso, budget.id, zip, turn_fuel)
                        .map_err(|e| anyhow::anyhow!("entrant {i} doesn't pass: {e}"))?,
                    None => {
                        let bundle = botstadia_host::Bundle::from_zip(zip, budget)
                            .map_err(|e| anyhow::anyhow!("entrant {i} doesn't load: {e:#}"))?;
                        botstadia_host::seat::Program::new(bundle, budget, turn_fuel)
                            .map_err(|e| anyhow::anyhow!("entrant {i} doesn't compile: {e:#}"))?;
                    }
                }
            }
            Ok(())
        },
    )
}
