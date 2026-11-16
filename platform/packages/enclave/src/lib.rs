//! The session runner (TRUST.md, BOTS.md): the one program in every enclave
//! image, run locally as a native process with mock attestation.
//!
//! - [`settings`]: the chain binding and everything else from the launch
//!   environment, fixed for the life of the boot.
//! - [`attest`]: attestation tokens, from Confidential Space's launcher in
//!   an image or the mock CA locally.
//! - [`client`]: the api as a courier, every request signed with the
//!   SubmitKey over a fresh nonce.

#[cfg(all(feature = "mock", botstadia_image))]
compile_error!("mock attestation can never be built into an enclave image");

#[cfg(all(feature = "mock", any(botstadia_testnet, botstadia_devnet)))]
compile_error!("the testnet's and the devnet's builds are enclave images: no mock attestation");

pub mod anchor;
pub mod attest;
pub mod batch;
pub mod boot;
pub mod bundle_key;
pub mod canary;
pub mod cdn;
pub mod chain_view;
pub mod client;
pub mod constants;
pub mod executor;
pub mod gate;
pub mod gce;
pub mod hold;
pub mod jobs;
pub mod keyholder;
pub mod play;
pub mod runner;
pub mod settings;
pub mod submit;
#[cfg(feature = "testing")]
pub mod testkit;
pub mod warmup;
pub mod worker;

/// `sha256-<hex>` of some bytes, as bundles and arenas are named.
pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256-{}", hex::encode(Sha256::digest(bytes)))
}
