#!/usr/bin/env bash
# Packaging tags keep native release tags immutable and provenance tied to the checkout.
set -euo pipefail
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
base="refs/tags/flatpak-$1"
revision=${2#"$base"}
if [[ "$1" != "v$version" || "$2" != "$base$revision" || ! "$revision" =~ ^(-r[1-9][0-9]*)?$ ]]; then
  echo 'Run from flatpak-vVERSION (optionally -rN) with tag input vVERSION matching Cargo.toml.' >&2
  exit 1
fi
