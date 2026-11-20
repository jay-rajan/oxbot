#!/bin/sh
# Signs oxbot's browser inside a built bundle (scripts/camoufox/build.mjs),
# inside-out: every library and program, then each helper app and
# framework, then Camoufox.app itself, all with the hardened runtime and
# scripts/mac/browser-entitlements.plist. The upstream app is only ad-hoc
# signed, which notarizing refuses. Signing changes the browser program,
# so its pinned checksum in bundle.json is updated to the signed one.
#
# Usage: sign.sh BUNDLE [IDENTITY]   (no identity: a local, ad-hoc signature)
set -e
bundle=$1
identity=${2:--}
[ -f "$bundle/bundle.json" ] || { echo "sign.sh: $bundle isn't a browser bundle" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
entitlements="$here/../mac/browser-entitlements.plist"
app="$bundle/browser/Camoufox.app"

sign() { # path [extra flags…]
  path=$1
  shift
  if [ "$identity" = - ]; then
    codesign --force --sign - --options runtime "$@" "$path"
  else
    codesign --force --sign "$identity" --timestamp --options runtime "$@" "$path"
  fi
}

# Whether file $1 is its app's own program (CFBundleExecutable): that's
# signed with its app below, once everything inside the app is signed.
# Signing it on its own first fails when a helper app beside it isn't
# signed yet (the Intel browser comes with nothing signed).
main_program() {
  case "$1" in *.app/Contents/MacOS/*) ;; *) return 1 ;; esac
  bundle=$(dirname "$(dirname "$(dirname "$1")")")
  [ "$(dirname "$1")" = "$bundle/Contents/MacOS" ] || return 1
  exe=$(/usr/libexec/PlistBuddy -c "Print :CFBundleExecutable" "$bundle/Contents/Info.plist" 2>/dev/null) || return 1
  [ "$(basename "$1")" = "$exe" ]
}

# Loose libraries and programs first. Each app's own program is signed
# with its app below.
find "$app" -depth -type f | while IFS= read -r f; do
  case "$f" in *.app/Contents/Info.plist | */_CodeSignature/*) continue ;; esac
  main_program "$f" && continue
  file -b "$f" | grep -q Mach-O || continue
  if [ -x "$f" ] && ! printf %s "$f" | grep -q '\.dylib$'; then
    sign "$f" --entitlements "$entitlements"
  else
    sign "$f"
  fi
done
# Then the bundles inside, deepest first, and the app around them.
find "$app/Contents" -depth -type d \( -name '*.app' -o -name '*.framework' \) | while IFS= read -r b; do
  case "$b" in
    *.app) sign "$b" --entitlements "$entitlements" ;;
    *) sign "$b" ;;
  esac
done
sign "$app" --entitlements "$entitlements"
codesign --verify --deep --strict "$app"

# The runtime checks the browser program against bundle.json.
node=${OXBOT_NODE:-node}
"$node" -e '
const fs = require("fs"), path = require("path"), crypto = require("crypto");
const root = process.argv[1], file = path.join(root, "bundle.json");
const manifest = JSON.parse(fs.readFileSync(file, "utf8"));
const exe = fs.readFileSync(path.join(root, manifest.executable));
manifest.executableSha256 = crypto.createHash("sha256").update(exe).digest("hex");
manifest.signed = process.argv[2] !== "-";
fs.writeFileSync(file, JSON.stringify(manifest, null, 2) + "\n");
' "$bundle" "$identity"
echo "sign.sh: signed $app"
