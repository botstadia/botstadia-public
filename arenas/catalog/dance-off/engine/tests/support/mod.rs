//! Test policies. They read the declaration crate only, never the engine, as
//! the real test bots will.

#![allow(dead_code)]

use arena_kit::Values;
use arena_kit::declaration::Declaration;
use arena_kit::testing::Policy;
use dance_off_declaration::*;

/// Hold the start stance.
pub fn stand(_: &Declaration, _: u32, _: &[Values]) -> Option<Vec<Values>> {
    Some(vec![Values::F32(START_TARGETS.to_vec())])
}

fn input_index(declaration: &Declaration, name: &str) -> usize {
    declaration
        .inputs
        .iter()
        .position(|v| v.name == name)
        .expect("declared input")
}

/// Seconds before a card's beat the scripted dancer starts moving into it.
pub const LEAD: f32 = 0.7;
/// …but never more than this many beats, so at a fast tempo the card before
/// still gets the second half of its window.
pub const LEAD_BEATS: f32 = 1.6;

/// The scripted baseline: classical control, no learning. It moves its whole
/// body (the legs too: reaching forward on straight legs topples a G1) from
/// one card's pose to the next over the last `LEAD` seconds before the beat,
/// holds it through the window, and on a freestyle bar swings its arms and
/// bobs its knees in time.
pub struct Scripted {
    from: [f32; 29],
    goal: [f32; 29],
    command: [f32; 29],
    /// For spreading outcomes in tests: a seeded wobble on every target, radians.
    noise: f32,
    rng: u64,
}

impl Scripted {
    pub fn new() -> Self {
        Self::noisy(0, 0.0)
    }

    pub fn noisy(seed: u64, noise: f32) -> Self {
        Self {
            from: START_TARGETS,
            goal: START_TARGETS,
            command: START_TARGETS,
            noise,
            rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
        }
    }

    fn wobble(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * self.noise
    }
}

fn card(cards: &[f32], which: usize) -> ([f32; 29], f32, f32) {
    let c = &cards[which * CARD_VALUES..(which + 1) * CARD_VALUES];
    let mut targets = [0.0; 29];
    targets.copy_from_slice(&c[..29]);
    (targets, c[CARD_KIND], c[CARD_SECONDS])
}

impl Policy for Scripted {
    fn act(&mut self, declaration: &Declaration, _: u32, input: &[Values]) -> Option<Vec<Values>> {
        let cards = input[input_index(declaration, IN_CARDS)].as_f32().unwrap();
        let dance = input[input_index(declaration, IN_DANCE)].as_f32().unwrap();
        if dance[DANCE_DOWN] > 0.5 {
            let (noise, rng) = (self.noise, self.rng);
            *self = Self::noisy(0, noise);
            self.rng = rng;
            return Some(vec![Values::F32(START_TARGETS.to_vec())]);
        }
        // How early to start for a card: `LEAD`, or at a fast tempo less, so
        // the card before still gets its beat.
        let period = dance[DANCE_BEAT_PERIOD];
        let lead = LEAD.min(LEAD_BEATS * period);
        let (current, current_kind, current_seconds) = card(cards, 0);
        let (next, next_kind, next_seconds) = card(cards, 1);
        // The pose to be in next: the current card until its beat has passed
        // and the next one is close.
        let (goal, kind, seconds) = if current_seconds < 0.0 && next_seconds <= lead {
            (next, next_kind, next_seconds)
        } else {
            (current, current_kind, current_seconds)
        };
        if goal != self.goal {
            self.from = self.command;
            self.goal = goal;
        }
        let progress = (1.0 - seconds / lead).clamp(0.0, 1.0);
        for (i, command) in self.command.iter_mut().enumerate() {
            *command = self.from[i] + (self.goal[i] - self.from[i]) * progress;
        }
        let mut out = self.command;
        if kind as u32 == CardKind::Freestyle as u32 && seconds.abs() <= LONG_WINDOW_BEATS * period
        {
            // Swing both arms out and in, and bob, once a beat.
            let phase = dance[DANCE_BEAT_PHASE] * std::f32::consts::TAU;
            let swing = phase.sin();
            out[16] = 0.9 + 0.6 * swing;
            out[23] = -0.9 - 0.6 * swing;
            out[15] = -0.3 * swing;
            out[22] = -0.3 * swing;
            out[18] = 1.0 + 0.4 * swing;
            out[25] = 1.0 + 0.4 * swing;
            let bob = 0.5 + 0.5 * phase.cos();
            for (hip, knee, ankle) in [(0, 3, 4), (6, 9, 10)] {
                out[hip] = -0.1 - 0.25 * bob;
                out[knee] = 0.3 + 0.5 * bob;
                out[ankle] = -0.2 - 0.25 * bob;
            }
        }
        for (value, (min, max)) in out.iter_mut().zip(JOINT_RANGES) {
            *value = (*value + self.wobble()).clamp(min, max);
        }
        Some(vec![Values::F32(out.to_vec())])
    }
}

/// Folds its legs after a second, so something but a foot meets the stage,
/// and again each time it's stood back up.
pub struct Faller {
    turn: u32,
}

impl Faller {
    pub fn new() -> Self {
        Self { turn: 0 }
    }
}

impl Policy for Faller {
    fn act(&mut self, declaration: &Declaration, _: u32, input: &[Values]) -> Option<Vec<Values>> {
        let dance = input[input_index(declaration, IN_DANCE)].as_f32().unwrap();
        if dance[DANCE_DOWN] > 0.5 {
            self.turn = 0;
        }
        self.turn += 1;
        let mut q = START_TARGETS;
        if self.turn > 50 {
            for (hp, kn, ap) in [(0, 3, 4), (6, 9, 10)] {
                q[hp] = -2.2;
                q[kn] = 2.8;
                q[ap] = 0.5;
            }
        }
        Some(vec![Values::F32(q.to_vec())])
    }
}
