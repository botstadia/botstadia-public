//! Proving the guest. These need the guest built
//! (`BOTSTADIA_ATTESTATION_BUILD_GUEST=1`, or `task attestation:prove-dev`)
//! and RISC Zero's `r0vm`, so they skip when it isn't.

use botstadia_attestation_core::{Journal, fixture, verify};
use botstadia_attestation_host::{Mode, guest_built, image_id, prove};

const IAT: u64 = 1_790_000_000;

#[test]
fn the_dev_mode_guest_commits_the_journal_verify_returns() {
    if !guest_built() {
        eprintln!("the guest isn't built: skipping");
        return;
    }
    let input = fixture::chain::input(&fixture::binding(), IAT, IAT + 60);
    let expected = verify(&input).unwrap();
    let proof = prove(&input, Mode::Dev).unwrap();
    assert_eq!(Journal::decode(&proof.journal), Some(expected));
    assert!(proof.seal.is_empty(), "dev mode has no seal");
    assert_ne!(image_id(), [0u8; 32]);
    eprintln!(
        "guest image id 0x{}, {} cycles",
        hex::encode(image_id()),
        proof.cycles
    );
}

#[test]
fn the_guest_refuses_a_token_that_fails_a_check() {
    if !guest_built() {
        return;
    }
    let mut input = fixture::chain::input(&fixture::binding(), IAT, IAT + 60);
    input.binding.chain_id = 8453;
    assert!(
        prove(&input, Mode::Dev).is_err(),
        "a nonce for another binding never proves"
    );
}

/// The attestation core's journal and the chain SDK's are the same bytes, so
/// what the guest commits is what the results contract decodes (the chain
/// SDK's Anvil test proves the contract decodes the SDK's).
#[test]
fn the_guest_s_journal_is_the_chain_sdk_s_journal() {
    let input = fixture::chain::input(&fixture::binding(), IAT, IAT + 60);
    let j = verify(&input).unwrap();
    let sdk = botstadia_chain::journal::Journal {
        image_digest: j.image_digest.into(),
        project_id: j.project_id.into(),
        submit_key: j.submit_key.into(),
        bundle_key: j.bundle_key.into(),
        chain_id: j.chain_id,
        results: j.results.into(),
        instruction: j.instruction,
        issued_at: j.issued_at,
        root_hash: j.root_hash.into(),
    };
    assert_eq!(sdk.encode(), j.encode().to_vec());
}

/// One real Groth16 proof of the guest (the STARK wrapped by RISC Zero's
/// Docker image), written as a Foundry fixture for the real verifier.
/// Ignored: run by hand once, with Docker (`task attestation:groth16`).
#[test]
#[ignore = "needs Docker and RISC Zero's Groth16 image; minutes to hours"]
fn a_real_groth16_proof_becomes_a_foundry_fixture() {
    assert!(
        guest_built(),
        "build the guest with BOTSTADIA_ATTESTATION_BUILD_GUEST"
    );
    let input = fixture::chain::input(&fixture::binding(), IAT, IAT + 60);
    let proof = prove(&input, Mode::Groth16).unwrap();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../contracts/test/fixtures/guest-groth16.json");
    std::fs::write(
        &out,
        format!(
            "{{\n  \"image_id\": \"0x{}\",\n  \"journal\": \"0x{}\",\n  \"seal\": \"0x{}\"\n}}\n",
            hex::encode(image_id()),
            hex::encode(&proof.journal),
            hex::encode(&proof.seal)
        ),
    )
    .unwrap();
}
