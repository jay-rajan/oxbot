#!/bin/sh
# Gets the tools that ship inside oxbot.app, so friends install nothing:
# Node.js and the pinned chrome-devtools-mcp (the browser tool, from
# scripts/tools/package-lock.json), and Codex, which runs every bot.
#
# Usage: fetch-tools.sh [arm64|x64]  (default: this Mac's chip)
# Output: target/tools-bundle/{node/bin/node, node/LICENSE, mcp/node_modules,
#         codex/bin/codex, codex/LICENSE, codex/NOTICE, …}
# make-dmg.sh copies it into oxbot.app/Contents/Resources/tools.
#
# Every download is checked against the checksums pinned here; change a
# version and its checksums together. Codex must stay on the version
# oxbot speaks (oxbot-codex: binary::SUPPORTED).
set -e
cd "$(dirname "$0")/.."

NODE_VERSION=24.22.2
NODE_SHA_ARM64=aa9549349e47dffaa42dcf95d6e06ade8c4bdafc980979883b88767670df385a
NODE_SHA_X64=d5e336d588c45be0c37dbd52cc29895593009ca4997a77a1fd02815131ed0e52

# Codex's own release for each chip, from npm (OpenAI's package), checked
# against npm's sha512; its license and notice from the same release tag.
CODEX_VERSION=0.148.0
CODEX_SHA512_ARM64='xgBPFiF1fHUlRS7HE6wGB56LjBJh16kGD7b4TTbwdVBZNB4QDkTok+vdkAGrfpVkfKcwGNhPSKDgCw+KMZOVug=='
CODEX_SHA512_X64='qepQolhJutfOp+e9i7L3xsi8aoWeCUiiRq274WMWqRj50rKTrXxsuAgkAwDbqEfT3G5VynhYZuQvDsW37JgdNQ=='
CODEX_LICENSE_SHA=d17f227e4df5da1600391338865ce0f3055211760a36688f816941d58232d8dc
CODEX_NOTICE_SHA=9d71575ecfd9a843fc1677b0efb08053c6ba9fd686a0de1a6f5382fd3c220915

arch=${1:-$(uname -m)}
case "$arch" in
  arm64 | aarch64) arch=arm64 triple=aarch64-apple-darwin node_sha=$NODE_SHA_ARM64 codex_sha=$CODEX_SHA512_ARM64 ;;
  x64 | x86_64) arch=x64 triple=x86_64-apple-darwin node_sha=$NODE_SHA_X64 codex_sha=$CODEX_SHA512_X64 ;;
  *) echo "fetch-tools: unknown chip '$arch' (arm64 or x64)" >&2; exit 2 ;;
esac

out=target/tools-bundle
cache=target/tools-cache
mkdir -p "$cache"

download() { # url file
  if [ ! -f "$cache/$2" ]; then
    curl -fsSL -o "$cache/$2.part" "$1"
    mv "$cache/$2.part" "$cache/$2"
  fi
}
bad() { # file
  echo "checksum mismatch for $1" >&2
  rm -f "$cache/$1"
  exit 1
}
check_sha256() { # sha file
  echo "$1  $cache/$2" | shasum -a 256 -c - >/dev/null || bad "$2"
}

# Node.js
node_file="node-v$NODE_VERSION-darwin-$arch.tar.gz"
download "https://nodejs.org/dist/v$NODE_VERSION/$node_file" "$node_file"
check_sha256 "$node_sha" "$node_file"

# Codex
codex_file="codex-$CODEX_VERSION-darwin-$arch.tgz"
download "https://registry.npmjs.org/@openai/codex/-/$codex_file" "$codex_file"
[ "$(openssl dgst -sha512 -binary "$cache/$codex_file" | base64)" = "$codex_sha" ] || bad "$codex_file"
for f in LICENSE NOTICE; do
  download "https://raw.githubusercontent.com/openai/codex/rust-v$CODEX_VERSION/$f" "codex-$CODEX_VERSION-$f"
done
check_sha256 "$CODEX_LICENSE_SHA" "codex-$CODEX_VERSION-LICENSE"
check_sha256 "$CODEX_NOTICE_SHA" "codex-$CODEX_VERSION-NOTICE"

rm -rf "$out"
mkdir -p "$out/node" "$out/mcp" "$out/codex"
tar -xzf "$cache/$node_file" -C "$out/node" --strip-components=1 \
  "node-v$NODE_VERSION-darwin-$arch/bin/node" "node-v$NODE_VERSION-darwin-$arch/LICENSE"

# Codex's release folder as it ships (the program, its helper, ripgrep and
# zsh beside it), kept with OpenAI's signatures.
tar -xzf "$cache/$codex_file" -C "$out/codex" --strip-components=3 "package/vendor/$triple"
cp "$cache/codex-$CODEX_VERSION-LICENSE" "$out/codex/LICENSE"
cp "$cache/codex-$CODEX_VERSION-NOTICE" "$out/codex/NOTICE"

# The browser tool, exactly as locked. No install scripts run.
cp scripts/tools/package.json scripts/tools/package-lock.json "$out/mcp/"
(cd "$out/mcp" && npm ci --omit=dev --ignore-scripts --no-audit --no-fund --loglevel=error)

# What can run here is checked; the other chip's programs only by type.
if [ "$arch" = "$(uname -m | sed 's/x86_64/x64/')" ]; then
  "$out/node/bin/node" --version >/dev/null
  "$out/codex/bin/codex" --version | grep -q "$CODEX_VERSION"
fi
lipo -archs "$out/node/bin/node"
lipo -archs "$out/codex/bin/codex"
echo "Built $out for $arch ($(du -sh "$out" | cut -f1))"
