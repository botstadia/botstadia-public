#!/bin/sh
# Rebuild what a tag names from this repository alone, and check its digest.
#
#   ./verify.sh <tag>                  e.g. ./verify.sh image-testnet/sha256-83f9…
#   ./verify.sh --list                 every tag
#   ./verify.sh --digest <kind> [arg]  build HEAD and print its digest (what
#                                      BotStadia's release task tags):
#                                      image <target>, guest, shell,
#                                      exit-page <target>, arena <slug>
#
# Needs git and Docker (with buildx). The guest also needs Rust (rustup
# installs the pinned toolchain from rust-toolchain.toml by itself) and RISC
# Zero's rzup with its Rust toolchain (`rzup install rust`): risc0-build looks
# for it, though the guest compiles in RISC Zero's pinned Docker image, and
# any version past 1.81 builds the same guest.
#
# Tags, one per digest, written only by BotStadia's release task:
#   image-<target>/sha256-<hex>    the enclave image (platform/), whose manifest
#                                  digest the results contract approves
#   guest/0x<hex>                  the zk guest's image id, which an approved
#                                  verifier is paired with
#   arena/<slug>-<version>/sha256-<hex>   an arena's engine (arenas/), the
#                                  digest every session's spec names
#   shell/sha256-<hex>             the policy shell (platform/)
#   exit-page-<target>/sha256-<hex>  the exit page's file (platform/)
set -eu

die() { echo "✗ $*" >&2; exit 1; }

# One BuildKit, pinned: the default driver can't write OCI archives or
# rewrite layer timestamps, and the image digest depends on both.
# BUILDER names another (BotStadia's release task shares its own's cache).
B=${BUILDER:-botstadia-verify}
builder() {
  docker buildx inspect "$B" >/dev/null 2>&1 ||
    docker buildx create --name "$B" --driver docker-container \
      --driver-opt image=moby/buildkit:v0.34.2@sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe \
      --bootstrap >/dev/null
}

sha() { (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

# build <commit> <kind> [arg]: print the digest the commit builds to. Build
# output goes to stderr; OUT, if set, receives the built file.
build() {
  commit=$1 kind=$2 arg=${3:-}
  work=$(mktemp -d)
  # A subdirectory of the commit, exactly as committed, as a tar stream.
  tree() { git archive --format=tar "$commit:$1"; }
  case "$kind" in
    image)
      [ -n "$arg" ] || die "image needs a target (testnet or prod)"
      builder
      epoch=$(git log -1 --format=%ct "$commit")
      tree platform | docker buildx build --builder "$B" \
        --platform linux/amd64 \
        --build-arg TARGET="$arg" --build-arg SOURCE_DATE_EPOCH="$epoch" \
        --provenance=false --sbom=false \
        --output "type=oci,dest=$work/image.tar,rewrite-timestamp=true" \
        --metadata-file "$work/meta.json" \
        -f packages/enclave/image/Dockerfile - >&2
      digest=$(sed -n 's/.*"containerimage.digest": *"\([^"]*\)".*/\1/p' "$work/meta.json")
      [ -z "${OUT:-}" ] || cp "$work/image.tar" "$OUT"
      ;;
    guest)
      command -v rzup >/dev/null || [ -x "$HOME/.risc0/bin/rzup" ] ||
        die "the guest's build needs RISC Zero's rzup and its Rust toolchain: https://dev.risczero.com/api/zkvm/install, then rzup install rust"
      mkdir "$work/src"
      tree platform | tar -x -C "$work/src"
      digest=$(cd "$work/src" && BOTSTADIA_ATTESTATION_BUILD_GUEST=docker \
        cargo run --release -q --locked -p botstadia-attestation-host --example trust_anchors |
        sed -n 's/^guest_image_id //p')
      ;;
    shell)
      builder
      tree platform | docker buildx build --builder "$B" --platform linux/amd64 \
        --output "type=local,dest=$work/out" -f packages/shell/Dockerfile - >&2
      digest=sha256-$(sha "$work/out/policy-shell.wasm")
      [ -z "${OUT:-}" ] || cp "$work/out/policy-shell.wasm" "$OUT"
      ;;
    exit-page)
      [ -n "$arg" ] || die "exit-page needs a target (testnet or prod)"
      builder
      tree platform | docker buildx build --builder "$B" --platform linux/amd64 \
        --build-arg TARGET="$arg" \
        --output "type=local,dest=$work/out" -f packages/exit-page/Dockerfile - >&2
      digest=sha256-$(sha "$work/out/exit-page.html")
      [ -z "${OUT:-}" ] || cp "$work/out/exit-page.html" "$OUT"
      ;;
    arena)
      [ -n "$arg" ] || die "arena needs its slug"
      git cat-file -e "$commit:arenas/Dockerfile" 2>/dev/null ||
        die "this engine was released before engines were built in Docker, so it can't be rebuilt here: its tag's message says why (git tag -n9 <tag>)"
      builder
      tree arenas | docker buildx build --builder "$B" --platform linux/amd64 \
        --build-arg ARENA="$arg" \
        --output "type=local,dest=$work/out" -f Dockerfile - >&2
      digest=sha256-$(sha "$work/out/arena.wasm")
      [ -z "${OUT:-}" ] || cp "$work/out/arena.wasm" "$OUT"
      ;;
    *) die "unknown kind $kind" ;;
  esac
  rm -rf "$work"
  [ -n "$digest" ] || die "the $kind build gave no digest"
  echo "$digest"
}

case "${1:-}" in
  --list)
    git tag -l | sort
    ;;
  --digest)
    shift
    [ $# -ge 1 ] || die "usage: $0 --digest <kind> [arg]"
    build "$(git rev-parse HEAD)" "$@"
    ;;
  ""|-*)
    die "usage: $0 <tag> | --list | --digest <kind> [arg]"
    ;;
  *)
    tag=$1
    git rev-parse -q --verify "refs/tags/$tag" >/dev/null || die "no tag $tag (git fetch --tags)"
    commit=$(git rev-parse "$tag^{commit}")
    case "$tag" in
      image-*/sha256-*) t=${tag#image-}; want=sha256:${tag##*/sha256-}; got=$(build "$commit" image "${t%%/*}") ;;
      guest/0x*) want=${tag#guest/}; got=$(build "$commit" guest) ;;
      shell/sha256-*) want=${tag#shell/}; got=$(build "$commit" shell) ;;
      exit-page-*/sha256-*) t=${tag#exit-page-}; want=${tag##*/}; got=$(build "$commit" exit-page "${t%%/*}") ;;
      arena/*/sha256-*) n=${tag#arena/}; n=${n%%/*}; want=${tag##*/}; got=$(build "$commit" arena "${n%-*}") ;;
      *) die "not a tag this script rebuilds: $tag" ;;
    esac
    [ "$want" = "$got" ] || die "$tag rebuilt to $got"
    echo "✅ $tag rebuilds to $got"
    ;;
esac
