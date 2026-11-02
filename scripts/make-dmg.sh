#!/bin/sh
# Builds oxbot for friends: one universal app (Apple chip + Intel) in a .dmg,
# with the browser tool, Node.js and oxbot's Mac helper (video frames and
# dictation) inside.
# No Apple account: the app gets a local (ad-hoc) signature, and friends allow
# it once in System Settings → Privacy & Security → Open Anyway.
# The .dmg is made with hdiutil, not Tauri's packer, which needs to control
# Finder.
set -e
cd "$(dirname "$0")/.."
# The browser tool and Node.js go inside the app (see fetch-tools.sh).
sh scripts/fetch-tools.sh
# So does the Mac helper, where Swift is here (see build-mac-helper.sh).
sh scripts/build-mac-helper.sh
cd app
cargo tauri build --target universal-apple-darwin --bundles app
bundle=../target/universal-apple-darwin/release/bundle
app=$bundle/macos/oxbot.app
rm -rf "$app/Contents/Resources/tools"
cp -R ../target/tools-bundle "$app/Contents/Resources/tools"
rm -f "$app/Contents/MacOS/oxbot-mac"
if [ -f ../target/mac-helper/oxbot-mac ]; then
  cp ../target/mac-helper/oxbot-mac "$app/Contents/MacOS/oxbot-mac"
  # Signed on its own first (its own identifier), then with the app.
  codesign --force --sign - --identifier dev.oxbot.mac-helper "$app/Contents/MacOS/oxbot-mac"
else
  echo "make-dmg: no Mac helper; this app takes no video frames and has no microphone" >&2
fi
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -1)
stage=$(mktemp -d)
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
mkdir -p "$bundle/dmg"
out="$bundle/dmg/oxbot_${version}_universal.dmg"
# The disk that opens shows the oxbot icon: build it writable, give its top
# folder the icon, then compress it.
icon="$app/Contents/Resources/icon.icns"
cp "$icon" "$stage/.VolumeIcon.icns"
rw="$stage.rw.dmg"
hdiutil create -volname oxbot -srcfolder "$stage" -ov -format UDRW "$rw" >/dev/null
mount=$(hdiutil attach "$rw" -nobrowse -noautoopen | sed -n 's|.*\(/Volumes/oxbot.*\)$|\1|p' | tail -1)
SetFile -a C "$mount"
SetFile -a V "$mount/.VolumeIcon.icns"
hdiutil detach "$mount" -quiet
rm -f "$out"
hdiutil convert "$rw" -format UDZO -o "$out" >/dev/null
rm -rf "$stage" "$rw"
# The .dmg file itself gets the icon too (Finder shows it until the file is
# uploaded somewhere that drops custom icons).
osascript -l JavaScript -e "ObjC.import('AppKit'); \$.NSWorkspace.sharedWorkspace.setIconForFileOptions(\$.NSImage.alloc.initWithContentsOfFile('$icon'), '$out', 0)" >/dev/null
echo "Built $out"
