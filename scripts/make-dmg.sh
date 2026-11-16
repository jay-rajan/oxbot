#!/bin/sh
# Builds oxbot for friends: one app per chip, each in its own .dmg, with
# oxbot's browser (Camoufox), Node.js, Codex and oxbot's Mac helper (video
# frames and dictation) inside. Codex alone is ~260 MB and the browser
# ~390 MB per chip, so a universal app would be twice the size for everyone.
#
# Usage: make-dmg.sh [--no-notarize] [arm64] [x64]  (default: both chips)
# Output: target/<triple>/release/bundle/dmg/oxbot_<version>_{apple-chip,intel}.dmg
#         and its SHA-256 next to it (…dmg.sha256), for friends to check;
#         target/<triple>/release/bundle/slim/oxbot.app, the same app
#         without its packs, notarized and stapled, which updates install
#         (the packs come apart: make-packs.sh, crates/oxbot/src/packs.rs).
#
# Signing: with a "Developer ID Application" certificate in the Keychain (or
# OXBOT_SIGN_ID naming one), the helper, the app and the .dmg are signed with
# it and a secure timestamp, then the .dmg is notarized by Apple with the
# notarytool profile OXBOT_NOTARY_PROFILE (default "oxbot") and stapled, so
# it opens on any Mac with no warning. Without a certificate the app gets a
# local (ad-hoc) signature, and friends allow it once in System Settings →
# Privacy & Security → Open Anyway.
# The app and the Mac helper run with the hardened runtime (no injected
# libraries or debuggers), allowed only the microphone
# (scripts/mac/entitlements.plist). Codex and Node.js keep their makers'
# signatures inside it: they sit in Resources, which signing seals but never
# re-signs; each must already be Developer ID, hardened and timestamped.
# The browser comes only ad-hoc signed, so its pack is signed, inside-out,
# with its own entitlements (make-packs.sh, scripts/camoufox/sign.sh).
# The .dmg is made with hdiutil, not Tauri's packer, which needs to control
# Finder.
set -e
cd "$(dirname "$0")/.."
# The Mac helper, where Swift is here (see build-mac-helper.sh): universal,
# thinned to each chip below.
sh scripts/build-mac-helper.sh
# Never ship oxbot-core's test-only features.
sh scripts/check-release-features.sh
entitlements=scripts/mac/entitlements.plist
notarize=yes
chips=
for arg in "$@"; do
  case "$arg" in
    --no-notarize) notarize=no ;;
    *) chips="$chips $arg" ;;
  esac
done
sign_id=${OXBOT_SIGN_ID:-$(security find-identity -v -p codesigning 2>/dev/null |
  sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -1)}
profile=${OXBOT_NOTARY_PROFILE:-oxbot}
if [ -n "$sign_id" ]; then
  echo "make-dmg: signing as $sign_id"
else
  echo "make-dmg: no Developer ID certificate; local signature, no notarizing" >&2
  notarize=no
fi
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

build_one() { # arm64|x64
  case "$1" in
    arm64) triple=aarch64-apple-darwin lipo_arch=arm64 label=apple-chip ;;
    x64) triple=x86_64-apple-darwin lipo_arch=x86_64 label=intel ;;
    *) echo "make-dmg: unknown chip '$1' (arm64 or x64)" >&2; exit 2 ;;
  esac
  # The browser, Node.js and Codex for this chip (see fetch-tools.sh), as
  # packs (see make-packs.sh).
  sh scripts/fetch-tools.sh "$1"
  pack_cache=$(sh scripts/make-packs.sh "$1" "${sign_id:--}" | sed -n 's/^PACK_CACHE=//p')
  [ -n "$pack_cache" ] || { echo "make-dmg: no packs" >&2; exit 1; }
  (cd app && cargo tauri build --target "$triple" --bundles app -- --locked)
  bundle=target/$triple/release/bundle
  app=$bundle/macos/oxbot.app
  # The browser's driver, and a first copy of each pack, so a new install
  # needs no download. packs.json names the versions this app runs with.
  tools="$app/Contents/Resources/tools"
  rm -rf "$tools"
  mkdir -p "$tools"
  cp -R "target/app-tools-$1/camoufox" "$tools/camoufox"
  target/tools-bundle/node/bin/node -e '
for (const p of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).packs)
  console.log(p.name + " " + p.version);' "$PWD/target/packs-$1.json" |
    while read -r name version; do
      cp -R "$pack_cache/$name-$version" "$tools/$name"
    done
  cp "target/packs-$1.json" "$app/Contents/Resources/packs.json"
  rm -f "$app/Contents/MacOS/oxbot-mac"
  if [ -f target/mac-helper/oxbot-mac ]; then
    lipo -thin "$lipo_arch" target/mac-helper/oxbot-mac -output "$app/Contents/MacOS/oxbot-mac"
    # Signed on its own first (its own identifier), then the app around it.
    sign --options runtime --entitlements "$entitlements" \
      --identifier dev.oxbot.mac-helper "$app/Contents/MacOS/oxbot-mac"
  else
    echo "make-dmg: no Mac helper; this app takes no video frames and has no microphone" >&2
  fi
  check_tools "$app"
  sign_app "$app"
  slim_app "$app" "$bundle/slim/oxbot.app"
  dmg="$bundle/dmg/oxbot_${version}_$label.dmg"
  make_dmg "$app" "$dmg"
  [ -n "$sign_id" ] && sign "$dmg"
  [ "$notarize" = yes ] && notarize_dmg "$dmg"
  # Last: stapling changes the .dmg.
  (cd "$(dirname "$dmg")" && shasum -a 256 "$(basename "$dmg")" >"$(basename "$dmg").sha256")
  echo "SHA-256: $(cut -d' ' -f1 "$dmg.sha256")"
}

sign() { # codesign flags… path
  if [ -n "$sign_id" ]; then
    codesign --force --sign "$sign_id" --timestamp "$@"
  else
    codesign --force --sign - "$@"
  fi
}

slim_app() { # app out: the app without its packs, for updates
  rm -rf "$2"
  mkdir -p "$(dirname "$2")"
  cp -R "$1" "$2"
  for name in browser codex node; do rm -rf "$2/Contents/Resources/tools/$name"; done
  sign_app "$2"
  [ "$notarize" = yes ] && notarize_app "$2"
  echo "Built $2 ($(du -sh "$2" | cut -f1))"
}

notarize_app() { # app: notarized and stapled on its own, as updates install it
  zip="$(dirname "$1")/oxbot-notarize.zip"
  rm -f "$zip"
  ditto -c -k --keepParent "$1" "$zip"
  echo "make-dmg: sending the app (no packs) to Apple (profile $profile)…"
  out=$(xcrun notarytool submit "$zip" --keychain-profile "$profile" --wait 2>&1) || true
  rm -f "$zip"
  echo "$out" | sed -nE 's/^ *(id|status): /  \1: /p'
  if ! echo "$out" | grep -q 'status: Accepted'; then
    id=$(echo "$out" | sed -n 's/^ *id: //p' | head -1)
    [ -n "$id" ] && xcrun notarytool log "$id" --keychain-profile "$profile" >&2
    echo "make-dmg: Apple did not accept the app" >&2
    exit 1
  fi
  xcrun stapler staple -q "$1"
  xcrun stapler validate -q "$1"
  spctl -a -t exec "$1" || { echo "make-dmg: Gatekeeper refused the app" >&2; exit 1; }
}

sign_app() { # app (its helper already signed)
  # No --deep: that would re-sign what's inside with the app's settings.
  sign --options runtime --entitlements "$entitlements" "$1"
  codesign --verify --deep --strict "$1"
}

check_tools() { # app: every program in Resources must pass notarizing as is
  [ -n "$sign_id" ] || return 0
  bad=0
  for f in $(find "$1/Contents/Resources" -type f \( -perm -u+x -o -name '*.node' -o -name '*.dylib' \)); do
    file -b "$f" | grep -q Mach-O || continue
    info=$(codesign -dvv "$f" 2>&1 || true)
    if ! printf %s "$info" | grep -q '^Authority=Developer ID Application' ||
      ! printf %s "$info" | grep -q 'flags=.*runtime' ||
      ! printf %s "$info" | grep -q '^Timestamp='; then
      echo "make-dmg: not Developer ID, hardened and timestamped: ${f#"$1"/}" >&2
      bad=1
    fi
  done
  [ "$bad" = 0 ] || exit 1
}

notarize_dmg() { # dmg
  echo "make-dmg: sending $(basename "$1") to Apple (profile $profile)…"
  out=$(xcrun notarytool submit "$1" --keychain-profile "$profile" --wait 2>&1) || true
  echo "$out" | sed -nE 's/^ *(id|status): /  \1: /p'
  if ! echo "$out" | grep -q 'status: Accepted'; then
    id=$(echo "$out" | sed -n 's/^ *id: //p' | head -1)
    [ -n "$id" ] && xcrun notarytool log "$id" --keychain-profile "$profile" >&2
    echo "make-dmg: Apple did not accept $(basename "$1")" >&2
    exit 1
  fi
  xcrun stapler staple -q "$1"
  xcrun stapler validate -q "$1"
  spctl -a -t open --context context:primary-signature "$1"
  mount=$(hdiutil attach "$1" -nobrowse -noautoopen -readonly | sed -n 's|.*\(/Volumes/oxbot.*\)$|\1|p' | tail -1)
  spctl -a -t exec "$mount/oxbot.app" && ok=1 || ok=0
  hdiutil detach "$mount" -quiet
  [ "$ok" = 1 ] || { echo "make-dmg: Gatekeeper refused the app" >&2; exit 1; }
  echo "make-dmg: notarized, stapled, and Gatekeeper accepts it"
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

for chip in ${chips:-arm64 x64}; do
  build_one "$chip"
done
