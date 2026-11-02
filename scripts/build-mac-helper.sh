#!/bin/sh
# Builds oxbot-mac, the small Swift helper that takes still frames from
# videos (AVFoundation) and runs on-device dictation (Speech), as one
# universal program (Apple chip + Intel) at target/mac-helper/oxbot-mac.
# make-dmg.sh puts it in the app; `oxbot serve` from a checkout finds it
# there too.
#
# Without Swift (Xcode or its Command Line Tools) it builds nothing and
# says so: oxbot still works, and video and the microphone then say
# they're not available on this Mac.
set -eu
cd "$(dirname "$0")/.."
out=target/mac-helper
if ! command -v swiftc >/dev/null 2>&1 || ! command -v lipo >/dev/null 2>&1; then
  echo "build-mac-helper: no Swift here (install Xcode's Command Line Tools: xcode-select --install); oxbot builds without video frames and dictation" >&2
  rm -f "$out/oxbot-mac"
  exit 0
fi
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for arch in arm64 x86_64; do
  # Its own Info.plist (inside the program) names it and says why it may
  # use the mic and speech recognition; macOS stops it without them.
  swiftc -O -swift-version 5 -target "$arch-apple-macos13.0" \
    -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker mac/Info.plist \
    mac/oxbot-mac.swift -o "$tmp/oxbot-mac-$arch"
done
lipo -create "$tmp/oxbot-mac-arm64" "$tmp/oxbot-mac-x86_64" -output "$out/oxbot-mac"
# A local signature, like the app's; make-dmg.sh signs it again inside the app.
codesign --force --sign - --identifier dev.oxbot.mac-helper "$out/oxbot-mac"
echo "Built $out/oxbot-mac"
