# syntax=docker/dockerfile:1.10@sha256:865e5dd094beca432e8c0a1d5e1c465db5f998dca4e439981029b3b81fb39ed5
# One arena's engine, built reproducibly from the repository root
# (DELIVERY.md, "Reproducible build"):
#
#   docker buildx build --platform linux/amd64 --build-arg ARENA=<slug> \
#     --output type=local,dest=out -f Dockerfile - < <git archive of the commit>
#
# out/arena.wasm's sha256 is the arena's digest, the one every session's spec
# names. The same source gives the same engine on any machine: the compilers
# are pinned (the Rust image by digest, the toolchain by rust-toolchain.toml,
# wasi-sdk by checksum), every dependency is Cargo.lock's, the compile runs
# with no network, and no path of the build reaches the wasm (rustc's are
# remapped, the C build's mapped in shared/mujoco-sys/build.rs).
ARG RUST_IMAGE=rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157

FROM --platform=linux/amd64 ${RUST_IMAGE} AS build
ADD --checksum=sha256:0ba8b5bfaeb2adf3f29bab5841d76cf5318ab8e1642ea195f88baba1abd47bce \
    https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-33/wasi-sdk-33.0-x86_64-linux.tar.gz /tmp/wasi-sdk.tgz
RUN mkdir -p /opt/wasi-sdk && tar -xzf /tmp/wasi-sdk.tgz -C /opt/wasi-sdk --strip-components=1 --no-same-owner
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain || rustup toolchain install
COPY . .
RUN cargo fetch --locked
ARG ARENA
RUN --network=none \
    : "${ARENA:?pass --build-arg ARENA=<slug>}" && \
    SOURCE_DATE_EPOCH=0 WASI_SDK_PATH=/opt/wasi-sdk \
    RUSTFLAGS="--remap-path-prefix=/src=/arenas --remap-path-prefix=/usr/local/cargo=/home/.cargo" \
      cargo build --release --locked --offline --target wasm32-wasip2 -p "$ARENA-component" && \
    mkdir /out && cp "target/wasm32-wasip2/release/$(echo "$ARENA" | tr '-' '_')_component.wasm" /out/arena.wasm

FROM scratch
COPY --from=build /out/ /
