//! The published test vectors (`vectors/v1.json`): seeds, entrant counts and
//! the seats version 1 draws for them. Anyone reimplementing the version
//! checks against them.
//!
//! `BOTSTADIA_WRITE_VECTORS=1 cargo test -p botstadia-seating --test vectors`
//! regenerates them; a plain run checks the crate still gives exactly what
//! they say.

use std::path::PathBuf;

use botstadia_seating::{VERSION, seats};
use serde_json::{Value, json};

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("vectors/v{VERSION}.json"))
}

fn generate() -> Value {
    let mut cases = Vec::new();
    for (i, n) in [1usize, 2, 2, 3, 4, 6, 6, 8].iter().enumerate() {
        let mut seed = [0u8; 32];
        for (j, b) in seed.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(j as u8 * 7);
        }
        cases.push(json!({
            "seed": format!("0x{}", hex::encode(seed)),
            "entrants": n,
            "seats": seats(&seed, *n),
        }));
    }
    json!({
        "version": VERSION,
        "about": "seats[i] is the spec position (entrants in bundle-digest order, from 0) sitting in seat i",
        "cases": cases,
    })
}

#[test]
fn the_vectors_hold() {
    let want = generate();
    if std::env::var("BOTSTADIA_WRITE_VECTORS").is_ok() {
        std::fs::create_dir_all(path().parent().unwrap()).unwrap();
        std::fs::write(path(), serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
    }
    let have: Value = serde_json::from_str(&std::fs::read_to_string(path()).unwrap()).unwrap();
    for case in have["cases"].as_array().unwrap() {
        let seed: [u8; 32] = hex::decode(case["seed"].as_str().unwrap().trim_start_matches("0x"))
            .unwrap()
            .try_into()
            .unwrap();
        let n = case["entrants"].as_u64().unwrap() as usize;
        let expected: Vec<usize> = serde_json::from_value(case["seats"].clone()).unwrap();
        assert_eq!(seats(&seed, n), expected, "seed {}", case["seed"]);
    }
    assert_eq!(have, want);
}
