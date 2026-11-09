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

echo "== one theme: colors and type sizes from theme/tokens.css, no inline styles"
# Raw colors and pixel font sizes live only in src/theme/; screens and pieces
# use its tokens. A style from data goes through vars() (custom properties).
styles=$(find "$src/src" -name '*.css' -not -path "$src/src/theme/*")
bad=0
if grep -nE '#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\(' $styles; then
  echo "Raw colors above: use a token from src/theme/tokens.css." >&2; bad=1
fi
if grep -nE 'font-size:[^;]*[0-9.]+(px|rem|em|pt)|(^|[^-])font:' $styles; then
  echo "Font sizes above: use var(--text-xs|sm|md|lg|xl)." >&2; bad=1
fi
if grep -rnE 'style=\{\{' "$src/src" --include='*.tsx'; then
  echo "Inline styles above: put the look in CSS; data goes through style={vars({...})}." >&2; bad=1
fi
[ "$bad" = 0 ]

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
