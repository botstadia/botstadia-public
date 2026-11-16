//! The attestation guest: checks a Confidential Space token as
//! `botstadia_attestation_core::verify` does and commits the journal the
//! results contract decodes. A proof of this run is what approves a SubmitKey
//! (TRUST.md#the-tee-and-the-zk-proof).

use botstadia_attestation_core::{Input, verify};
use risc0_zkvm::guest::env;

fn main() {
    let input: Input = env::read();
    let journal = verify(&input).expect("the token passes every check");
    env::commit_slice(&journal.encode());
}
