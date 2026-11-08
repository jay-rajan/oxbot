#!/bin/sh
# Builds oxbot for friends: one app per chip, each in its own .dmg, with the
# browser tool, Node.js, Codex and oxbot's Mac helper (video frames and
# dictation) inside. Codex alone is ~260 MB per chip, so a universal app
# would be twice the size for everyone.
#
# Usage: make-dmg.sh [arm64] [x64]  (default: both)
# Output: target/<triple>/release/bundle/dmg/oxbot_<version>_{apple-chip,intel}.dmg
#
# No Apple account: the app gets a local (ad-hoc) signature, and friends allow
# it once in System Settings → Privacy & Security → Open Anyway. Codex and
# Node.js keep their makers' signatures inside it.
# The .dmg is made with hdiutil, not Tauri's packer, which needs to control
# Finder.
set -e
cd "$(dirname "$0")/.."
# The Mac helper, where Swift is here (see build-mac-helper.sh): universal,
# thinned to each chip below.
sh scripts/build-mac-helper.sh
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

build_one() { # arm64|x64
  case "$1" in
    arm64) triple=aarch64-apple-darwin lipo_arch=arm64 label=apple-chip ;;
    x64) triple=x86_64-apple-darwin lipo_arch=x86_64 label=intel ;;
    *) echo "make-dmg: unknown chip '$1' (arm64 or x64)" >&2; exit 2 ;;
  esac
  # The browser tool, Node.js and Codex for this chip (see fetch-tools.sh).
  sh scripts/fetch-tools.sh "$1"
  (cd app && cargo tauri build --target "$triple" --bundles app)
  bundle=target/$triple/release/bundle
  app=$bundle/macos/oxbot.app
  rm -rf "$app/Contents/Resources/tools"
  cp -R target/tools-bundle "$app/Contents/Resources/tools"
  rm -f "$app/Contents/MacOS/oxbot-mac"
  if [ -f target/mac-helper/oxbot-mac ]; then
    lipo -thin "$lipo_arch" target/mac-helper/oxbot-mac -output "$app/Contents/MacOS/oxbot-mac"
    # Signed on its own first (its own identifier), then with the app.
    codesign --force --sign - --identifier dev.oxbot.mac-helper "$app/Contents/MacOS/oxbot-mac"
  else
    echo "make-dmg: no Mac helper; this app takes no video frames and has no microphone" >&2
  fi
  codesign --force --deep --sign - "$app"
  codesign --verify --deep --strict "$app"
  make_dmg "$app" "$bundle/dmg/oxbot_${version}_$label.dmg"
}

make_dmg() { # app out
  app=$1 out=$2
stage=$(mktemp -d)
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
mkdir -p "$(dirname "$out")"
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
echo "Built $out ($(du -h "$out" | cut -f1))"
}

for chip in ${@:-arm64 x64}; do
  build_one "$chip"
done
