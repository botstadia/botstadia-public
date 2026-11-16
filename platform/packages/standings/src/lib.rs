//! BotStadia's standings version (COMPETITION.md#pairing-and-points): how a
//! week pairs its rounds, scores them, breaks ties and picks its finalists.
//!
//! One implementation, used by the keeper to post rounds, by the enclave's
//! final's gate (TRUST.md#the-finals-gate) and by anyone checking us, so the
//! same field and the same outcomes give the same week everywhere. It has no
//! dependencies, does no I/O and uses integers only: points are counted in
//! [`UNIT`]ths of a point, which every session size up to [`MAX_ENTRANTS`]
//! divides exactly.
//!
//! Version 1, in full:
//!
//! - **Order.** Bots are ranked by points, then strength of opponents beaten
//!   (the sum of the current points of every opponent a bot finished strictly
//!   ahead of, once per time), then rating at the freeze (`μ − 3σ` at the
//!   fixed precision), then bundle digest, ascending. In round 1 everyone has
//!   no points, so that's rating order.
//! - **Byes.** The fewest that let the rest split into sessions within the
//!   arena's minimum and maximum entrants (one in an odd 1v1 field). Each goes
//!   to the bot with the fewest byes so far that stands lowest. A bye scores
//!   one point.
//! - **Sessions.** As few as the maximum allows, filled as evenly as possible
//!   (sizes differ by at most one, larger first). Round 1 deals the ranked
//!   bots across them (position `p` to session `p mod k`), which for a 1v1
//!   arena is top half against bottom half. Later rounds take consecutive
//!   blocks of the ranking. Sessions are filled one by one: each takes the
//!   next bot in that sequence that shares an account group with nobody in it.
//!   In a 1v1 arena each bot in turn takes its partner instead, from the
//!   same sequence: after round 1 the eligible bot it has met least often
//!   this week, nearest first, so repeats wait until every pairing is used
//!   and a second cycle then begins by itself. A partner is taken only if
//!   the bots still left can pair as fully as any choice allows (all but the
//!   excess of their largest group over the rest), so a 1v1 round leaves
//!   unpaired only the bots the group rule must.
//! - **The fallback.** In an arena of more than two entrants, bots left over
//!   (no session could take them under the group rule) join any session with
//!   room that takes them, earliest first; a session still below the minimum
//!   is dissolved. A bot with no session
//!   after that is **unpaired**: it sits the round out and scores nothing, so
//!   the group rule never hands out a free point.
//! - **No sessions, no round.** A round that would hold no session (a field
//!   below the minimum, or one the group rule leaves unplayable) isn't
//!   posted, and the week runs no more rounds.
//! - **Points.** `(n − place) / (n − 1)` with `place` counted from 1, tied
//!   places sharing their average: a 1v1 win 1, draw ½, loss 0. A void or a
//!   session with no result scores 0 for every entrant.
//! - **Finalists.** The top finisher of each account group, in standings
//!   order, up to the final's size, `clamp(field / 2, min, max)`. Fewer than
//!   the arena's minimum means no final.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

/// The standings version this crate implements.
pub const VERSION: u32 = 1;
/// One point, in the units points are kept in: divisible by `2(n − 1)` for
/// every session size `n` up to [`MAX_ENTRANTS`].
pub const UNIT: i64 = 840;
/// The most entrants a session may have under this version.
pub const MAX_ENTRANTS: usize = 8;

/// A bundle digest or an account group id.
pub type Id = [u8; 32];

/// One bot in a week's field, as its field entry's leaf commits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBot {
    pub digest: Id,
    /// Rating at the freeze, at the fixed precision (× 1e6).
    pub mu: i64,
    pub sigma: i64,
    /// The pseudonymous account group.
    pub group: Id,
}

impl FieldBot {
    /// The rating ordinal the order uses: `μ − 3σ`, exact.
    pub fn ordinal(&self) -> i64 {
        self.mu - 3 * self.sigma
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotAscending,
    Empty,
    Entrants {
        min: usize,
        max: usize,
    },
    /// A session's entrants aren't distinct field bots.
    BadSession,
    /// A finishing order that isn't one place per entrant, ties sharing the
    /// lowest place they cover.
    BadOrder,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAscending => write!(f, "the field isn't in strictly ascending digest order"),
            Self::Empty => write!(f, "an empty field"),
            Self::Entrants { min, max } => write!(
                f,
                "{min}..{max} entrants is outside this version's 2..{MAX_ENTRANTS}"
            ),
            Self::BadSession => write!(f, "a session's entrants aren't distinct field bots"),
            Self::BadOrder => write!(f, "a malformed finishing order"),
        }
    }
}

impl std::error::Error for Error {}

/// A week's field: its bots in strictly ascending digest order, and the
/// arena's entrant range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    bots: Vec<FieldBot>,
    min: usize,
    max: usize,
}

impl Field {
    pub fn new(bots: Vec<FieldBot>, min: usize, max: usize) -> Result<Self, Error> {
        if bots.is_empty() {
            return Err(Error::Empty);
        }
        if min < 2 || max < min || max > MAX_ENTRANTS {
            return Err(Error::Entrants { min, max });
        }
        if !bots.windows(2).all(|w| w[0].digest < w[1].digest) {
            return Err(Error::NotAscending);
        }
        Ok(Self { bots, min, max })
    }

    pub fn bots(&self) -> &[FieldBot] {
        &self.bots
    }

    pub fn len(&self) -> usize {
        self.bots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bots.is_empty()
    }

    pub fn min_entrants(&self) -> usize {
        self.min
    }

    pub fn max_entrants(&self) -> usize {
        self.max
    }

    /// A bot's index by digest.
    pub fn index_of(&self, digest: &Id) -> Option<usize> {
        self.bots.binary_search_by(|b| b.digest.cmp(digest)).ok()
    }

    /// The final's size: half the field rounded down, within the arena's
    /// minimum and maximum entrants.
    pub fn final_size(&self) -> usize {
        final_size(self.len(), self.min, self.max)
    }

    /// Whether the week has a championship (a final and a week-winner pool):
    /// a field of at least twice the arena's minimum, plus any raise.
    pub fn championship(&self, raise: usize) -> bool {
        championship(self.len(), self.min, raise)
    }
}

pub fn final_size(field: usize, min: usize, max: usize) -> usize {
    (field / 2).clamp(min, max)
}

pub fn championship(field: usize, min: usize, raise: usize) -> bool {
    field >= 2 * min + raise
}

/// How one session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Each entrant's place, 0 first, ties sharing the lowest place they
    /// cover (`[0, 0, 2]`), in the session's entrant order.
    Order(Vec<u8>),
    /// Our system failed: nobody scores.
    Void,
    /// No result (cancelled, past its deadline, its key revoked): nobody
    /// scores.
    Missing,
}

/// One session as played: its entrants (field indexes, ascending) and how
/// it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    pub entrants: Vec<usize>,
    pub outcome: Outcome,
}

/// One round as the standings version gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Round {
    /// Each session's entrants, field indexes ascending; sessions ordered by
    /// their lowest entrant.
    pub sessions: Vec<Vec<usize>>,
    /// Bots sitting out with a point.
    pub byes: Vec<usize>,
    /// Bots no session could take under the group rule: no point.
    pub unpaired: Vec<usize>,
}

/// One bot's place in the standings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub bot: usize,
    /// In [`UNIT`]ths of a point.
    pub points: i64,
    /// Strength of opponents beaten, in [`UNIT`]ths of a point.
    pub strength: i64,
    /// Sessions played (byes not counted).
    pub sessions: u32,
    pub byes: u32,
}

/// Points for each entrant of one finishing order, in [`UNIT`]ths.
pub fn points(places: &[u8]) -> Result<Vec<i64>, Error> {
    let n = places.len();
    if !(2..=MAX_ENTRANTS).contains(&n) {
        return Err(Error::BadOrder);
    }
    let mut sorted: Vec<usize> = places.iter().map(|p| *p as usize).collect();
    sorted.sort_unstable();
    for (i, p) in sorted.iter().enumerate() {
        let ok = if i == 0 {
            *p == 0
        } else {
            *p == i || *p == sorted[i - 1]
        };
        if !ok {
            return Err(Error::BadOrder);
        }
    }
    let denominator = 2 * (n as i64 - 1);
    Ok(places
        .iter()
        .map(|p| {
            let p = *p as i64;
            let tied = places.iter().filter(|q| **q as i64 == p).count() as i64;
            // Average 0-based place of the tie: (2p + t − 1) / 2.
            let numerator = denominator - (2 * p + tied - 1);
            UNIT * numerator / denominator
        })
        .collect())
}

/// A week in progress: its field and everything its rounds have done so far.
/// Replaying the same rounds always gives the same next round.
#[derive(Debug, Clone)]
pub struct Week {
    field: Field,
    points: Vec<i64>,
    byes: Vec<u32>,
    sessions: Vec<u32>,
    /// Opponents each bot finished strictly ahead of, once per time.
    beaten: Vec<Vec<usize>>,
    meetings: BTreeMap<(usize, usize), u32>,
    rounds: usize,
}

impl Week {
    pub fn new(field: Field) -> Self {
        let n = field.len();
        Self {
            field,
            points: vec![0; n],
            byes: vec![0; n],
            sessions: vec![0; n],
            beaten: vec![Vec::new(); n],
            meetings: BTreeMap::new(),
            rounds: 0,
        }
    }

    pub fn field(&self) -> &Field {
        &self.field
    }

    /// Rounds recorded so far.
    pub fn rounds(&self) -> usize {
        self.rounds
    }

    fn met(&self, a: usize, b: usize) -> u32 {
        let key = if a < b { (a, b) } else { (b, a) };
        self.meetings.get(&key).copied().unwrap_or(0)
    }

    fn strength(&self, bot: usize) -> i64 {
        self.beaten[bot].iter().map(|o| self.points[*o]).sum()
    }

    /// Every bot, best first: points, strength of opponents beaten, rating at
    /// the freeze, digest.
    pub fn order(&self) -> Vec<usize> {
        let strength: Vec<i64> = (0..self.field.len()).map(|b| self.strength(b)).collect();
        let mut order: Vec<usize> = (0..self.field.len()).collect();
        order.sort_by(|&a, &b| {
            self.points[b]
                .cmp(&self.points[a])
                .then(strength[b].cmp(&strength[a]))
                .then(
                    self.field.bots[b]
                        .ordinal()
                        .cmp(&self.field.bots[a].ordinal()),
                )
                .then(self.field.bots[a].digest.cmp(&self.field.bots[b].digest))
        });
        order
    }

    /// The standings, best first.
    pub fn standings(&self) -> Vec<Standing> {
        self.order()
            .into_iter()
            .map(|bot| Standing {
                bot,
                points: self.points[bot],
                strength: self.strength(bot),
                sessions: self.sessions[bot],
                byes: self.byes[bot],
            })
            .collect()
    }

    /// The next round; one with no sessions means the week runs no more.
    /// `absent` lists bots that can't be posted this round
    /// (the keeper's only use; a check always passes none, so a round posted
    /// with someone absent is off the version, as intended).
    pub fn next_round(&self, absent: &[usize]) -> Round {
        let (min, max) = (self.field.min, self.field.max);
        let ranked: Vec<usize> = self
            .order()
            .into_iter()
            .filter(|b| !absent.contains(b))
            .collect();
        let n = ranked.len();
        if n < min {
            return Round::default();
        }
        let fits = |m: usize| m >= min && m >= m.div_ceil(max) * min;
        let bye_count = (0..=n - min).find(|b| fits(n - b)).unwrap_or(0);
        // Byes: fewest so far, lowest standing first; but never a choice that
        // leaves more bots unpaired than another would (one at a time, the
        // first candidate in that order that leaves fewest unpaired).
        let mut by_need: Vec<(usize, usize)> = ranked.iter().copied().enumerate().collect();
        by_need.sort_by(|(pa, a), (pb, b)| self.byes[*a].cmp(&self.byes[*b]).then(pb.cmp(pa)));
        let need: Vec<usize> = by_need.into_iter().map(|(_, b)| b).collect();
        let mut byes: Vec<usize> = Vec::with_capacity(bye_count);
        for slot in 0..bye_count {
            let mut best: Option<(usize, usize)> = None;
            for &c in need.iter().filter(|c| !byes.contains(c)) {
                let mut trial = byes.clone();
                trial.push(c);
                trial.extend(
                    need.iter()
                        .filter(|b| !trial.contains(b))
                        .take(bye_count - slot - 1)
                        .copied()
                        .collect::<Vec<_>>(),
                );
                let left = self.fill(&ranked, &trial).unpaired.len();
                if best.is_none_or(|(_, l)| left < l) {
                    best = Some((c, left));
                }
                if left == 0 {
                    break;
                }
            }
            if let Some((c, _)) = best {
                byes.push(c);
            }
        }
        self.fill(&ranked, &byes)
    }

    /// The sessions for `ranked` (standings order) once `byes` sit out.
    fn fill(&self, ranked: &[usize], byes: &[usize]) -> Round {
        let (min, max) = (self.field.min, self.field.max);
        let rest: Vec<usize> = ranked
            .iter()
            .copied()
            .filter(|b| !byes.contains(b))
            .collect();
        let m = rest.len();
        if m == 0 {
            return Round::default();
        }
        let k = m.div_ceil(max);
        let sizes: Vec<usize> = (0..k).map(|j| m / k + usize::from(j < m % k)).collect();
        let sequence: Vec<usize> = if self.rounds == 0 {
            (0..k)
                .flat_map(|j| (j..m).step_by(k).map(|p| rest[p]))
                .collect()
        } else {
            rest
        };
        let group = |b: usize| self.field.bots[b].group;
        if max == 2 {
            return self.pair(&sequence, byes);
        }
        let fits_group =
            |session: &[usize], b: usize| session.iter().all(|s| group(*s) != group(b));
        let mut left: Vec<Option<usize>> = sequence.into_iter().map(Some).collect();
        let mut sessions: Vec<Vec<usize>> = Vec::with_capacity(k);
        for size in &sizes {
            let mut session: Vec<usize> = Vec::with_capacity(*size);
            while session.len() < *size {
                let candidates = left
                    .iter()
                    .enumerate()
                    .filter_map(|(i, b)| b.map(|b| (i, b)))
                    .filter(|(_, b)| fits_group(&session, *b));
                let Some((i, b)) = candidates.min_by_key(|(i, _)| *i) else {
                    break;
                };
                left[i] = None;
                session.push(b);
            }
            sessions.push(session);
        }
        // The fallback: leftovers join any session with room that takes them.
        let mut unpaired = Vec::new();
        for b in left.into_iter().flatten() {
            match sessions
                .iter_mut()
                .find(|s| s.len() < max && fits_group(s, b))
            {
                Some(s) => s.push(b),
                None => unpaired.push(b),
            }
        }
        let mut kept = Vec::with_capacity(sessions.len());
        for s in sessions {
            if s.len() >= min {
                kept.push(s);
            } else {
                unpaired.extend(s);
            }
        }
        for s in &mut kept {
            s.sort_unstable();
        }
        kept.sort_by_key(|s| s[0]);
        let mut byes = byes.to_vec();
        byes.sort_unstable();
        unpaired.sort_unstable();
        Round {
            sessions: kept,
            byes,
            unpaired,
        }
    }

    /// A 1v1 round's pairs for `sequence` once `byes` sit out. Each bot in
    /// turn takes the first eligible partner in preference order (sequence
    /// order in round 1, then least met, nearest first) that still lets the
    /// rest pair as fully as possible; a bot with no such partner is unpaired.
    /// Only the group rule forbids a pair, so the most the rest can pair is
    /// exact: all but the excess of its largest group over the others.
    fn pair(&self, sequence: &[usize], byes: &[usize]) -> Round {
        let group = |b: usize| self.field.bots[b].group;
        let fewest_unpaired = |rest: &[usize]| {
            let mut counts: BTreeMap<Id, usize> = BTreeMap::new();
            for b in rest {
                *counts.entry(group(*b)).or_default() += 1;
            }
            let largest = counts.values().copied().max().unwrap_or(0);
            let n = rest.len();
            n - 2 * (n / 2).min(n - largest)
        };
        let mut left: Vec<usize> = sequence.to_vec();
        let mut unpaired = Vec::new();
        let mut spare = fewest_unpaired(&left);
        let mut sessions: Vec<Vec<usize>> = Vec::new();
        while !left.is_empty() {
            let a = left.remove(0);
            let mut candidates: Vec<(usize, usize)> = left
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, b)| group(*b) != group(a))
                .collect();
            if self.rounds > 0 {
                candidates.sort_by_key(|(i, b)| (self.met(a, *b), *i));
            }
            let pick = candidates.into_iter().find(|(i, _)| {
                let mut rest = left.clone();
                rest.remove(*i);
                fewest_unpaired(&rest) <= spare
            });
            match pick {
                Some((i, b)) => {
                    left.remove(i);
                    let mut session = vec![a, b];
                    session.sort_unstable();
                    sessions.push(session);
                }
                None => {
                    unpaired.push(a);
                    spare = spare.saturating_sub(1);
                }
            }
        }
        sessions.sort_by_key(|s| s[0]);
        let mut byes = byes.to_vec();
        byes.sort_unstable();
        unpaired.sort_unstable();
        Round {
            sessions,
            byes,
            unpaired,
        }
    }

    /// Record a round: its sessions as played and its byes.
    pub fn record(&mut self, played: &[Played], byes: &[usize]) -> Result<(), Error> {
        let n = self.field.len();
        let mut seen = vec![false; n];
        for p in played {
            if p.entrants.len() < 2 || p.entrants.iter().any(|e| *e >= n || seen[*e]) {
                return Err(Error::BadSession);
            }
            for e in &p.entrants {
                seen[*e] = true;
            }
            if let Outcome::Order(places) = &p.outcome
                && places.len() != p.entrants.len()
            {
                return Err(Error::BadOrder);
            }
        }
        if byes.iter().any(|b| *b >= n || seen[*b]) {
            return Err(Error::BadSession);
        }
        for p in played {
            if let Outcome::Order(places) = &p.outcome {
                let scored = points(places)?;
                for (i, e) in p.entrants.iter().enumerate() {
                    self.points[*e] += scored[i];
                    for (j, o) in p.entrants.iter().enumerate() {
                        if places[i] < places[j] {
                            self.beaten[*e].push(*o);
                        }
                    }
                }
            }
            for (i, a) in p.entrants.iter().enumerate() {
                self.sessions[*a] += 1;
                for b in &p.entrants[i + 1..] {
                    let key = if a < b { (*a, *b) } else { (*b, *a) };
                    *self.meetings.entry(key).or_insert(0) += 1;
                }
            }
        }
        for b in byes {
            self.points[*b] += UNIT;
            self.byes[*b] += 1;
        }
        self.rounds += 1;
        Ok(())
    }

    /// The finalists, best first: the top finisher of each account group, up
    /// to the final's size. Empty if fewer than the arena's minimum.
    pub fn finalists(&self) -> Vec<usize> {
        let size = self.field.final_size();
        let mut groups: Vec<Id> = Vec::new();
        let mut out = Vec::new();
        for b in self.order() {
            if out.len() == size {
                break;
            }
            let g = self.field.bots[b].group;
            if !groups.contains(&g) {
                groups.push(g);
                out.push(b);
            }
        }
        if out.len() < self.field.min {
            out.clear();
        }
        out
    }
}

/// Why a posted week is off the standings version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Round `round` (1-based) isn't the version's pairing.
    Mispaired {
        round: usize,
    },
    /// A round was posted though the field runs none.
    NoRounds,
    Malformed(Error),
}

/// Check a posted week against the standings version: every round's
/// sessions, from the field and the outcomes before it. Returns the week
/// after them, for its standings and finalists.
pub fn check_week(field: &Field, rounds: &[Vec<Played>]) -> Result<Week, Fault> {
    let mut week = Week::new(field.clone());
    for (r, posted) in rounds.iter().enumerate() {
        let want = week.next_round(&[]);
        if want.sessions.is_empty() {
            return Err(Fault::NoRounds);
        }
        let mut got: Vec<Vec<usize>> = posted
            .iter()
            .map(|p| {
                let mut e = p.entrants.clone();
                e.sort_unstable();
                e
            })
            .collect();
        got.sort();
        let mut expected = want.sessions.clone();
        expected.sort();
        if got != expected {
            return Err(Fault::Mispaired { round: r + 1 });
        }
        let posted: Vec<Played> = posted
            .iter()
            .map(|p| {
                // Outcomes in sorted entrant order, so recording is canonical.
                let mut pairs: Vec<(usize, Option<u8>)> = match &p.outcome {
                    Outcome::Order(places) => p
                        .entrants
                        .iter()
                        .copied()
                        .zip(places.iter().map(|x| Some(*x)))
                        .collect(),
                    _ => p.entrants.iter().map(|e| (*e, None)).collect(),
                };
                pairs.sort_unstable();
                Played {
                    entrants: pairs.iter().map(|(e, _)| *e).collect(),
                    outcome: match &p.outcome {
                        Outcome::Order(_) => {
                            Outcome::Order(pairs.iter().map(|(_, x)| x.unwrap_or(0)).collect())
                        }
                        other => other.clone(),
                    },
                }
            })
            .collect();
        week.record(&posted, &want.byes).map_err(Fault::Malformed)?;
    }
    Ok(week)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> Id {
        let mut d = [0u8; 32];
        d[28..].copy_from_slice(&n.to_be_bytes());
        d
    }

    /// `n` bots, each its own group, rated so bot 0 is best.
    fn field(n: u32, min: usize, max: usize) -> Field {
        let bots = (0..n)
            .map(|i| FieldBot {
                digest: id(i + 1),
                mu: 40_000_000 - i64::from(i) * 1_000_000,
                sigma: 1_000_000,
                group: id(1000 + i),
            })
            .collect();
        Field::new(bots, min, max).unwrap()
    }

    #[test]
    fn points_are_exact_for_wins_draws_heats_and_ties() {
        assert_eq!(points(&[0, 1]).unwrap(), vec![UNIT, 0]);
        assert_eq!(points(&[0, 0]).unwrap(), vec![UNIT / 2, UNIT / 2]);
        assert_eq!(
            points(&[0, 1, 2, 3]).unwrap(),
            vec![UNIT, UNIT * 2 / 3, UNIT / 3, 0]
        );
        // Two drones tied for first in a 4-drone heat: ⅚ each.
        assert_eq!(
            points(&[0, 3, 0, 2]).unwrap(),
            vec![UNIT * 5 / 6, 0, UNIT * 5 / 6, UNIT / 3]
        );
        // Every size up to the maximum divides exactly.
        for n in 2..=MAX_ENTRANTS {
            assert_eq!(UNIT % (2 * (n as i64 - 1)), 0);
        }
        assert!(points(&[0, 2]).is_err());
        assert!(points(&[1, 1]).is_err());
        assert!(points(&[0]).is_err());
    }

    #[test]
    fn a_field_must_be_ascending_and_within_the_version_s_range() {
        let mut bots = field(3, 2, 2).bots().to_vec();
        bots.swap(0, 1);
        assert_eq!(Field::new(bots, 2, 2), Err(Error::NotAscending));
        assert!(Field::new(field(3, 2, 2).bots().to_vec(), 2, 9).is_err());
        assert!(Field::new(field(3, 2, 2).bots().to_vec(), 1, 2).is_err());
        assert_eq!(Field::new(vec![], 2, 2), Err(Error::Empty));
    }

    #[test]
    fn round_one_is_top_half_against_bottom_half() {
        let week = Week::new(field(8, 2, 2));
        let r = week.next_round(&[]);
        assert_eq!(
            r.sessions,
            vec![vec![0, 4], vec![1, 5], vec![2, 6], vec![3, 7]]
        );
        assert!(r.byes.is_empty() && r.unpaired.is_empty());
    }

    #[test]
    fn an_odd_field_gives_the_lowest_bot_without_one_a_bye_worth_a_point() {
        let mut week = Week::new(field(5, 2, 2));
        let r = week.next_round(&[]);
        assert_eq!(r.byes, vec![4]);
        assert_eq!(r.sessions, vec![vec![0, 2], vec![1, 3]]);
        let played: Vec<Played> = r
            .sessions
            .iter()
            .map(|s| Played {
                entrants: s.clone(),
                outcome: Outcome::Order(vec![0, 1]),
            })
            .collect();
        week.record(&played, &r.byes).unwrap();
        let s = week.standings();
        assert_eq!(s.iter().find(|x| x.bot == 4).unwrap().points, UNIT);
        // Bot 4 had its bye: the next goes to the lowest bot without one.
        let r2 = week.next_round(&[]);
        assert_eq!(r2.byes.len(), 1);
        assert_ne!(r2.byes[0], 4);
    }

    #[test]
    fn a_field_below_the_minimum_runs_no_rounds() {
        let week = Week::new(field(1, 2, 2));
        assert_eq!(week.next_round(&[]), Round::default());
        assert!(week.finalists().is_empty());
    }

    #[test]
    fn heats_are_as_even_as_possible_within_the_range() {
        // 13 drones, 2..6 per heat: three heats of 5, 4 and 4.
        let week = Week::new(field(13, 2, 6));
        let r = week.next_round(&[]);
        let mut sizes: Vec<usize> = r.sessions.iter().map(Vec::len).collect();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![4, 4, 5]);
        assert!(r.byes.is_empty());
        // 3..4 per heat with 5 bots: one heat of 4 and a bye.
        let week = Week::new(field(5, 3, 4));
        let r = week.next_round(&[]);
        assert_eq!(r.sessions.len(), 1);
        assert_eq!(r.sessions[0].len(), 4);
        assert_eq!(r.byes, vec![4]);
    }

    #[test]
    fn two_bots_of_one_group_never_share_a_session() {
        let mut bots = field(4, 2, 2).bots().to_vec();
        // Bots 0 and 2 share a group: round 1 would pair them.
        bots[2].group = bots[0].group;
        let week = Week::new(Field::new(bots, 2, 2).unwrap());
        let r = week.next_round(&[]);
        assert_eq!(r.sessions, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn a_bot_no_session_can_take_sits_out_unpaired_with_no_point() {
        let mut bots = field(2, 2, 2).bots().to_vec();
        bots[1].group = bots[0].group;
        let mut week = Week::new(Field::new(bots, 2, 2).unwrap());
        let r = week.next_round(&[]);
        assert!(r.sessions.is_empty());
        assert_eq!(r.unpaired, vec![0, 1]);
        week.record(&[], &r.byes).unwrap();
        assert!(week.standings().iter().all(|s| s.points == 0));
    }

    #[test]
    fn a_1v1_pair_is_taken_only_if_the_rest_can_still_pair() {
        // Bots 2 and 3 share a group. Round 1 deals 0–2 and 1–3; the winners
        // 0 and 1 lead round 2. Taking the nearest, 0–1, would leave 2 and 3
        // with nobody, so 0 takes 3 and 1 takes 2.
        let mut bots = field(4, 2, 2).bots().to_vec();
        bots[3].group = bots[2].group;
        let mut week = Week::new(Field::new(bots, 2, 2).unwrap());
        let r = week.next_round(&[]);
        assert_eq!(r.sessions, vec![vec![0, 2], vec![1, 3]]);
        let played: Vec<Played> = r
            .sessions
            .iter()
            .map(|s| Played {
                entrants: s.clone(),
                outcome: Outcome::Order(vec![0, 1]),
            })
            .collect();
        week.record(&played, &r.byes).unwrap();
        let r = week.next_round(&[]);
        assert_eq!(r.sessions, vec![vec![0, 3], vec![1, 2]]);
        assert!(r.unpaired.is_empty());
    }

    #[test]
    fn a_1v1_round_leaves_unpaired_only_the_excess_of_the_largest_group() {
        // Four of six bots share a group: two of them must sit out.
        let mut bots = field(6, 2, 2).bots().to_vec();
        for i in [1, 2, 3] {
            bots[i].group = bots[0].group;
        }
        let week = Week::new(Field::new(bots, 2, 2).unwrap());
        let r = week.next_round(&[]);
        assert_eq!(r.sessions.len(), 2);
        assert_eq!(r.unpaired.len(), 2);
        for s in &r.sessions {
            assert!(s.contains(&4) || s.contains(&5));
        }
    }

    #[test]
    fn repeats_wait_until_every_pairing_is_used() {
        let mut week = Week::new(field(4, 2, 2));
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..3 {
            let r = week.next_round(&[]);
            for s in &r.sessions {
                assert!(
                    seen.insert(s.clone()),
                    "a repeat before the cycle ends: {s:?}"
                );
            }
            let played: Vec<Played> = r
                .sessions
                .iter()
                .map(|s| Played {
                    entrants: s.clone(),
                    outcome: Outcome::Order(vec![0, 1]),
                })
                .collect();
            week.record(&played, &r.byes).unwrap();
        }
        assert_eq!(seen.len(), 6, "all six pairings of four bots");
        // A second cycle begins by itself.
        assert_eq!(week.next_round(&[]).sessions.len(), 2);
    }

    #[test]
    fn a_void_or_a_missing_result_scores_nothing() {
        let mut week = Week::new(field(4, 2, 2));
        week.record(
            &[
                Played {
                    entrants: vec![0, 2],
                    outcome: Outcome::Void,
                },
                Played {
                    entrants: vec![1, 3],
                    outcome: Outcome::Missing,
                },
            ],
            &[],
        )
        .unwrap();
        assert!(
            week.standings()
                .iter()
                .all(|s| s.points == 0 && s.sessions == 1)
        );
    }

    #[test]
    fn ties_break_on_strength_then_rating_then_digest() {
        // 0 beats 1, 2 beats 3; then 1 beats 3... strength separates the
        // two one-point bots by whom they beat.
        let mut week = Week::new(field(4, 2, 2));
        let win = |a: usize, b: usize| Played {
            entrants: vec![a.min(b), a.max(b)],
            outcome: Outcome::Order(if a < b { vec![0, 1] } else { vec![1, 0] }),
        };
        week.record(&[win(3, 0), win(2, 1)], &[]).unwrap();
        week.record(&[win(3, 2), win(0, 1)], &[]).unwrap();
        // Points: 3 → 2, 0 → 1, 2 → 1, 1 → 0. Bot 0 beat 1 (0 points), bot 2
        // beat 1 too: equal strength, so rating decides: 0 is rated higher.
        let order: Vec<usize> = week.standings().iter().map(|s| s.bot).collect();
        assert_eq!(order, vec![3, 0, 2, 1]);
    }

    #[test]
    fn finalists_are_the_top_of_each_group_up_to_the_final_s_size() {
        let mut bots = field(8, 2, 6).bots().to_vec();
        bots[1].group = bots[0].group;
        let week = Week::new(Field::new(bots, 2, 6).unwrap());
        // No rounds yet: rating order. Final size 8 / 2 = 4, bot 1 skipped.
        assert_eq!(week.finalists(), vec![0, 2, 3, 4]);
        assert_eq!(final_size(3, 2, 2), 2);
        assert_eq!(final_size(40, 2, 6), 6);
        assert!(championship(4, 2, 0) && !championship(3, 2, 0) && !championship(4, 2, 1));
    }

    #[test]
    fn check_week_accepts_the_version_s_rounds_and_flags_any_other() {
        let f = field(6, 2, 2);
        let mut week = Week::new(f.clone());
        let mut posted = Vec::new();
        for _ in 0..3 {
            let r = week.next_round(&[]);
            let played: Vec<Played> = r
                .sessions
                .iter()
                .map(|s| Played {
                    entrants: s.clone(),
                    outcome: Outcome::Order(vec![1, 0]),
                })
                .collect();
            week.record(&played, &r.byes).unwrap();
            posted.push(played);
        }
        let checked = check_week(&f, &posted).unwrap();
        assert_eq!(checked.finalists(), week.finalists());
        let mut bad = posted.clone();
        let (a, b) = (bad[1][0].entrants[1], bad[1][1].entrants[1]);
        bad[1][0].entrants[1] = b;
        bad[1][1].entrants[1] = a;
        assert_eq!(
            check_week(&f, &bad).unwrap_err(),
            Fault::Mispaired { round: 2 }
        );
    }

    #[test]
    fn absent_bots_are_paired_around() {
        let week = Week::new(field(5, 2, 2));
        let r = week.next_round(&[4]);
        assert!(r.byes.is_empty());
        assert_eq!(r.sessions.len(), 2);
        assert!(r.sessions.iter().flatten().all(|b| *b != 4));
    }
}
