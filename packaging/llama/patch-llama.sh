#!/bin/sh
# Ninimma modification: independent project packaging and branding.
# Lets smart search (llama.cpp) build on FreeBSD and with MinGW (Windows,
# cross-built on Linux).
#
# llama-cpp-sys-2's build script stops on FreeBSD (it only knows Windows,
# Apple, Android and Linux), and with MinGW misses ggml's libraries (named
# "ggml.a", not "libggml.a"). This copies the crate from Cargo's cache into
# target/, applies llama-cpp-sys-2.patch (FreeBSD builds like Linux, linking
# libc++; MinGW gets lib-prefixed copies of ggml's libraries), points this
# checkout's Cargo.lock at the copy, and prints the --config value that makes
# Cargo use it:
#
#   export LIBCLANG_PATH=/usr/local/llvm19/lib     # or the llvm you have
#   cargo build --locked --release --config "$(sh packaging/llama/patch-llama.sh)"
#
# Cargo.lock changes only in llama-cpp-sys-2's entry (a path dependency has
# no source or checksum), so --locked still guarantees nothing else moves.
# Restore it afterwards with `git checkout Cargo.lock`; don't commit it.
# If the crate's version changes, the patch is checked against it and this
# fails loudly rather than building something unpatched.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
lock="$root/Cargo.lock"
version=$(awk '$0 == "name = \"llama-cpp-sys-2\"" { getline; gsub(/version = |"/, ""); print; exit }' "$lock")
[ -n "$version" ] || { echo "llama-cpp-sys-2 isn't in Cargo.lock" >&2; exit 1; }

cargo fetch --locked --manifest-path "$root/Cargo.toml" >/dev/null
src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"llama-cpp-sys-2-$version" | head -n 1)
dest="$root/target/llama-cpp-sys-2-patched"
rm -rf "$dest"
mkdir -p "$root/target"
cp -R "$src" "$dest"
(cd "$dest" && patch -s -p1 --forward < "$root/packaging/llama/llama-cpp-sys-2.patch") >&2

# A path dependency has no source or checksum in the lock file.
awk '
    $0 == "name = \"llama-cpp-sys-2\"" { inside = 1 }
    inside && /^(source|checksum) = / { next }
    /^$/ { inside = 0 }
    { print }
' "$lock" > "$lock.tmp" && mv "$lock.tmp" "$lock"

echo "patch.crates-io.llama-cpp-sys-2.path=\"$dest\""
