#!/bin/sh
# Checks oxbot-mac on this Mac (macOS only): still frames from the 3 s test
# clip, and that can-dictate answers. Dictating a spoken file needs speech
# recognition allowed for the terminal, so it runs only with SPEECH=1:
#   sh mac/tests.sh            SPEECH=1 sh mac/tests.sh
set -eu
cd "$(dirname "$0")/.."
sh scripts/build-mac-helper.sh
helper=target/mac-helper/oxbot-mac
[ -x "$helper" ] || { echo "no helper built (no Swift here): skipped"; exit 0; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*"; exit 1; }
out=$("$helper" frames e2e/fixtures/clip.mov "$tmp/frames")
case "$out" in *'"frames":3'*) ;; *) fail "frames: $out" ;; esac
[ "$(ls "$tmp/frames" | wc -l | tr -d ' ')" = 3 ] || fail "3 JPEGs expected"
echo "ok   frames: $out"
"$helper" frames README.md "$tmp/x" >"$tmp/err" 2>/dev/null && fail "a non-video gave frames"
grep -q '"error"' "$tmp/err" || fail "no error for a non-video"
echo "ok   a non-video is refused"
out=$("$helper" can-dictate)
case "$out" in *'"ok":'*) ;; *) fail "can-dictate: $out" ;; esac
echo "ok   can-dictate: $out"
if [ "${SPEECH:-}" = 1 ]; then
  say -o "$tmp/hello.aiff" "Remind me to call the plumber"
  out=$("$helper" dictate --file "$tmp/hello.aiff" | tail -1)
  case "$out" in *plumber*'"final":true'*|*'"final":true'*plumber*) ;; *) fail "dictate: $out" ;; esac
  echo "ok   dictate (on-device): $out"
else
  echo "skip dictate (SPEECH=1 to run it)"
fi
echo "ALL OK"
