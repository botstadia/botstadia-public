//! A local enclave's approval, end to end on Anvil: a mock token, checked by
//! the same `verify` the guest runs, its journal committed through RISC
//! Zero's mock verifier, approved by the results contract
//! (`task chain:test-sdk` deploys, runs this and stops Anvil).

use alloy_sol_types::SolCall;
use botstadia_attestation_core::{Binding, Input, mock, receipt, verify};
use botstadia_chain::abi::Results;
use botstadia_chain::rpc::{BlockTag, Client, Http};
use botstadia_chain::sign::Key;
use botstadia_chain::{Address, B256, Bytes, U256, keccak256, tx};
use serde_json::json;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is set by `task chain:test-sdk`"))
}

#[test]
fn the_config_s_local_anchors_are_the_mock_module_s() {
    let Ok(root) = std::env::var("BOTSTADIA_LOCAL_ROOT_HASH") else {
        return;
    };
    assert_eq!(root, format!("0x{}", hex::encode(mock::root_hash())));
    assert_eq!(
        env("BOTSTADIA_LOCAL_IMAGE_DIGEST"),
        format!("0x{}", mock::IMAGE_DIGEST.trim_start_matches("sha256:"))
    );
    assert_eq!(env("BOTSTADIA_PROJECT_ID"), mock::PROJECT);
}

#[tokio::test]
#[ignore = "needs the local chain: run `task chain:test-sdk`"]
async fn a_mock_attested_key_is_approved_by_the_local_results_contract() {
    let rpc = Client::new(Http::new(env("BOTSTADIA_ANVIL_URL")));
    let chain_id = rpc.chain_id().await.unwrap();
    let results: Address = env("BOTSTADIA_RESULTS").parse().unwrap();
    let mock_verifier: Address = env("BOTSTADIA_MOCK_VERIFIER").parse().unwrap();
    let guest: B256 = env("BOTSTADIA_LOCAL_GUEST_IMAGE_ID").parse().unwrap();
    let selector: [u8; 4] = env("BOTSTADIA_MOCK_SELECTOR").parse::<B256>().unwrap()[..4]
        .try_into()
        .unwrap();

    let enclave = Key::from_bytes(&keccak256("host-test-enclave").0).unwrap();
    rpc.request(
        "anvil_setBalance",
        json!([enclave.address(), "0x56BC75E2D63100000"]),
    )
    .await
    .unwrap();
    // Genesis may already be filled (the chain SDK's test runs first): a
    // later holder carries the BundleKey the chain holds for it.
    let held = rpc
        .call_typed(results, &Results::instructionsCall(1), BlockTag::Latest)
        .await
        .unwrap()
        .bundleKey;
    let bundle_key = if held == B256::ZERO {
        keccak256("host-test-bundle-key")
    } else {
        held
    };
    let binding = Binding {
        submit_key: enclave.address().0.0,
        bundle_key: bundle_key.0,
        chain_id,
        results: results.0.0,
        instruction: 1,
    };
    let now = rpc.block(BlockTag::Latest).await.unwrap().timestamp;
    let journal = verify(&Input {
        token: mock::issue(&[binding.nonce()], now),
        keys: Vec::new(),
        now: now + 1,
        binding,
    })
    .unwrap();
    let seal = receipt::mock_seal(&selector, &guest.0, &journal.digest());

    let verifier = rpc
        .call_typed(
            results,
            &Results::verifierIdCall {
                verifier: mock_verifier,
                guestImageId: guest,
            },
            BlockTag::Latest,
        )
        .await
        .unwrap();
    let data = Results::approveKeyCall {
        verifier,
        seal: Bytes::from(seal),
        journal: Bytes::from(journal.encode().to_vec()),
    }
    .abi_encode();
    let gas = rpc
        .estimate_gas(enclave.address(), results, &data)
        .await
        .unwrap();
    let price = rpc.gas_price().await.unwrap();
    let nonce = rpc
        .nonce(enclave.address(), BlockTag::Latest)
        .await
        .unwrap();
    let signed = tx::sign(
        &enclave,
        chain_id,
        nonce,
        gas * 2,
        tx::Fees {
            max_fee_per_gas: price * 2,
            max_priority_fee_per_gas: price,
        },
        &tx::Call {
            to: results,
            data,
            value: U256::ZERO,
        },
    )
    .unwrap();
    rpc.send_raw(&signed.raw).await.unwrap();
    let mut receipt = None;
    for _ in 0..50 {
        receipt = rpc.receipt(signed.hash).await.unwrap();
        if receipt.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let receipt = receipt.expect("Anvil mines within a second");
    assert!(receipt.succeeded());
    let active = rpc
        .call_typed(
            results,
            &Results::keyActiveCall {
                key: enclave.address(),
            },
            BlockTag::Latest,
        )
        .await
        .unwrap();
    assert!(active, "a mock-attested key is approved on the local chain");
}
