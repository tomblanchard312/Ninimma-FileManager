#!/usr/bin/env bash
# Check the actual DMG payload on a native macOS runner without touching /Applications.
set -euo pipefail
dmg=${1:?DMG required}
stage=$(mktemp -d)
pid=
cleanup() {
    if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
    hdiutil detach "$stage/mount" -quiet 2>/dev/null || true
    rm -rf "$stage"
}
trap cleanup EXIT
mkdir "$stage/mount" "$stage/installed"
hdiutil attach "$dmg" -mountpoint "$stage/mount" -readonly -nobrowse -quiet
test -L "$stage/mount/Applications"
ditto "$stage/mount/Ninimma.app" "$stage/installed/Ninimma.app"
app="$stage/installed/Ninimma.app/Contents"
for file in Info.plist MacOS/ninimma Resources/ninimma.icns Resources/LICENSE Resources/NOTICE Resources/LICENSE-lucide; do
    test -s "$app/$file"
done
plutil -lint "$app/Info.plist"
"$app/MacOS/ninimma" --version | grep -E '^ninimma [0-9]'
"$app/MacOS/ninimma" "$stage" >"$stage/app.log" 2>&1 &
pid=$!
sleep 8
kill -0 "$pid" || { cat "$stage/app.log"; exit 1; }
kill "$pid"
wait "$pid" || true
pid=
rm -rf "$stage/installed/Ninimma.app"
test ! -e "$stage/installed/Ninimma.app"
echo 'DMG install, native startup, and removal passed'
