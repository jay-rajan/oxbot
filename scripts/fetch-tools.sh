#!/bin/sh
# Gets the browser tool that ships inside oxbot.app, so friends don't
# install Node.js: a universal Node.js (Apple chip + Intel) and the pinned
# chrome-devtools-mcp from scripts/tools/package-lock.json.
#
# Output: target/tools-bundle/{node/bin/node, node/LICENSE, mcp/node_modules}
# make-dmg.sh copies it into oxbot.app/Contents/Resources/tools.
#
# Node.js comes from nodejs.org and is checked against the checksums pinned
# here; change both together when moving to a newer Node.js.
set -e
cd "$(dirname "$0")/.."

NODE_VERSION=24.22.2
NODE_SHA_ARM64=aa9549349e47dffaa42dcf95d6e06ade8c4bdafc980979883b88767670df385a
NODE_SHA_X64=d5e336d588c45be0c37dbd52cc29895593009ca4997a77a1fd02815131ed0e52

out=target/tools-bundle
cache=target/tools-cache
mkdir -p "$cache"

fetch_node() { # arch sha
  file="node-v$NODE_VERSION-darwin-$1.tar.gz"
  if [ ! -f "$cache/$file" ]; then
    curl -fsSL -o "$cache/$file.part" "https://nodejs.org/dist/v$NODE_VERSION/$file"
    mv "$cache/$file.part" "$cache/$file"
  fi
  echo "$2  $cache/$file" | shasum -a 256 -c - >/dev/null || {
    echo "checksum mismatch for $file" >&2
    rm -f "$cache/$file"
    exit 1
  }
  rm -rf "$cache/$1"
  mkdir -p "$cache/$1"
  tar -xzf "$cache/$file" -C "$cache/$1" --strip-components=1
}

fetch_node arm64 "$NODE_SHA_ARM64"
fetch_node x64 "$NODE_SHA_X64"

rm -rf "$out"
mkdir -p "$out/node/bin" "$out/mcp"
lipo -create "$cache/arm64/bin/node" "$cache/x64/bin/node" -output "$out/node/bin/node"
chmod +x "$out/node/bin/node"
cp "$cache/arm64/LICENSE" "$out/node/LICENSE"

# The tool, exactly as locked. No install scripts run.
cp scripts/tools/package.json scripts/tools/package-lock.json "$out/mcp/"
(cd "$out/mcp" && npm ci --omit=dev --ignore-scripts --no-audit --no-fund --loglevel=error)

"$out/node/bin/node" --version >/dev/null
lipo -archs "$out/node/bin/node"
echo "Built $out ($(du -sh "$out" | cut -f1))"
