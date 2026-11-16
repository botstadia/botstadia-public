//! Proving the attestation guest (TRUST.md#the-keeper-and-the-prover).
//!
//! The prover's input, Google's token, isn't secret, so proving needs no
//! enclave. It runs the guest through `r0vm` (RISC Zero's executor, installed
//! by `rzup`):
//! - [`Mode::Dev`]: a dev-mode receipt with no real seal, for tests and local
//!   work; only [`botstadia_attestation_core::receipt::mock_seal`] makes it
//!   acceptable, to the local mock verifier alone;
//! - [`Mode::Groth16`]: a real Groth16 receipt (the STARK wrapped by RISC
//!   Zero's Docker image), for the testnet and prod prover.

use anyhow::{Context, Result, ensure};
use botstadia_attestation_core::{Input, Journal};
use botstadia_attestation_methods::{
    BOTSTADIA_ATTESTATION_GUEST_ELF, BOTSTADIA_ATTESTATION_GUEST_ID,
};
use risc0_zkvm::{ExecutorEnv, ProverOpts, default_prover};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dev,
    Groth16,
}

/// A proven run of the guest.
#[derive(Debug, Clone)]
pub struct Proof {
    /// The journal's bytes, as the results contract decodes them.
    pub journal: Vec<u8>,
    /// The seal as the EVM verifier takes it: the verifier's 4-byte selector,
    /// then the Groth16 seal. Empty in dev mode.
    pub seal: Vec<u8>,
    /// The cycles the guest ran for.
    pub cycles: u64,
}

/// The guest's image id as bytes, as the results contract names it. Zero
/// when the guest wasn't built (see the methods crate).
pub fn image_id() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, word) in BOTSTADIA_ATTESTATION_GUEST_ID.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

pub fn guest_built() -> bool {
    !BOTSTADIA_ATTESTATION_GUEST_ELF.is_empty()
}

/// Prove the guest over `input`.
pub fn prove(input: &Input, mode: Mode) -> Result<Proof> {
    ensure!(
        guest_built(),
        "the guest isn't built: set BOTSTADIA_ATTESTATION_BUILD_GUEST (see the methods crate)"
    );
    prove_elf(
        BOTSTADIA_ATTESTATION_GUEST_ELF,
        BOTSTADIA_ATTESTATION_GUEST_ID,
        input,
        mode,
    )
}

/// The image id of a guest ELF, as the results contract names it.
pub fn image_id_of(elf: &[u8]) -> Result<[u8; 32]> {
    let digest = risc0_zkvm::compute_image_id(elf).context("computing the guest's image id")?;
    Ok(digest.as_bytes().try_into().expect("a 32-byte digest"))
}

/// Prove a guest given as an ELF (the prover's image carries the guest
/// built reproducibly, rather than building it), whose image id is `id`.
pub fn prove_elf(elf: &[u8], id: [u32; 8], input: &Input, mode: Mode) -> Result<Proof> {
    let env = ExecutorEnv::builder()
        .write(input)
        .context("writing the guest's input")?
        .build()
        .context("building the executor environment")?;
    let (opts, dev) = match mode {
        Mode::Dev => (ProverOpts::default().with_dev_mode(true), true),
        Mode::Groth16 => (ProverOpts::groth16(), false),
    };
    let info = default_prover()
        .prove_with_opts(env, elf, &opts)
        .context("proving the attestation guest")?;
    let receipt = info.receipt;
    let journal = receipt.journal.bytes.clone();
    ensure!(
        Journal::decode(&journal).is_some(),
        "the guest committed something other than a journal"
    );
    let seal = if dev {
        Vec::new()
    } else {
        receipt.verify(id).context("the receipt verifies")?;
        let groth16 = receipt.inner.groth16().context("a Groth16 receipt")?;
        let mut seal = groth16.verifier_parameters.as_bytes()[..4].to_vec();
        seal.extend_from_slice(&groth16.seal);
        seal
    };
    Ok(Proof {
        journal,
        seal,
        cycles: info.stats.total_cycles,
    })
}
