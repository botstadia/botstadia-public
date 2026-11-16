//! The published test vectors (`vectors/*.json`): a field, every round the
//! standings version gives it with the outcomes played, and the standings and
//! finalists after. Anyone reimplementing the version checks against them.
//!
//! `BOTSTADIA_WRITE_VECTORS=1 cargo test -p botstadia-standings --test
//! vectors` regenerates them from the scenarios below; a plain run checks the
//! crate still gives exactly what they say.

use std::path::{Path, PathBuf};

use botstadia_standings::{Field, FieldBot, Id, Outcome, Played, Week, check_week};
use serde_json::{Value, json};

fn id(tag: u8, n: u32) -> Id {
    let mut d = [0u8; 32];
    d[0] = tag;
    d[28..].copy_from_slice(&n.to_be_bytes());
    d
}

/// A small deterministic stream for the scenarios' outcomes.
struct Stream(u64);
impl Stream {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

struct Scenario {
    name: &'static str,
    about: &'static str,
    min: usize,
    max: usize,
    /// Group of each bot (bots are rated best first).
    groups: Vec<u32>,
    rounds: usize,
    /// Per session: how it ends, given its entrants and a stream.
    outcome: fn(&[usize], &mut Stream) -> Outcome,
}

/// The better-rated (lower index) bot wins most of the time.
fn mostly_favourites(entrants: &[usize], s: &mut Stream) -> Outcome {
    let mut order: Vec<usize> = (0..entrants.len()).collect();
    order.sort_by_key(|i| entrants[*i] as u64 * 3 + s.next() % 5);
    let mut places = vec![0u8; entrants.len()];
    for (place, i) in order.iter().enumerate() {
        places[*i] = place as u8;
    }
    Outcome::Order(places)
}

fn draws_voids_and_missing(entrants: &[usize], s: &mut Stream) -> Outcome {
    match s.next() % 6 {
        0 => Outcome::Void,
        1 => Outcome::Missing,
        2 => Outcome::Order(vec![0; entrants.len()]),
        _ => mostly_favourites(entrants, s),
    }
}

/// Heats with ties: two entrants sometimes share a place.
fn heats_with_ties(entrants: &[usize], s: &mut Stream) -> Outcome {
    let Outcome::Order(places) = mostly_favourites(entrants, s) else {
        unreachable!()
    };
    if s.next().is_multiple_of(3) {
        // Tie whoever is first and second: both take place 0.
        let mut p = places.clone();
        for x in &mut p {
            if *x == 1 {
                *x = 0;
            }
        }
        return Outcome::Order(p);
    }
    Outcome::Order(places)
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "odd-1v1",
            about: "Seven bots, 1v1: a bye every round, to the lowest bot with the fewest byes.",
            min: 2,
            max: 2,
            groups: (0..7).collect(),
            rounds: 6,
            outcome: mostly_favourites,
        },
        Scenario {
            name: "exhausted-pairings",
            about: "Four bots, 1v1, five rounds: every pairing is used by round 3, then a second cycle begins.",
            min: 2,
            max: 2,
            groups: (0..4).collect(),
            rounds: 5,
            outcome: mostly_favourites,
        },
        Scenario {
            name: "same-group",
            about: "Eight bots, three of them one account group: they never share a session.",
            min: 2,
            max: 2,
            groups: vec![0, 1, 0, 2, 0, 3, 4, 5],
            rounds: 5,
            outcome: mostly_favourites,
        },
        Scenario {
            name: "group-fallback",
            about: "Four bots, three of one group: one session a round, and the two left over, who can't meet, sit out unpaired with no point.",
            min: 2,
            max: 2,
            groups: vec![0, 0, 0, 1],
            rounds: 3,
            outcome: mostly_favourites,
        },
        Scenario {
            name: "pair-the-rest",
            about: "Six bots, 1v1, two pairs of one group each: a bot takes its nearest partner only if the bots left can still all pair, so nobody sits out.",
            min: 2,
            max: 2,
            groups: vec![0, 1, 2, 3, 2, 3],
            rounds: 5,
            outcome: mostly_favourites,
        },
        Scenario {
            name: "uneven-heats",
            about: "Thirteen drones, 2 to 6 a heat: heats of 5, 4 and 4, with tied places.",
            min: 2,
            max: 6,
            groups: (0..13).collect(),
            rounds: 5,
            outcome: heats_with_ties,
        },
        Scenario {
            name: "heats-with-byes",
            about: "Five entrants, 3 to 4 a session: one session of four and a bye each round.",
            min: 3,
            max: 4,
            groups: (0..5).collect(),
            rounds: 4,
            outcome: mostly_favourites,
        },
        Scenario {
            name: "ties-voids-missing",
            about: "Ten bots, 1v1: draws, voids and sessions with no result, which score nothing.",
            min: 2,
            max: 2,
            groups: (0..10).collect(),
            rounds: 6,
            outcome: draws_voids_and_missing,
        },
        Scenario {
            name: "below-minimum",
            about: "One bot in a 1v1 arena: no rounds and no finalists.",
            min: 2,
            max: 2,
            groups: vec![0],
            rounds: 1,
            outcome: mostly_favourites,
        },
    ]
}

fn field_of(s: &Scenario) -> Field {
    let bots = s
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| FieldBot {
            digest: id(1, i as u32 + 1),
            mu: 30_000_000 - i as i64 * 700_000,
            // Uncertainties differ, so the ordinal isn't just μ.
            sigma: 2_000_000 + (i as i64 % 3) * 250_000,
            group: id(2, *g),
        })
        .collect();
    Field::new(bots, s.min, s.max).unwrap()
}

fn outcome_json(o: &Outcome) -> Value {
    match o {
        Outcome::Order(p) => json!({ "places": p }),
        Outcome::Void => json!("void"),
        Outcome::Missing => json!("missing"),
    }
}

fn outcome_of(v: &Value) -> Outcome {
    match v {
        Value::String(s) if s == "void" => Outcome::Void,
        Value::String(s) if s == "missing" => Outcome::Missing,
        v => Outcome::Order(
            v["places"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as u8)
                .collect(),
        ),
    }
}

fn indexes(v: &Value) -> Vec<usize> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as usize)
        .collect()
}

fn generate(s: &Scenario) -> Value {
    let field = field_of(s);
    let mut week = Week::new(field.clone());
    let mut stream = Stream(
        s.name
            .bytes()
            .fold(7u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)),
    );
    let mut rounds = Vec::new();
    for _ in 0..s.rounds {
        let r = week.next_round(&[]);
        if r.sessions.is_empty() {
            break;
        }
        let played: Vec<Played> = r
            .sessions
            .iter()
            .map(|e| Played {
                entrants: e.clone(),
                outcome: (s.outcome)(e, &mut stream),
            })
            .collect();
        week.record(&played, &r.byes).unwrap();
        rounds.push(json!({
            "sessions": r.sessions,
            "byes": r.byes,
            "unpaired": r.unpaired,
            "outcomes": played.iter().map(|p| outcome_json(&p.outcome)).collect::<Vec<_>>(),
        }));
    }
    json!({
        "standings_version": botstadia_standings::VERSION,
        "unit": botstadia_standings::UNIT,
        "name": s.name,
        "about": s.about,
        "min_entrants": s.min,
        "max_entrants": s.max,
        "field": field.bots().iter().map(|b| json!({
            "digest": hex::encode(b.digest),
            "mu": b.mu,
            "sigma": b.sigma,
            "group": hex::encode(b.group),
        })).collect::<Vec<_>>(),
        "rounds": rounds,
        "standings": week.standings().iter().map(|x| json!({
            "bot": x.bot,
            "points": x.points,
            "strength": x.strength,
            "sessions": x.sessions,
            "byes": x.byes,
        })).collect::<Vec<_>>(),
        "final_size": field.final_size(),
        "finalists": week.finalists(),
    })
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors")
}

fn check(v: &Value) {
    let name = v["name"].as_str().unwrap();
    let id32 = |x: &Value| -> Id {
        hex::decode(x.as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap()
    };
    let bots = v["field"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| FieldBot {
            digest: id32(&b["digest"]),
            mu: b["mu"].as_i64().unwrap(),
            sigma: b["sigma"].as_i64().unwrap(),
            group: id32(&b["group"]),
        })
        .collect();
    let field = Field::new(
        bots,
        v["min_entrants"].as_u64().unwrap() as usize,
        v["max_entrants"].as_u64().unwrap() as usize,
    )
    .unwrap();
    let mut week = Week::new(field.clone());
    let mut posted = Vec::new();
    for (i, r) in v["rounds"].as_array().unwrap().iter().enumerate() {
        let got = week.next_round(&[]);
        let sessions: Vec<Vec<usize>> = r["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(indexes)
            .collect();
        assert_eq!(got.sessions, sessions, "{name}: round {} sessions", i + 1);
        assert_eq!(
            got.byes,
            indexes(&r["byes"]),
            "{name}: round {} byes",
            i + 1
        );
        assert_eq!(
            got.unpaired,
            indexes(&r["unpaired"]),
            "{name}: round {} unpaired",
            i + 1
        );
        let played: Vec<Played> = sessions
            .iter()
            .zip(r["outcomes"].as_array().unwrap())
            .map(|(e, o)| Played {
                entrants: e.clone(),
                outcome: outcome_of(o),
            })
            .collect();
        week.record(&played, &got.byes).unwrap();
        posted.push(played);
    }
    let standings: Vec<(usize, i64, i64)> = week
        .standings()
        .iter()
        .map(|x| (x.bot, x.points, x.strength))
        .collect();
    let want: Vec<(usize, i64, i64)> = v["standings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["bot"].as_u64().unwrap() as usize,
                x["points"].as_i64().unwrap(),
                x["strength"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(standings, want, "{name}: standings");
    assert_eq!(
        week.finalists(),
        indexes(&v["finalists"]),
        "{name}: finalists"
    );
    // The check a gate runs agrees with the replay.
    if !posted.is_empty() {
        let checked = check_week(&field, &posted).unwrap();
        assert_eq!(checked.finalists(), week.finalists(), "{name}: check_week");
    }
}

#[test]
fn the_crate_gives_exactly_what_the_published_vectors_say() {
    if std::env::var("BOTSTADIA_WRITE_VECTORS").is_ok() {
        for s in scenarios() {
            let v = generate(&s);
            std::fs::write(
                dir().join(format!("{}.json", s.name)),
                serde_json::to_string_pretty(&v).unwrap() + "\n",
            )
            .unwrap();
        }
    }
    let mut checked = 0;
    for s in scenarios() {
        let text = std::fs::read_to_string(dir().join(format!("{}.json", s.name)))
            .unwrap_or_else(|_| panic!("vectors/{}.json is missing: regenerate them", s.name));
        check(&serde_json::from_str(&text).unwrap());
        checked += 1;
    }
    assert_eq!(checked, scenarios().len());
}

#[test]
fn scenarios_cover_what_the_vectors_promise() {
    let all: Vec<Value> = scenarios().iter().map(generate).collect();
    let by = |n: &str| all.iter().find(|v| v["name"] == n).unwrap();
    let rounds = |v: &Value| v["rounds"].as_array().unwrap().clone();
    assert!(
        rounds(by("odd-1v1"))
            .iter()
            .all(|r| r["byes"].as_array().unwrap().len() == 1)
    );
    assert!(
        rounds(by("group-fallback"))
            .iter()
            .any(|r| !r["unpaired"].as_array().unwrap().is_empty())
    );
    assert!(
        rounds(by("ties-voids-missing"))
            .iter()
            .any(|r| r["outcomes"].to_string().contains("void"))
    );
    assert!(
        rounds(by("ties-voids-missing"))
            .iter()
            .any(|r| r["outcomes"].to_string().contains("missing"))
    );
    assert!(
        rounds(by("heats-with-byes"))
            .iter()
            .all(|r| r["byes"].as_array().unwrap().len() == 1)
    );
    assert!(rounds(by("below-minimum")).is_empty());
    assert!(
        rounds(by("pair-the-rest"))
            .iter()
            .all(|r| r["unpaired"].as_array().unwrap().is_empty())
    );
    // Exhausted pairings: round 4 repeats a round-1..3 pairing.
    let ex = rounds(by("exhausted-pairings"));
    let early: Vec<String> = ex[..3]
        .iter()
        .flat_map(|r| {
            r["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        early
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );
    assert!(
        ex[3]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| early.contains(&s.to_string()))
    );
    // Same group never shares a session.
    let sg = by("same-group");
    for r in rounds(sg) {
        for s in r["sessions"].as_array().unwrap() {
            let groups: Vec<&str> = s
                .as_array()
                .unwrap()
                .iter()
                .map(|b| {
                    sg["field"][b.as_u64().unwrap() as usize]["group"]
                        .as_str()
                        .unwrap()
                })
                .collect();
            assert!(
                groups.len()
                    == groups
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
            );
        }
    }
    // Uneven heats: sizes 5, 4, 4.
    let mut sizes: Vec<usize> = rounds(by("uneven-heats"))[0]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_array().unwrap().len())
        .collect();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![4, 4, 5]);
}
