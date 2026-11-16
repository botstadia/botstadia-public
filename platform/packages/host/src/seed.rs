//! Labelled random streams. Each seat's randomness, and the arena's, is
//! derived one way from the session's seed under its own label, so no bot
//! learns the seed or any other stream.

use sha2::{Digest, Sha256};

/// A session's seed.
pub type Seed = [u8; 32];

/// The stream for `label`, derived one way from `seed`.
pub fn derive(seed: &Seed, label: &str) -> Seed {
    let mut hash = Sha256::new();
    hash.update(b"botstadia-seed-v1");
    hash.update(seed);
    hash.update((label.len() as u32).to_le_bytes());
    hash.update(label.as_bytes());
    hash.finalize().into()
}

/// A seed from a small number, for local runs and training: the same number
/// always gives the same session.
pub fn from_u64(value: u64) -> Seed {
    let mut hash = Sha256::new();
    hash.update(b"botstadia-local-seed");
    hash.update(value.to_le_bytes());
    hash.finalize().into()
}

/// The arena's stream.
pub fn arena(seed: &Seed) -> Seed {
    derive(seed, "arena")
}

/// Seat `index`'s stream.
pub fn seat(seed: &Seed, index: usize) -> Seed {
    derive(seed, &format!("seat-{index}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_differ_and_repeat() {
        let seed = from_u64(7);
        assert_ne!(arena(&seed), seat(&seed, 0));
        assert_ne!(seat(&seed, 0), seat(&seed, 1));
        assert_eq!(seat(&seed, 1), seat(&from_u64(7), 1));
    }
}
