#!/usr/bin/env bash
# Ninimma modification: independent project packaging and branding.
# Builds the Linux release (tar.gz, .deb, .rpm) inside Ubuntu 22.04, so the
# binary needs only glibc 2.35 (Ubuntu 22.04, Mint 21, Zorin 17, Debian 12,
# Fedora 36+) whatever the runner host runs.
#
# On the host:   packaging/ci/build-linux-release.sh v0.3.0
# It starts the container itself (against the system Docker daemon, not a
# Docker Desktop context that may not be running) and leaves the files in
# the current directory, owned by the calling user.
set -euo pipefail

tag=${1:-${GITHUB_REF_NAME:-}}
[[ -n $tag ]] || { echo "usage: $0 <tag, e.g. v0.3.0>" >&2; exit 1; }

if [[ ${IN_RELEASE_CONTAINER:-} != 1 ]]; then
    export DOCKER_HOST=${DOCKER_HOST:-unix:///var/run/docker.sock}
    docker info >/dev/null || { echo "Docker isn't reachable at $DOCKER_HOST." >&2; exit 1; }
    exec docker run --rm \
        -v "$PWD:/src" -w /src \
        -e IN_RELEASE_CONTAINER=1 -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
        ubuntu:22.04 bash packaging/ci/build-linux-release.sh "$tag"
fi

# --- Inside the container, as root ---
target=x86_64-unknown-linux-gnu
archive=ninimma-linux-x64.tar.gz
# Hand everything back to the caller, however this ends.
trap 'chown -R "$HOST_UID:$HOST_GID" /src/target /src/*.deb /src/*.rpm /src/$archive 2>/dev/null || true' EXIT

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    ca-certificates curl git build-essential pkg-config cmake clang libclang-dev \
    libasound2-dev libssl-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev \
    libwayland-dev rpm >/dev/null
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
. "$HOME/.cargo/env"
cargo install --locked cargo-deb cargo-generate-rpm >/dev/null 2>&1
# llama.cpp's bindings need the C compiler's built-in headers.
export BINDGEN_EXTRA_CLANG_ARGS="-I$(gcc -print-file-name=include)"

cargo build --locked --release --target "$target"
echo "Needs: $(objdump -T "target/$target/release/ninimma" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)"

release=target/$target/release
python3 packaging/stage-enki.py --target "$target" --out "$release/enki"
cp packaging/icons/LICENSE-lucide "$release/"
cp packaging/linux/ninimma.desktop packaging/linux/install.sh LICENSE NOTICE \
    packaging/linux/io.github.tomblanchard312.ninimma.metainfo.xml "$release/"
mkdir -p "$release/icons"
cp assets/icons/*.png "$release/icons/"
cp assets/icon.svg "$release/icons/ninimma.svg"
tar -C "$release" -czf "$archive" ninimma enki ninimma.desktop install.sh icons LICENSE NOTICE LICENSE-lucide \
    io.github.tomblanchard312.ninimma.metainfo.xml

# v1.2.0-rc1 -> 1.2.0~rc1: "~" sorts before the final release in both dpkg
# and rpm (a "-" is invalid in an rpm Version, and dpkg would read "rc1" as a
# revision newer than 1.2.0).
version=${tag#v}
version=${version//-/\~}
cargo deb --no-build --target "$target" --deb-version "$version" --output .
cargo generate-rpm --target "$target" --set-metadata "version = \"$version\"" --output .
ls -la "$archive" ./*.deb ./*.rpm
