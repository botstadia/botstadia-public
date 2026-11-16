//! The routine and its tempo, by the design note's rules, over many seeds
//! and at both ends of the tempo range.

use std::collections::HashSet;

use arena_kit::Seed;
use dance_off_declaration::*;
use dance_off_engine::{Card, routine, routine_at, tempo};

fn pose(card: &Card) -> [f32; 29] {
    if card.mirrored {
        mirror(&MOVES[card.move_id].targets)
    } else {
        MOVES[card.move_id].targets
    }
}

fn check(cards: &[Card], bpm: u32) {
    let at = |i: usize| format!("{bpm} BPM, card {i}");
    let mut previous: Option<[f32; 29]> = None;
    for (i, card) in cards.iter().enumerate() {
        assert_eq!(card.beat, card_beat_turn(i, bpm), "{}", at(i));
        assert_eq!(card.window, card.kind.window_turns(bpm), "{}", at(i));
        assert!(card.closes() <= MAX_TURNS, "{} closes after the end", at(i));
        assert_eq!(
            card.kind == CardKind::Freestyle,
            is_freestyle(i),
            "{}",
            at(i)
        );
        if card.kind == CardKind::Freestyle {
            continue;
        }
        if i < PLAIN_CARDS {
            assert_eq!(
                (card.kind, card.mirrored),
                (CardKind::Pose, false),
                "{}",
                at(i)
            );
        }
        let pool: Vec<usize> = if i < RAMP_CARDS {
            BASIC_MOVES.to_vec()
        } else if i < TIER_3_CARDS {
            BASIC_MOVES.iter().chain(&ARM_MOVES).copied().collect()
        } else {
            (0..MOVES.len()).collect()
        };
        // A phrase draws from the tier of its first card.
        let first = if i < PLAIN_CARDS {
            i / PHRASE * PHRASE
        } else {
            i - (i - PLAIN_CARDS) % BLOCK % PHRASE
        };
        let pool_first: Vec<usize> = if first < RAMP_CARDS {
            BASIC_MOVES.to_vec()
        } else if first < TIER_3_CARDS {
            BASIC_MOVES.iter().chain(&ARM_MOVES).copied().collect()
        } else {
            (0..MOVES.len()).collect()
        };
        assert!(
            pool_first.contains(&card.move_id) || pool.contains(&card.move_id),
            "{}: move {} outside its tier",
            at(i),
            card.move_id
        );
        assert_ne!(
            previous,
            Some(pose(card)),
            "{}: the same pose twice running",
            at(i)
        );
        previous = Some(pose(card));
    }
    // The routine is every card whose window closes inside the session.
    let next = cards.len();
    let kind = if is_freestyle(next) {
        CardKind::Freestyle
    } else {
        CardKind::Pose
    };
    assert!(
        card_beat_turn(next, bpm) + kind.window_turns(bpm) > MAX_TURNS
            || card_beat_turn(next, bpm) + CardKind::Hold.window_turns(bpm) > MAX_TURNS,
        "{bpm} BPM: another card would have fit"
    );
}

#[test]
fn routines_follow_the_rules_at_both_ends_of_the_tempo_range() {
    for bpm in [BPM_MIN, DEFAULT_BPM, BPM_MAX] {
        for seed in 0..200 {
            check(&routine_at(&Seed::from_u64(seed), bpm), bpm);
        }
    }
    let fastest = routine_at(&Seed::from_u64(1), BPM_MAX).len();
    let slowest = routine_at(&Seed::from_u64(1), BPM_MIN).len();
    println!("cards: {slowest} at {BPM_MIN} BPM, {fastest} at {BPM_MAX}");
    let most = (0..200)
        .map(|s| routine_at(&Seed::from_u64(s), BPM_MAX).len())
        .max()
        .unwrap();
    assert_eq!(most, MAX_CARDS, "the fastest tempo's longest routine");
}

#[test]
fn the_tempo_spans_its_range() {
    let tempos: HashSet<u32> = (0..2000).map(|s| tempo(&Seed::from_u64(s))).collect();
    assert!(tempos.iter().all(|t| (BPM_MIN..=BPM_MAX).contains(t)));
    assert!(tempos.contains(&BPM_MIN) && tempos.contains(&BPM_MAX));
    assert_eq!(tempos.len() as u32, BPM_MAX - BPM_MIN + 1);
}

#[test]
fn every_routine_draws_on_every_twist_and_move() {
    let mut moves = HashSet::new();
    let (mut mirrors, mut holds, mut freestyles) = (0, 0, 0);
    for seed in 0..200 {
        let (bpm, cards) = routine(&Seed::from_u64(seed));
        check(&cards, bpm);
        for card in &cards {
            if card.kind != CardKind::Freestyle {
                moves.insert(card.move_id);
            }
        }
        mirrors += cards.iter().filter(|c| c.mirrored).count();
        holds += cards.iter().filter(|c| c.kind == CardKind::Hold).count();
        freestyles += cards
            .iter()
            .filter(|c| c.kind == CardKind::Freestyle)
            .count();
    }
    assert_eq!(moves.len(), MOVES.len(), "moves never drawn");
    assert!(mirrors > 0 && holds > 0 && freestyles > 0);
}

#[test]
fn no_two_sessions_share_a_routine() {
    let mut seen = HashSet::new();
    for seed in 0..10_000 {
        // The moves and twists alone, whatever the tempo.
        let (_, cards) = routine(&Seed::from_u64(seed));
        let key: Vec<(usize, u8, bool)> = cards
            .iter()
            .take(59)
            .map(|c| (c.move_id, c.kind as u8, c.mirrored))
            .collect();
        assert!(seen.insert(key), "seed {seed} repeats a routine");
    }
}
