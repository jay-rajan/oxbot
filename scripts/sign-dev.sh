#!/bin/sh
# Signs the development build of oxbot with the Developer ID in this Mac's
# Keychain, so the Keychain's "Always Allow" for oxbot's OpenRouter key
# lasts across rebuilds (it follows the signer, not one build). Without a
# Developer ID it does nothing.
#
# Usage: sign-dev.sh [binary]   (default: target/debug/oxbot)
set -e
cd "$(dirname "$0")/.."
bin=${1:-target/debug/oxbot}
id=${OXBOT_SIGN_ID:-$(security find-identity -v -p codesigning 2>/dev/null |
  sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -1)}
if [ -z "$id" ]; then
  echo "sign-dev: no Developer ID certificate; left unsigned" >&2
  exit 0
fi
codesign --force --sign "$id" --identifier dev.oxbot.server "$bin"
echo "sign-dev: signed $bin as $id"
