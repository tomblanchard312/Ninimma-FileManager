#!/bin/sh
# Ninimma modification: independent project packaging and branding.
# Lets smart search (llama.cpp) build on FreeBSD.
#
# llama-cpp-sys-2's build script does not recognize FreeBSD. This copies the
# crate from Cargo's cache into target/, applies the FreeBSD/libc++ patch,
# points this
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
patch_file="$root/packaging/llama/llama-cpp-sys-2.patch"
# Preserve a validated cached copy's mtimes, or Cargo rebuilds all of llama.cpp.
if ! cmp -s "$src/Cargo.toml" "$dest/Cargo.toml" || ! cmp -s "$patch_file" "$dest/.applied-patch"; then
    rm -rf "$dest"
    mkdir -p "$root/target"
    cp -pR "$src" "$dest"
    (cd "$dest" && patch -s -p1 --forward < "$patch_file") >&2
    cp "$patch_file" "$dest/.applied-patch"
fi

# A path dependency has no source or checksum in the lock file.
awk '
    $0 == "name = \"llama-cpp-sys-2\"" { inside = 1 }
    inside && /^(source|checksum) = / { next }
    /^$/ { inside = 0 }
    { print }
' "$lock" > "$lock.tmp" && mv "$lock.tmp" "$lock"

echo "patch.crates-io.llama-cpp-sys-2.path=\"$dest\""
