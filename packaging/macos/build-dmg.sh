#!/usr/bin/env bash
# Ninimma modification: Finder-installable app bundle.
set -euo pipefail
target=${1:?target required}
arch=${2:?architecture required}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app="$stage/Ninimma.app/Contents"
mkdir -p "$app/MacOS" "$app/Resources" "$stage/Ninimma.iconset"
cp "target/$target/release/ninimma" "$app/MacOS/ninimma"
cp LICENSE NOTICE "$app/Resources/"
cp packaging/icons/LICENSE-lucide "$app/Resources/"
for size in 16 32 128 256 512; do
    cp "assets/icons/ninimma-$size.png" "$stage/Ninimma.iconset/icon_${size}x${size}.png"
done
iconutil -c icns "$stage/Ninimma.iconset" -o "$app/Resources/ninimma.icns"
rm -rf "$stage/Ninimma.iconset"
cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>NSMicrophoneUsageDescription</key>
<string>Record audio only when you choose Dictate or Voice command for local speech recognition.</string>
<key>CFBundleName</key><string>Ninimma</string>
<key>CFBundleDisplayName</key><string>Ninimma</string>
<key>CFBundleIdentifier</key><string>io.github.tomblanchard312.ninimma</string>
<key>CFBundleExecutable</key><string>ninimma</string>
<key>CFBundleIconFile</key><string>ninimma.icns</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
ln -s /Applications "$stage/Applications"
hdiutil create -volname Ninimma -srcfolder "$stage" -ov -format UDZO "ninimma-macos-$arch.dmg"
