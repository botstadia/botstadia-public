//! Builds the attestation guest for the zkVM.
//!
//! Building it needs RISC Zero's toolchain (`rzup`), so it is opt-in:
//! - `BOTSTADIA_ATTESTATION_BUILD_GUEST=1` builds it with the local toolchain,
//!   for dev-mode proving and tests;
//! - `BOTSTADIA_ATTESTATION_BUILD_GUEST=docker` builds it inside RISC Zero's
//!   pinned guest-builder image, so its image id is the same on every machine
//!   and anyone can rebuild the id a verifier is approved for
//!   (TRUST.md#approving-builds).
//!
//! Without it, the crate holds an empty ELF and a zero image id, which is all
//! a build that never proves needs (every ordinary build, and CI).

use std::collections::HashMap;

use risc0_build::{DockerOptionsBuilder, GuestOptionsBuilder};

fn main() {
    println!("cargo:rerun-if-env-changed=BOTSTADIA_ATTESTATION_BUILD_GUEST");
    let mode = std::env::var("BOTSTADIA_ATTESTATION_BUILD_GUEST").unwrap_or_default();
    if mode.is_empty() {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        std::fs::write(
            out.join("methods.rs"),
            "pub const BOTSTADIA_ATTESTATION_GUEST_ELF: &[u8] = &[];\n\
             pub const BOTSTADIA_ATTESTATION_GUEST_ID: [u32; 8] = [0; 8];\n\
             pub const BOTSTADIA_ATTESTATION_GUEST_PATH: &str = \"\";\n",
        )
        .expect("writing the empty guest");
        return;
    }
    let guest = if mode == "docker" {
        // The build context is the attestation directory, so the guest's path
        // dependency on the core crate is inside it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let docker = DockerOptionsBuilder::default()
            .root_dir(root)
            // Pinned: the builder image is part of what the image id depends on.
            .docker_container_tag("r0.1.91.1")
            .build()
            .expect("docker options");
        GuestOptionsBuilder::default()
            .use_docker(docker)
            .build()
            .expect("guest options")
    } else {
        GuestOptionsBuilder::default()
            .build()
            .expect("guest options")
    };
    risc0_build::embed_methods_with_options(HashMap::from([(
        "botstadia-attestation-guest",
        guest,
    )]));
}
