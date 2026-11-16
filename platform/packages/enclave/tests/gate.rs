//! The final's gate (TRUST.md#the-finals-gate) on a chain in memory: a week
//! played out by the standings version, then each way the enclave must refuse
//! to anchor its final.

use botstadia_chain::entry::{
    Entry, FieldEntry, FieldLeaf, ROUND_FINAL, field_root, group_id, ladder_id, week_id,
};
use botstadia_chain::outcome::{Outcome, VoidReason};
use botstadia_chain::sign::{Key, ResultMessage};
use botstadia_chain::spec::Spec;
use botstadia_chain::{Address, B256, U256, keccak256, merkle};
use botstadia_enclave::chain_view::{FakeChain, OnChainResult};
use botstadia_enclave::gate::check;
use botstadia_enclave::hold::{
    EntrantRef, Held, HeldWeek, LeagueResult, PostList, SessionPayload, WeekPayload,
};
use botstadia_standings as standings;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tamper {
    None,
    /// A league session with no accepted result on-chain.
    MissingResult,
    /// Round 2 posted with two entrants swapped between its sessions.
    Mispair,
    /// The final between the wrong bots.
    WrongFinalists,
    /// The final closing a minute before the committed close.
    BadClose,
    CalledOff,
    AlreadyBound,
    /// The api leaves a post list out.
    DropPost,
    /// A league result signed by a key the session isn't bound to.
    OtherKey,
    /// One league session voided: a void counts as a result.
    VoidOne,
    /// The final posted before the week-winner pool closed.
    EarlyFinal,
    /// A week whose field has no championship.
    NoChampionship,
    /// A finalized post after the final's, which the api leaves out.
    DropLaterPost,
    /// A post after the final's not finalized yet: left out, and that's fine.
    UnfinalizedLaterPost,
    /// The final also posted earlier, while the pool was open; held through
    /// its later post.
    FinalPostedTwice,
    /// The keeper stops a round short of the committed rounds.
    RoundShort,
}

const CHAIN: u64 = 31_337;
const ROUNDS: u32 = 3;

fn results() -> Address {
    Address::repeat_byte(5)
}

fn hexs(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn week(tamper: Tamper) -> (FakeChain, Held) {
    let view = FakeChain::genesis();
    let enclave = Key::from_bytes(&[7; 32]).unwrap();
    let ladder = ladder_id("boxing", 1);
    let week = week_id(ladder, 1_800_000_000);
    let leaves: Vec<FieldLeaf> = (1..=4u8)
        .map(|i| FieldLeaf {
            digest: B256::repeat_byte(i),
            payout: Address::repeat_byte(0xc0 + i),
            mu: 30_000_000 - i64::from(i) * 1_000_000,
            sigma: 1_000_000,
            group: group_id(week, &format!("user-{i}")),
        })
        .collect();
    let field = FieldEntry {
        week,
        ladder_id: ladder,
        arena: B256::repeat_byte(0xaa),
        budget_version: 1,
        rating_version: botstadia_rating::VERSION,
        standings_version: standings::VERSION,
        field_root: field_root(&leaves).unwrap(),
        pool_close: 1_800_400_000,
        final_close: 1_800_450_000,
        championship: tamper != Tamper::NoChampionship,
        // RoundShort: the week committed one round more than it played.
        rounds: if tamper == Tamper::RoundShort {
            ROUNDS + 1
        } else {
            ROUNDS
        },
    };
    let field_entry = Entry::Field(field.clone()).encode();
    let sfield = standings::Field::new(
        leaves
            .iter()
            .map(|l| standings::FieldBot {
                digest: l.digest.0,
                mu: l.mu,
                sigma: l.sigma,
                group: l.group.0,
            })
            .collect(),
        2,
        2,
    )
    .unwrap();
    let mut played = standings::Week::new(sfield);
    let mut posts: Vec<Vec<Vec<u8>>> = vec![vec![field_entry.clone()]];
    let mut signed = Vec::new();
    for r in 1..=ROUNDS {
        let round = played.next_round(&[]);
        let mut sessions = round.sessions.clone();
        if tamper == Tamper::Mispair && r == 2 {
            let (a, b) = (sessions[0][1], sessions[1][1]);
            sessions[0][1] = b;
            sessions[1][1] = a;
            for s in &mut sessions {
                s.sort_unstable();
            }
        }
        let mut records = Vec::new();
        let mut entries = Vec::new();
        for (j, s) in sessions.iter().enumerate() {
            let entry = Entry::Session {
                spec_hash: keccak256([r as u8, j as u8]),
                digests: s.iter().map(|i| leaves[*i].digest).collect(),
                close: 1_800_000_000 + u64::from(r) * 86_400,
                week,
                round: r,
            }
            .encode();
            let id = keccak256(&entry);
            let void = tamper == Tamper::VoidOne && r == 1 && j == 0;
            let places = vec![0u8, 1];
            let outcome = if void {
                Outcome::Void {
                    reason: VoidReason::ArenaTrap,
                }
            } else {
                Outcome::Order {
                    places: places.clone(),
                }
            };
            let archive = keccak256(id);
            let missing = tamper == Tamper::MissingResult && r == 3 && j == 1;
            if !missing {
                view.results.borrow_mut().insert(
                    id,
                    OnChainResult {
                        voided: void,
                        first_mask: if void { U256::ZERO } else { U256::from(1u8) },
                        archive_hash: if void { B256::ZERO } else { archive },
                    },
                );
            }
            view.bound.borrow_mut().insert(id, enclave.address());
            let message = ResultMessage {
                chain_id: CHAIN,
                results: results(),
                session: id,
                outcome: outcome.encode(),
                anchor: B256::repeat_byte(r as u8),
                anchor_block: u64::from(r),
                seed: B256::repeat_byte(9),
                spec_hash: keccak256([r as u8, j as u8]),
                archive_hash: archive,
            };
            let signer = if tamper == Tamper::OtherKey && r == 2 {
                Key::from_bytes(&[8; 32]).unwrap()
            } else {
                enclave.clone()
            };
            signed.push(LeagueResult {
                session: format!("{id:#x}"),
                outcome: hexs(&outcome.encode()),
                seed: format!("{:#x}", message.seed),
                archive_hash: format!("{archive:#x}"),
                signature: format!("{}", signer.sign_message(&message.encode()).unwrap()),
                anchor: format!("{:#x}", message.anchor),
                anchor_block: message.anchor_block,
                spec_hash: format!("{:#x}", message.spec_hash),
            });
            records.push(standings::Played {
                entrants: s.clone(),
                outcome: if void {
                    standings::Outcome::Void
                } else {
                    standings::Outcome::Order(places)
                },
            });
            entries.push(entry);
        }
        played.record(&records, &round.byes).unwrap();
        if r == 1 {
            posts[0].extend(entries);
        } else {
            posts.push(entries);
        }
    }
    let mut finalists = played.finalists();
    if tamper == Tamper::WrongFinalists {
        finalists = (0..4).filter(|i| !finalists.contains(i)).collect();
    }
    finalists.sort_unstable();
    let close = if tamper == Tamper::BadClose {
        field.final_close - 60
    } else {
        field.final_close
    };
    let spec = Spec {
        digests: finalists.iter().map(|i| leaves[*i].digest).collect(),
        payouts: finalists.iter().map(|i| leaves[*i].payout).collect(),
        arena: field.arena,
        chain_id: CHAIN,
        results: results(),
        nonce: B256::ZERO,
        budget_version: 1,
    };
    let final_entry = Entry::Session {
        spec_hash: spec.hash(),
        digests: spec.digests.clone(),
        close,
        week,
        round: ROUND_FINAL,
    }
    .encode();
    if tamper == Tamper::FinalPostedTwice {
        posts.last_mut().unwrap().push(final_entry.clone());
    }
    posts.push(vec![final_entry.clone()]);
    let final_post = posts.len() as u64;
    if matches!(tamper, Tamper::DropLaterPost | Tamper::UnfinalizedLaterPost) {
        posts.push(vec![B256::repeat_byte(0x99).to_vec()]);
    }
    for (i, p) in posts.iter().enumerate() {
        let number = i as u64 + 1;
        let time = if number < final_post || tamper == Tamper::EarlyFinal {
            1_800_000_000
        } else {
            field.pool_close
        };
        if tamper == Tamper::UnfinalizedLaterPost && number > final_post {
            continue;
        }
        view.posts
            .borrow_mut()
            .insert(number, (merkle::root(p), time));
    }
    if tamper == Tamper::CalledOff {
        view.weeks.borrow_mut().insert(week, (true, B256::ZERO));
    }
    if tamper == Tamper::AlreadyBound {
        view.weeks
            .borrow_mut()
            .insert(week, (false, B256::repeat_byte(1)));
    }
    let mut lists: Vec<PostList> = posts
        .iter()
        .enumerate()
        .map(|(i, p)| PostList {
            number: i as u64 + 1,
            entries: p.iter().map(|e| hexs(e)).collect(),
        })
        .collect();
    if tamper == Tamper::DropPost {
        lists.remove(1);
    }
    if tamper == Tamper::DropLaterPost {
        lists.pop();
    }
    let payload = SessionPayload {
        session_id: format!("{:#x}", keccak256(&final_entry)),
        spec_hash: format!("{:#x}", spec.hash()),
        entry: hexs(&final_entry),
        post: final_post,
        proof: vec![],
        close,
        arena_digest: botstadia_chain::spec::digest_string(&field.arena),
        arena_id: String::new(),
        budget_version: 1,
        entrants: spec
            .digests
            .iter()
            .enumerate()
            .map(|(i, d)| EntrantRef {
                position: i as u64,
                bot_id: format!("b{i}"),
                digest: botstadia_chain::spec::digest_string(d),
            })
            .collect(),
        spec,
        window: vec![],
        arena_post: None,
        week: WeekPayload {
            id: format!("{week:#x}"),
            round: ROUND_FINAL,
            field_entry: hexs(&field_entry),
            leaves: leaves.iter().map(|l| hexs(&l.encode())).collect(),
            posts: lists,
            results: signed,
        },
    };
    let held = Held {
        week: Some(HeldWeek {
            id: week,
            round: ROUND_FINAL,
            field_post: 1,
            field_entry: field_entry.clone(),
            field,
            leaves,
        }),
        payload,
        entry: final_entry,
        proof: vec![],
        arena: vec![],
        bundles: vec![],
    };
    (view, held)
}

async fn refusal(tamper: Tamper) -> String {
    let (view, held) = week(tamper);
    match check(&view, &held, 2, 2).await {
        Ok(()) => String::new(),
        Err(e) => format!("{e:#}"),
    }
}

#[tokio::test]
async fn a_week_played_by_the_standings_version_passes_the_gate() {
    assert_eq!(refusal(Tamper::None).await, "");
    assert_eq!(
        refusal(Tamper::VoidOne).await,
        "",
        "a void counts as a result"
    );
    assert_eq!(
        refusal(Tamper::UnfinalizedLaterPost).await,
        "",
        "a post the chain hasn't finalized can't be checked yet"
    );
}

#[tokio::test]
async fn the_gate_refuses_a_final_posted_before_the_pool_closed() {
    assert!(
        refusal(Tamper::EarlyFinal)
            .await
            .contains("before the week-winner pool closed")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_final_also_posted_before_the_pool_closed() {
    assert!(
        refusal(Tamper::FinalPostedTwice)
            .await
            .contains("before the week-winner pool closed (post")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_league_stopped_short_of_its_committed_rounds() {
    assert!(
        refusal(Tamper::RoundShort)
            .await
            .contains("played 3 of the 4 rounds it committed")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_week_with_no_championship() {
    assert!(
        refusal(Tamper::NoChampionship)
            .await
            .contains("no championship")
    );
}

#[tokio::test]
async fn the_gate_refuses_when_a_finalized_later_post_is_left_out() {
    assert!(
        refusal(Tamper::DropLaterPost)
            .await
            .contains("aren't every post")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_league_session_with_no_accepted_result() {
    assert!(
        refusal(Tamper::MissingResult)
            .await
            .contains("no accepted result")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_round_off_the_standings_version() {
    assert!(
        refusal(Tamper::Mispair)
            .await
            .contains("round 2 isn't the standings version's pairing")
    );
}

#[tokio::test]
async fn the_gate_refuses_finalists_that_aren_t_the_standings_version_s() {
    assert!(refusal(Tamper::WrongFinalists).await.contains("finalists"));
}

#[tokio::test]
async fn the_gate_refuses_a_close_that_isn_t_the_committed_one() {
    assert!(
        refusal(Tamper::BadClose)
            .await
            .contains("committed final close")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_called_off_week_and_a_week_already_bound() {
    assert!(refusal(Tamper::CalledOff).await.contains("called off"));
    assert!(
        refusal(Tamper::AlreadyBound)
            .await
            .contains("already has a final")
    );
}

#[tokio::test]
async fn the_gate_refuses_when_a_post_list_is_left_out() {
    assert!(
        refusal(Tamper::DropPost)
            .await
            .contains("aren't every post")
    );
}

#[tokio::test]
async fn the_gate_refuses_a_result_not_signed_by_its_bound_key() {
    assert!(refusal(Tamper::OtherKey).await.contains("bound key"));
}

#[tokio::test]
async fn the_gate_refuses_a_post_list_that_isn_t_its_root() {
    let (view, mut held) = week(Tamper::None);
    held.payload.week.posts[1].entries.pop();
    let e = check(&view, &held, 2, 2).await.unwrap_err();
    assert!(format!("{e:#}").contains("isn't its root"));
}
