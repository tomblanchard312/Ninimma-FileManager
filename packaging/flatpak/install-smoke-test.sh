#!/usr/bin/env bash
# Run only under the dedicated ordinary-user build account.
set -euo pipefail
app=io.github.tomblanchard312.ninimma
flatpak install --user --noninteractive --reinstall ./ninimma-linux-x64.flatpak
cleanup() { flatpak kill "$app" 2>/dev/null || true; }
trap cleanup EXIT
flatpak run "$app" --version | grep -E '^ninimma [0-9]'
status=0
dbus-run-session -- xvfb-run -a timeout 8 flatpak run --nosocket=wayland --socket=x11 --env=LIBGL_ALWAYS_SOFTWARE=1 "$app" /tmp >flatpak-startup.log 2>&1 || status=$?
if [[ "$status" != 124 ]]; then cat flatpak-startup.log; exit 1; fi
cleanup
flatpak uninstall --user --noninteractive "$app"
if flatpak info --user "$app" >/dev/null 2>&1; then exit 1; fi
echo 'Flatpak bundle install, ordinary-user GUI startup, and removal passed'
