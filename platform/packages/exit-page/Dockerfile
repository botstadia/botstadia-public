# syntax=docker/dockerfile:1.10@sha256:865e5dd094beca432e8c0a1d5e1c465db5f998dca4e439981029b3b81fb39ed5
# The exit page (TRUST.md#a-way-out-without-us), one self-contained HTML file,
# built reproducibly for a target from the repository root:
#
#   docker buildx build --platform linux/amd64 --build-arg TARGET=testnet \
#     --output type=local,dest=out -f packages/exit-page/Dockerfile - < <git archive of the commit>
#
# out/exit-page.html's sha256 is the page's published hash. The target's
# chain defaults (its wallet contracts, where our settlement contract's
# events start, a public RPC and bundlers) come from config.yml, baked in.
# The compiler and wasm-bindgen are pinned (images by digest, the toolchain
# by rust-toolchain.toml, wasm-bindgen by Cargo.lock), the compile runs with
# no network, and paths are remapped to fixed ones.
ARG RUST_IMAGE=rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157
ARG PYTHON_IMAGE=python:3.13-slim-bookworm@sha256:a5a7017efc5969e00ae2b91c8eb0631049a569652aa4ffc4501769049ae45c72

FROM --platform=linux/amd64 ${PYTHON_IMAGE} AS chains
ARG TARGET
COPY config.yml packages/exit-page/chains.py /in/
RUN : "${TARGET:?pass --build-arg TARGET=testnet or prod}" && \
    python3 /in/chains.py /in/config.yml "$TARGET" > /chains.json

FROM --platform=linux/amd64 ${RUST_IMAGE} AS build
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain || rustup toolchain install
COPY Cargo.lock ./
RUN version=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock) && \
    cargo install wasm-bindgen-cli --version "$version" --locked --root /opt/wasm-bindgen
COPY . .
RUN cargo fetch --locked
COPY --from=chains /chains.json /chains.json
RUN --network=none \
    SOURCE_DATE_EPOCH=0 \
    EXIT_PAGE_CHAINS="$(cat /chains.json)" \
    RUSTFLAGS="--remap-path-prefix=/src=/platform --remap-path-prefix=/usr/local/cargo=/home/.cargo" \
      cargo build --locked --offline --profile frontend --target wasm32-unknown-unknown -p botstadia-exit-page && \
    /opt/wasm-bindgen/bin/wasm-bindgen --target web --no-typescript --out-dir /bindgen \
      target/wasm32-unknown-unknown/frontend/botstadia_exit_page.wasm

FROM --platform=linux/amd64 ${PYTHON_IMAGE} AS page
COPY packages/exit-page/index.html packages/exit-page/inline.py /in/
COPY --from=build /bindgen /bindgen
RUN mkdir /out && python3 /in/inline.py /in/index.html /bindgen/botstadia_exit_page.js \
      /bindgen/botstadia_exit_page_bg.wasm /out/exit-page.html

FROM scratch
COPY --from=page /out/ /
