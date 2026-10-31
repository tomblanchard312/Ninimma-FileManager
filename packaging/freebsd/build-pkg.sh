#!/bin/sh
# Ninimma modification: independent project packaging and branding.
# Build a FreeBSD package (.pkg) from an already-built release binary.
#
#   cargo build --release
#   sh packaging/freebsd/build-pkg.sh [version] [output-dir]
#
# Install the result with:  pkg add ./ninimma-<version>.pkg
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
version="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)}"
out="${2:-$root/target/freebsd}"
binary="$root/target/release/ninimma"
[ -x "$binary" ] || { echo "Build first: cargo build --release" >&2; exit 1; }

stage=$(mktemp -d)
work=$(mktemp -d)
trap 'rm -rf "$stage" "$work"' EXIT
prefix=/usr/local

# Lay out the files under the staging root, and list them for the package.
plist="$work/plist"
: > "$plist"
put() { # put <source> <path under prefix> <mode>
    mkdir -p "$(dirname "$stage$prefix/$2")"
    install -m "$3" "$1" "$stage$prefix/$2"
    echo "$2" >> "$plist"
}
put "$binary" bin/ninimma 755
put "$root/packaging/linux/ninimma.desktop" share/applications/ninimma.desktop 644
for png in "$root"/assets/icons/ninimma-*.png; do
    size=$(basename "$png" .png | sed 's/ninimma-//')
    put "$png" "share/icons/hicolor/${size}x${size}/apps/ninimma.png" 644
done
put "$root/assets/icon.svg" share/icons/hicolor/scalable/apps/ninimma.svg 644
put "$root/README.md" share/doc/ninimma/README.md 644
put "$root/LICENSE" share/licenses/ninimma/LICENSE 644
put "$root/NOTICE" share/licenses/ninimma/NOTICE 644
put "$root/packaging/icons/LICENSE-lucide" share/licenses/ninimma/LICENSE-lucide 644

# The window-system libraries are loaded at runtime, so declare them as
# dependencies, using the versions installed on the build machine.
deps=""
for name in alsa-lib libX11 libxkbcommon libXcursor libXi libXrender mesa-libs; do
    if line=$(pkg query '%n|%o|%v' "$name" 2>/dev/null) && [ -n "$line" ]; then
        n=${line%%|*}; rest=${line#*|}; o=${rest%%|*}; v=${rest#*|}
        deps="$deps    \"$n\": { origin: \"$o\", version: \"$v\" },
"
    else
        echo "warning: $name not installed on the build machine; not declared as a dependency" >&2
    fi
done

# FreeBSD versions can't contain "-": v1.2.0-rc1 -> 1.2.0.rc1
pkg_version=$(echo "$version" | tr '-' '.')
cat > "$work/+MANIFEST" <<MANIFEST
name: ninimma
version: "$pkg_version"
origin: x11-fm/ninimma
comment: "File manager with large thumbnails, bulk image rotation and tabs"
www: https://github.com/tomblanchard312/Ninimma-FileManager
maintainer: tomblanchard3@gmail.com
prefix: $prefix
licenselogic: single
licenses: [APACHE20]
categories: [x11-fm]
desc: <<EOD
A fast file manager with large thumbnails, Ctrl + mouse wheel icon sizing,
lossless bulk image rotation, tabs, undo/redo, drag and drop, network
shares and familiar keyboard shortcuts.
EOD
deps: {
$deps}
scripts: {
  post-install: "gtk-update-icon-cache -q -t $prefix/share/icons/hicolor 2>/dev/null || true; update-desktop-database -q $prefix/share/applications 2>/dev/null || true"
  post-deinstall: "gtk-update-icon-cache -q -t $prefix/share/icons/hicolor 2>/dev/null || true; update-desktop-database -q $prefix/share/applications 2>/dev/null || true"
}
MANIFEST

mkdir -p "$out"
pkg create -M "$work/+MANIFEST" -p "$plist" -r "$stage" -o "$out"
ls -l "$out"/ninimma-*.pkg
