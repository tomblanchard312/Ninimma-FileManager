#!/bin/sh
# Ninimma modification: independent project packaging and branding.
# Install Ninimma for the current user (no root needed).
#   ./install.sh            install binary + menu entry
#   ./install.sh --default  also make it the default app for opening folders
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
app_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"

mkdir -p "$bin_dir" "$app_dir"
install -m 755 "$here/ninimma" "$bin_dir/ninimma"
# Use an absolute path so it works even if ~/.local/bin isn't on PATH.
# Escape sed replacement characters and quote the desktop Exec path.
exec_path=$(printf '%s' "$bin_dir/ninimma" | sed 's/[\\"`$]/\\&/g; s/[&|]/\\&/g')
sed "s|^Exec=ninimma|Exec=\"$exec_path\"|" "$here/ninimma.desktop" > "$app_dir/ninimma.desktop"
icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
for png in "$here"/icons/ninimma-*.png; do
    size=$(basename "$png" .png | sed 's/ninimma-//')
    install -D -m 644 "$png" "$icon_dir/${size}x${size}/apps/ninimma.png"
done
install -D -m 644 "$here/icons/ninimma.svg" "$icon_dir/scalable/apps/ninimma.svg"
# Software centres read the AppStream metadata (older tarballs lack it).
for xml in "$here"/*.metainfo.xml; do
    if [ -f "$xml" ]; then
        install -D -m 644 "$xml" "${XDG_DATA_HOME:-$HOME/.local/share}/metainfo/$(basename "$xml")"
    fi
done
notice_dir="${XDG_DATA_HOME:-$HOME/.local/share}/licenses/ninimma"
mkdir -p "$notice_dir"
for notice in LICENSE NOTICE LICENSE-lucide; do
    install -m 644 "$here/$notice" "$notice_dir/$notice"
done
gtk-update-icon-cache -q -t "$icon_dir" 2>/dev/null || true
update-desktop-database "$app_dir" 2>/dev/null || true
echo "Installed to $bin_dir/ninimma"

if [ "${1:-}" = "--default" ]; then
    "$bin_dir/ninimma" --make-default
fi
