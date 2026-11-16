# syntax=docker/dockerfile:1.10@sha256:865e5dd094beca432e8c0a1d5e1c465db5f998dca4e439981029b3b81fb39ed5
# The policy shell (BOTS.md#the-policy-shells-onnx-contract-version-1),
# built reproducibly from the repository root:
#
#   docker buildx build --platform linux/amd64 --output type=local,dest=out \
#     -f packages/shell/Dockerfile - < <git archive of the commit>
#
# out/policy-shell.wasm's sha256 is the shell's digest. The wasm is the same
# whatever machine builds it: the compiler is pinned (the image by digest,
# the toolchain by rust-toolchain.toml), every dependency is Cargo.lock's,
# the compile runs with no network, and the only paths it can embed are
# remapped to fixed ones (/platform for the source, /home/.cargo for the
# registry), which are also what a build outside Docker remaps them to.
ARG RUST_IMAGE=rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157

FROM --platform=linux/amd64 ${RUST_IMAGE} AS build
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain || rustup toolchain install
COPY . .
RUN cargo fetch --locked
RUN --network=none \
    SOURCE_DATE_EPOCH=0 \
    RUSTFLAGS="--remap-path-prefix=/src=/platform --remap-path-prefix=/usr/local/cargo=/home/.cargo" \
      cargo build --release --locked --offline --target wasm32-wasip2 -p botstadia-policy-shell && \
    mkdir /out && cp target/wasm32-wasip2/release/botstadia_policy_shell.wasm /out/policy-shell.wasm

FROM scratch
COPY --from=build /out/ /
