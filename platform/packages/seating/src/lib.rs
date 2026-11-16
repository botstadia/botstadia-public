//! The seating version (BOTS.md#the-bot-interface): who sits in which seat.
//!
//! A session's spec lists its entrants in bundle-digest order and says
//! nothing about seats. The enclave draws them from the session's seed, which
//! it signs into the result, so once the result is on-chain anyone can redraw
//! them with this crate. `seats(seed, n)[i]` is the spec position sitting in
//! seat `i`.
//!
//! The draw is a uniform Fisher-Yates shuffle on a stream derived one way
//! from the seed under the label `seats`, exactly as each seat's and the
//! arena's own streams are derived. Changing anything here is a new version.
//! The published test vectors (`vectors/`) pin version 1.

use sha2::{Digest, Sha256};
use sha3::Keccak256;

/// This crate's seating version.
pub const VERSION: u32 = 1;

/// A session's seed: the one its result carries.
pub type Seed = [u8; 32];

/// The stream for `label`, derived one way from `seed`: the session runner's
/// labelled streams, of which `seats` is one.
pub fn derive(seed: &Seed, label: &str) -> Seed {
    let mut hash = Sha256::new();
    hash.update(b"botstadia-seed-v1");
    hash.update(seed);
    hash.update((label.len() as u32).to_le_bytes());
    hash.update(label.as_bytes());
    hash.finalize().into()
}

/// Seats from the seed: `seats[i]` is the spec position sitting in seat `i`.
pub fn seats(seed: &Seed, entrants: usize) -> Vec<usize> {
    let stream = derive(seed, "seats");
    let mut order: Vec<usize> = (0..entrants).collect();
    for i in (1..entrants).rev() {
        let mut hash = Keccak256::new();
        hash.update(stream);
        hash.update((i as u64).to_be_bytes());
        let draw: [u8; 32] = hash.finalize().into();
        let r = u64::from_be_bytes(draw[..8].try_into().expect("8 bytes"));
        order.swap(i, (r % (i as u64 + 1)) as usize);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_gives_one_shuffle() {
        let seed = [7u8; 32];
        let a = seats(&seed, 6);
        assert_eq!(a, seats(&seed, 6));
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, (0..6).collect::<Vec<_>>());
        assert_ne!(a, seats(&[8u8; 32], 6), "another seed, another draw");
        assert_eq!(seats(&seed, 1), vec![0]);
        assert!(seats(&seed, 0).is_empty());
    }

    #[test]
    fn every_seat_is_drawn_evenly() {
        // Entrant 0's seat over many seeds: no seat favoured.
        let mut counts = [0u32; 4];
        for n in 0..4000u32 {
            let mut seed = [0u8; 32];
            seed[..4].copy_from_slice(&n.to_be_bytes());
            let s = seats(&seed, 4);
            counts[s.iter().position(|p| *p == 0).unwrap()] += 1;
        }
        assert!(counts.iter().all(|c| (850..1150).contains(c)), "{counts:?}");
    }
}
