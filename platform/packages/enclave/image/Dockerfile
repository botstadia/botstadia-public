# syntax=docker/dockerfile:1.10@sha256:865e5dd094beca432e8c0a1d5e1c465db5f998dca4e439981029b3b81fb39ed5
# The enclave image (TRUST.md#approving-builds): `botstadia-runner` alone,
# on Confidential Space, which attests to this image by its digest. Built
# from the repository root by `task enclave:image` (TARGET=testnet or
# prod), which is the only supported way to build it:
#
#   docker buildx build --platform linux/amd64 --build-arg TARGET=testnet \
#     --build-arg SOURCE_DATE_EPOCH=<last commit time> \
#     --output type=oci,dest=image.tar,rewrite-timestamp=true \
#     -f packages/enclave/image/Dockerfile - < <git archive of the commit>
#
# Reproducible: anyone rebuilding this commit gets the same digest.
# - Every base is pinned by digest, the toolchain by rust-toolchain.toml.
# - Network is used only to install that toolchain and vendor exactly
#   Cargo.lock; the compile runs with no network, `--locked --offline`.
# - Paths are fixed (/src, /usr/local/cargo), so nothing machine-specific
#   reaches the binary, and every file's time is SOURCE_DATE_EPOCH.
# - The builder is always linux/amd64, so a laptop's build (emulated) and
#   Cloud Build's (native) run the same compilers.
#
# Everything it runs with is in the image. The launch policy (the labels at
# the end) lets a launch set only the chain binding and the api's URL
# (settings::IMAGE_LAUNCH_VARS), never the command, and sends its log lines
# nowhere outside a debug image. The RPC providers' keys come from the api
# once the enclave is registered, so no attestation token carries them.

ARG RUST_IMAGE=rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157
ARG RUN_IMAGE=gcr.io/distroless/cc-debian12@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa

# The toolchain rust-toolchain.toml pins, and every dependency's source.
FROM --platform=linux/amd64 ${RUST_IMAGE} AS vendor
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain || rustup toolchain install
COPY . .
RUN mkdir -p .cargo && cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml

# ONNX Runtime, the inference host's library: Microsoft's own release,
# checked against .github/onnxruntime.sha256's hash, linked dynamically.
ADD --checksum=sha256:a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407 \
    https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-linux-x64-1.28.0.tgz /tmp/onnxruntime.tgz
RUN mkdir -p /opt/onnxruntime && \
    tar -xzf /tmp/onnxruntime.tgz -C /opt/onnxruntime --strip-components=1 --no-same-owner && \
    ln -sf libonnxruntime.so.1 /opt/onnxruntime/lib/libonnxruntime.so

# The runner, compiled with no network.
FROM vendor AS build
# testnet: the fast testnet's image (`--cfg botstadia_testnet`, shortened
# waits, serves Base Sepolia); prod: prod's.
ARG TARGET
ARG SOURCE_DATE_EPOCH
# No LTO and one codegen unit: ThinLTO's promoted symbols carry hashes
# that differed between two clean builds of one commit, so the image's
# binary is built the plain, deterministic way. It changes only how the
# runner itself is optimised, never what a bot is charged.
ENV ORT_LIB_LOCATION=/opt/onnxruntime/lib \
    ORT_PREFER_DYNAMIC_LINK=1 \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_LTO=off \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}
RUN --network=none \
    : "${TARGET:?pass --build-arg TARGET=testnet or prod}" && \
    case "$TARGET" in \
      testnet) cfg="--cfg botstadia_image --cfg botstadia_testnet" ;; \
      prod) cfg="--cfg botstadia_image" ;; \
      *) echo "TARGET is testnet or prod" >&2; exit 1 ;; \
    esac && \
    RUSTFLAGS="$cfg -C link-arg=-Wl,--build-id=none --remap-path-prefix=/src=. --remap-path-prefix=/usr/local/cargo=cargo" \
      cargo build --release --locked --offline -p botstadia-enclave --bin botstadia-runner && \
    root=/out/root && lib="$root/usr/lib/x86_64-linux-gnu" && \
    mkdir -p "$root/usr/local/bin" "$lib" && \
    cp target/release/botstadia-runner "$root/usr/local/bin/" && \
    cp /opt/onnxruntime/lib/libonnxruntime.so.1.28.0 "$lib/" && \
    ln -s libonnxruntime.so.1.28.0 "$lib/libonnxruntime.so.1" && \
    find "$root" -exec touch -h -d "@${SOURCE_DATE_EPOCH}" {} +

FROM --platform=linux/amd64 ${RUN_IMAGE}
ARG TARGET
# One layer, every file and directory in it stamped SOURCE_DATE_EPOCH.
COPY --from=build /out/root/ /
LABEL "tee.launch_policy.allow_env_override"="BOTSTADIA_API_URL,BOTSTADIA_CHAIN_ID,BOTSTADIA_RESULTS" \
      "tee.launch_policy.allow_cmd_override"="false" \
      "tee.launch_policy.log_redirect"="debugonly" \
      "com.botstadia.target"="${TARGET}"
# Root: the launcher's token socket is root's, and the runner is the
# machine's only workload. Seats drop to their own namespaces and filters.
ENTRYPOINT ["/usr/local/bin/botstadia-runner"]
