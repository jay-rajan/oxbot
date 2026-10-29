#!/bin/sh
# Builds oxbot for friends: one universal app (Apple chip + Intel) in a .dmg,
# with the browser tool and Node.js inside.
# No Apple account: the app gets a local (ad-hoc) signature, and friends allow
# it once in System Settings → Privacy & Security → Open Anyway.
# The .dmg is made with hdiutil, not Tauri's packer, which needs to control
# Finder.
set -e
cd "$(dirname "$0")/.."
# The browser tool and Node.js go inside the app (see fetch-tools.sh).
sh scripts/fetch-tools.sh
cd app
cargo tauri build --target universal-apple-darwin --bundles app
bundle=../target/universal-apple-darwin/release/bundle
app=$bundle/macos/oxbot.app
rm -rf "$app/Contents/Resources/tools"
cp -R ../target/tools-bundle "$app/Contents/Resources/tools"
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -1)
stage=$(mktemp -d)
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
mkdir -p "$bundle/dmg"
out="$bundle/dmg/oxbot_${version}_universal.dmg"
hdiutil create -volname oxbot -srcfolder "$stage" -ov -format UDZO "$out"
rm -rf "$stage"
echo "Built $out"
