# BotStadia's public source

[BotStadia](https://botstadia.com) runs robot sessions inside enclaves and settles bets on their results on Base. This repository holds exactly the code that decides an outcome or holds a key, so anyone can rebuild it and check it against what's on-chain:

- `platform/`: the enclave image (the session runner, `packages/enclave`, and every crate it links), the zk guest that proves an enclave's identity (`packages/attestation`), the policy shell that runs trained bots (`packages/shell`), the exit page that moves an account wallet without us (`packages/exit-page`), and every contract we deploy, with its tests (`contracts`).
- `arenas/`: every arena's engine, the physics and rules a session runs (`catalog/<arena>`), with the shared crates it links.

Nothing else of BotStadia is here, and nothing here is developed here: each directory is exported from a private repository by `tools/export.py`, whose file list is the boundary. The standalone upload tool has a repository of its own, [botstadia-upload-tool](https://github.com/botstadia/botstadia-upload-tool).

## Tags

Each build that matters is tagged by its digest, by BotStadia's release task and nobody else:

| Tag | What | Checked against |
|---|---|---|
| `image-<target>/sha256-<hex>` | The enclave image's manifest digest (`testnet` or `prod`) | The results contract's `imageApproved(bytes32)`, after a 2-day public timelock |
| `guest/0x<hex>` | The zk guest's image id | The guest image id an approved verifier is paired with (`verifiers(bytes32)`) |
| `arena/<arena>-<version>/sha256-<hex>` | An arena engine's digest | Every session's spec, posted on-chain before betting opens |
| `shell/sha256-<hex>` | The policy shell's digest | A trained bot's upload report, and the CDN's `policy-shell/` |
| `exit-page-<target>/sha256-<hex>` | The exit page's file | The file in your recovery download, and the release asset of the same name |

Our own bots (house bots and baselines) are assets of the `bots` release, one zip per digest, the same files the CDN serves under `bot/catalog/`.

## Rebuilding a tag

You need git and Docker with buildx. The guest also needs Rust (`rustup` installs the pinned toolchain itself) and RISC Zero's [rzup](https://dev.risczero.com/api/zkvm/install) with its Rust toolchain (`rzup install rust`): the guest compiles in RISC Zero's pinned Docker image, but its build looks for that toolchain, and any version past 1.81 gives the same guest.

```sh
git clone https://github.com/botstadia/botstadia-public && cd botstadia-public
./verify.sh --list
./verify.sh image-testnet/sha256-…
```

`verify.sh` builds from the tagged commit alone, in Docker images pinned by digest, with no network during the compile, and fails unless the digest matches the tag. The same check runs on GitHub: Actions, "Verify a tag", Run workflow (in a fork if you like).

What makes each build reproducible is written at the top of its Dockerfile: `platform/packages/enclave/image/Dockerfile`, `platform/packages/shell/Dockerfile`, `platform/packages/exit-page/Dockerfile` and `arenas/Dockerfile`. The guest is built by RISC Zero's own pinned Docker builder (`platform/packages/attestation/methods/build.rs`). An image's `SOURCE_DATE_EPOCH` is its tag's commit time.

To see what changed between two approved images, diff their tags: `git diff image-testnet/sha256-<old> image-testnet/sha256-<new> -- platform`.

## Checking against the chain

`platform/config.yml` lists our contracts' addresses per target, the GCP project the guest pins, Google's root and the guest image id. The contracts' sources are also verified on the block explorer.

The contracts' tests run with [Foundry](https://getfoundry.sh) 1.2.3:

```sh
git submodule update --init --recursive
cd platform/contracts && forge test
```

The Rust crates' tests run with `cargo test --workspace` in `platform/` (some need ONNX Runtime, which `ort` downloads) and in `arenas/`.

## Licence

The Business Source License 1.1 ([LICENSE](LICENSE)): read, build and run it to check our builds and results, or to build and test bots for BotStadia. Each version becomes Apache 2.0 four years after it's published, or on 2030-11-16, whichever is first. Third-party code keeps its own licence, beside it.
