//! Prints what a results contract is proposed to trust for this guest and
//! Google's root: the guest's image id (build with
//! `BOTSTADIA_ATTESTATION_BUILD_GUEST=docker` for the reproducible one) and
//! the Confidential Space root's hash. `task chain:trust-anchors` runs it.

fn main() {
    let id = botstadia_attestation_host::image_id();
    if id == [0; 32] {
        eprintln!("the guest wasn't built: set BOTSTADIA_ATTESTATION_BUILD_GUEST=docker");
        std::process::exit(1);
    }
    println!("guest_image_id 0x{}", hex::encode(id));
    println!(
        "google_root_hash 0x{}",
        hex::encode(botstadia_attestation_core::confidential_space_root_hash())
    );
}
