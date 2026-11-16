#!/bin/sh
# Makes oxbot's packs for one chip: its browser (Camoufox.app), Codex and
# Node.js, each a folder apart from the app, so an update downloads only
# the packs that changed (crates/oxbot/src/packs.rs).
#
# Usage: make-packs.sh arm64|x64 [SIGN_ID]   (after fetch-tools.sh)
# Input:  target/tools-bundle/{node,codex,camoufox/browser}
# Output: in the pack cache, for each pack <name>-<version>/ (the folder,
#         marked with .pack.json) and <name>-<version>-<chip>.tar.gz (its
#         download); target/packs-<chip>.json, the list the app carries;
#         target/app-tools-<chip>/camoufox, the browser's driver for the
#         app: without its browser/ folder, its bundle.json naming the
#         pack's signed browser. target/tools-bundle stays as it is (dev
#         and tests run from it).
#
# A pack is made once per version and kept in the cache
# (OXBOT_PACK_CACHE, default ~/Library/Caches/oxbot-release/packs), outside
# target/ so `cargo clean` can't remake it: signing again would change its
# bytes, and homes that have it would download it again for nothing. Change
# BROWSER_REV when the browser is signed or trimmed differently.
set -e
cd "$(dirname "$0")/.."
BROWSER_REV=1
arch=$1
sign_id=${2:--}
case "$arch" in
  arm64 | x64) ;;
  *) echo "make-packs: chip is arm64 or x64" >&2; exit 2 ;;
esac
tools=target/tools-bundle
cache=${OXBOT_PACK_CACHE:-$HOME/Library/Caches/oxbot-release/packs}/$arch
mkdir -p "$cache"
node_version=$(sed -n 's/^NODE_VERSION=//p' scripts/fetch-tools.sh)
codex_version=$(sed -n 's/^CODEX_VERSION=//p' scripts/fetch-tools.sh)
browser_version=$(sed -n 's/^ *"version": "\(.*\)",$/\1/p' "$tools/camoufox/bundle.json")-$BROWSER_REV
[ -n "$node_version" ] && [ -n "$codex_version" ] && [ "$browser_version" != "-$BROWSER_REV" ] ||
  { echo "make-packs: couldn't read the tool versions" >&2; exit 1; }

# The browser is signed only when its pack is made. A signed pack can't be
# swapped for an unsigned one: an ad-hoc (local) build never fills the
# cache a release uses.
if [ "$sign_id" = - ]; then
  cache=$cache/local
  mkdir -p "$cache"
fi

pack() { # name version source-folder
  name=$1 version=$2 from=$3
  dir=$cache/$name-$version
  tar=$cache/$name-$version-$arch.tar.gz
  if [ -d "$dir" ] && [ -f "$tar" ]; then
    echo "make-packs: $name $version (kept)"
    return
  fi
  rm -rf "$dir" "$tar"
  cp -R "$from" "$dir"
  printf '{"name":"%s","version":"%s"}' "$name" "$version" >"$dir/.pack.json"
  (cd "$dir" && tar -czf "$tar.part" .)
  mv "$tar.part" "$tar"
  echo "make-packs: $name $version (made)"
}

pack node "$node_version" "$tools/node"
pack codex "$codex_version" "$tools/codex"
browser_dir=$cache/browser-$browser_version
if [ ! -d "$browser_dir" ] || [ ! -f "$cache/browser-$browser_version-$arch.tar.gz" ]; then
  OXBOT_NODE=$tools/node/bin/node sh scripts/camoufox/sign.sh "$tools/camoufox" "$sign_id"
  staged=$(mktemp -d)
  cp -R "$tools/camoufox/browser/Camoufox.app" "$staged/"
  pack browser "$browser_version" "$staged"
  rm -rf "$staged"
else
  echo "make-packs: browser $browser_version (kept)"
fi

# The app's driver, naming the browser it runs: the pack's signed program.
driver=target/app-tools-$arch/camoufox
rm -rf "$driver"
mkdir -p "$(dirname "$driver")"
cp -R "$tools/camoufox" "$driver"
rm -rf "$driver/browser"
"$tools/node/bin/node" -e '
const fs = require("fs"), path = require("path"), crypto = require("crypto");
const [driver, browser, signed] = process.argv.slice(1);
const file = path.join(driver, "bundle.json");
const manifest = JSON.parse(fs.readFileSync(file, "utf8"));
const exe = path.join(browser, manifest.executable.replace(/^browser\//, ""));
manifest.executableSha256 = crypto.createHash("sha256").update(fs.readFileSync(exe)).digest("hex");
manifest.signed = signed !== "-";
fs.writeFileSync(file, JSON.stringify(manifest, null, 2) + "\n");
' "$driver" "$browser_dir" "$sign_id"

# The list the app carries.
entry() { # name version
  tar=$cache/$1-$2-$arch.tar.gz
  printf '{"name":"%s","version":"%s","sha256":"%s","size":%s}' "$1" "$2" \
    "$(shasum -a 256 "$tar" | cut -d' ' -f1)" "$(stat -f %z "$tar")"
}
list=target/packs-$arch.json
printf '{"schema":1,"packs":[%s,%s,%s]}\n' \
  "$(entry browser "$browser_version")" "$(entry codex "$codex_version")" \
  "$(entry node "$node_version")" >"$list"
echo "make-packs: $list"
echo "PACK_CACHE=$cache"
