#!/bin/sh
# Dev mode: rebuilds and restarts `oxbot serve` whenever Rust code changes.
# The page is served from crates/oxbot/ui-dist on every request: run
# `npm run build -- --watch` in crates/oxbot/ui-src beside this, and page
# edits show on refresh. An open page reloads itself after each restart.
#
#   scripts/dev.sh                 # your real bots and data
#   OXBOT_HOME=/tmp/oxdev scripts/dev.sh   # a separate, throwaway home
#
# Needs watchexec once: brew install watchexec
set -eu
cd "$(dirname "$0")/.."
if ! command -v watchexec >/dev/null 2>&1; then
  echo "dev mode needs watchexec: brew install watchexec" >&2
  exit 1
fi
export OXBOT_UI_DIR="$PWD/crates/oxbot/ui-dist"
# Video frames and dictation need the Mac helper (built once).
[ -f target/mac-helper/oxbot-mac ] || sh scripts/build-mac-helper.sh
exec watchexec --restart --stop-signal SIGTERM --stop-timeout 10s \
  --watch crates --exts rs,toml \
  -- cargo run -q -p oxbot -- serve "$@"
