//! Seeded randomness with labelled streams.
//!
//! The host derives the arena's seed; every random choice a world makes comes
//! from a stream drawn from it under its own label (`layout`, `props`,
//! `start`), so adding a stream never shifts another's numbers.

use sha2::{Digest, Sha256};

/// The arena's seed, as the host gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed(Vec<u8>);

impl Seed {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// A seed from a number, for tests and local runs.
    pub fn from_u64(value: u64) -> Self {
        Self(value.to_le_bytes().to_vec())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// The stream for `label`, independent of every other label's.
    pub fn stream(&self, label: &str) -> Rng {
        let mut hash = Sha256::new();
        hash.update(b"botstadia-arena-stream");
        hash.update((self.0.len() as u64).to_le_bytes());
        hash.update(&self.0);
        hash.update(label.as_bytes());
        let digest = hash.finalize();
        let word = |index: usize| {
            u64::from_le_bytes(
                digest[index * 8..index * 8 + 8]
                    .try_into()
                    .expect("eight bytes"),
            )
        };
        let mut state = [word(0), word(1), word(2), word(3)];
        if state == [0; 4] {
            state[0] = 1;
        }
        Rng { state }
    }
}

/// xoshiro256**: small, fast, and plenty for scenarios.
#[derive(Debug, Clone)]
pub struct Rng {
    state: [u64; 4],
}

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        let [a, b, c, d] = &mut self.state;
        let result = b.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = *b << 17;
        *c ^= *a;
        *d ^= *b;
        *b ^= *c;
        *a ^= *d;
        *c ^= t;
        *d = d.rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[min, max)`.
    pub fn range(&mut self, min: f64, max: f64) -> f64 {
        min + self.unit() * (max - min)
    }

    /// Uniform in `0..n`. `n` must be at least 1.
    pub fn below(&mut self, n: u32) -> u32 {
        ((self.unit() * f64::from(n)) as u32).min(n - 1)
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_stable_and_independent() {
        let seed = Seed::from_u64(7);
        let first: Vec<u64> = (0..4)
            .map({
                let mut rng = seed.stream("layout");
                move |_| rng.next_u64()
            })
            .collect();
        let again: Vec<u64> = (0..4)
            .map({
                let mut rng = seed.stream("layout");
                move |_| rng.next_u64()
            })
            .collect();
        assert_eq!(first, again);
        assert_ne!(
            seed.stream("layout").next_u64(),
            seed.stream("start").next_u64()
        );
        assert_ne!(
            seed.stream("layout").next_u64(),
            Seed::from_u64(8).stream("layout").next_u64()
        );
        let mut rng = seed.stream("x");
        assert!((0..1000).all(|_| rng.below(3) < 3));
    }
}
