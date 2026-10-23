#!/usr/bin/env bash
# Packaging tags keep native release tags immutable and provenance tied to the checkout.
set -euo pipefail
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [[ "$1" != "v$version" || "$2" != "refs/tags/flatpak-$1" ]]; then
  echo 'Run from flatpak-vVERSION with tag input vVERSION matching Cargo.toml.' >&2
  exit 1
fi
