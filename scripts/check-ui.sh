#!/usr/bin/env bash
# Checks that the built React page in crates/oxbot/ui-dist/ (which the oxbot
# program embeds) matches a fresh build of crates/oxbot/ui-src/, and that the
# page's protocol types match the Rust ones. Needs Node and npm, so it's
# opt-in and not part of `cargo test`.
#
#   scripts/check-ui.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/crates/oxbot/ui-src"
dist="$root/crates/oxbot/ui-dist"
fresh="$(mktemp -d)"
trap 'rm -rf "$fresh"' EXIT

echo "== protocol types"
(cd "$root" && cargo test -q -p oxbot-protocol --features ts --test bindings)

echo "== net/ and state/ stay platform-neutral"
# Generated types are checked by the protocol test above; look for uses
# (`window.`, `localStorage`…) in the hand-written code.
if grep -rnE '\b(document|window|location|navigator|history|globalThis)\.|\b(localStorage|sessionStorage)\b' \
  "$src/src/net" "$src/src/state" --include='*.ts' --include='*.tsx' --exclude-dir=protocol; then
  echo "Browser-only APIs in src/net or src/state: pass them in from main.tsx instead." >&2
  exit 1
fi

echo "== install (lockfile only, no install scripts)"
(cd "$src" && npm ci --no-audit --no-fund)

echo "== types and unit tests"
(cd "$src" && npx tsc --noEmit && npx vitest run)

echo "== fresh build"
(cd "$src" && npx vite build --outDir "$fresh" --emptyOutDir >/dev/null)

if ! diff -r "$fresh" "$dist"; then
  echo "crates/oxbot/ui-dist/ is out of date: run \`npm run build\` in crates/oxbot/ui-src and commit it." >&2
  exit 1
fi
echo "ui-dist matches the sources."
