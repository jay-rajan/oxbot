#!/bin/sh
# Dev mode: rebuilds and restarts `oxbot serve` whenever Rust code changes.
# The page is served from crates/oxbot/ui on every request, so page edits
# show on refresh with no rebuild, and an open page reloads itself after
# each restart.
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
export OXBOT_UI_DIR="$PWD/crates/oxbot/ui"
exec watchexec --restart --stop-signal SIGTERM --stop-timeout 10s \
  --watch crates --exts rs,toml \
  -- cargo run -q -p oxbot -- serve "$@"
