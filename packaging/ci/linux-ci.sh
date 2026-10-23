#!/usr/bin/env bash
# Ninimma modification: independent project packaging and branding.
# Linux CI inside Ubuntu 22.04, so the runner host needs only Docker (no
# sudo) and results match the release build's environment.
#
#   packaging/ci/linux-ci.sh check   formatting, the disk helper's tests, clippy
#   packaging/ci/linux-ci.sh build   tests, release build, .deb/.rpm packaging
#
# It starts the container itself (against the system Docker daemon, not a
# Docker Desktop context that may not be running); files it leaves in the
# checkout are handed back to the calling user.
set -euo pipefail

mode=${1:-}
[[ $mode == check || $mode == build ]] || { echo "usage: $0 check|build" >&2; exit 1; }

if [[ ${IN_CI_CONTAINER:-} != 1 ]]; then
    export DOCKER_HOST=${DOCKER_HOST:-unix:///var/run/docker.sock}
    docker info >/dev/null || { echo "Docker isn't reachable at $DOCKER_HOST." >&2; exit 1; }
    exec docker run --rm \
        -v "$PWD:/src" -w /src \
        -e IN_CI_CONTAINER=1 -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" -e CARGO_TERM_COLOR \
        ubuntu:22.04 bash packaging/ci/linux-ci.sh "$mode"
fi

# --- Inside the container, as root ---
trap 'chown -R "$HOST_UID:$HOST_GID" /src/target 2>/dev/null || true' EXIT

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    ca-certificates curl git build-essential pkg-config cmake clang libclang-dev \
    libssl-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev \
    libwayland-dev python3 rpm >/dev/null
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --component clippy,rustfmt >/dev/null
. "$HOME/.cargo/env"
# llama.cpp's bindings need the C compiler's built-in headers.
export BINDGEN_EXTRA_CLANG_ARGS="-I$(gcc -print-file-name=include)"

case $mode in
    check)
        cargo fmt --all -- --check
        # No root-owned __pycache__ left in the runner's checkout.
        PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s packaging/linux -p 'test_disk_images.py'
        # Both builds: with smart search (the default) and without it.
        cargo clippy --locked --all-targets -- -D warnings
        cargo clippy --locked --all-targets --no-default-features -- -D warnings
        ;;
    build)
        cargo test --locked
        cargo build --locked --release
        cargo install --locked cargo-deb cargo-generate-rpm >/dev/null 2>&1
        cargo deb --no-build --output target/
        cargo generate-rpm --output target/
        ls -la target/*.deb target/*.rpm
        ;;
esac
