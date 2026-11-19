#!/bin/sh
# Builds an oxbot release on this Mac, ready to publish (publish-release.sh):
# tested, both Mac chips signed and notarized, the updates signed with the
# update key, the Android device app signed, and the beta feed.
#
# Usage: release.sh VERSION [--skip-tests]   (the workspace must already
#        say VERSION in Cargo.toml, committed)
# Output: target/release-out/VERSION/
#   oxbot_VERSION_{apple-chip,intel}.dmg (+ .sha256)    new installs
#   oxbot_VERSION_{apple-chip,intel}.app.tar.gz (+ .sig) the updates
#   oxbot_VERSION_android.apk (+ .sha256)               the phone app
#   packs/<pack>-<version>-<chip>.tar.gz               every pack, made once
#   beta.json                                          the feed
#   notes.md                                           a draft to edit
#
# Keys, all on this Mac and none in the repo:
# - Developer ID and the notary profile "oxbot" (make-dmg.sh).
# - The update key: OXBOT_UPDATE_KEY (default ~/.oxbot-release/update.key),
#   made once with `cargo tauri signer generate`; its password in the
#   Keychain as "oxbot update key". Its public half is in
#   app/src-tauri/tauri.conf.json.
# - The Android key: OXBOT_ANDROID_KEYSTORE (default
#   ~/.oxbot-release/android.jks, alias "oxbot"); its password in the
#   Keychain as "oxbot android key". Without it there's no .apk.
# Passwords are read from the Keychain (macOS asks you to allow it) and
# passed through the environment, never printed.
set -e
cd "$(dirname "$0")/.."
version=$1
[ -n "$version" ] || { echo "usage: release.sh VERSION [--skip-tests]" >&2; exit 2; }
tests=yes
[ "${2:-}" = --skip-tests ] && tests=no
say() { echo "release: $*"; }
fail() { echo "release: $*" >&2; exit 1; }

# 1 · Ready?
[ "$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)" = "$version" ] ||
  fail "Cargo.toml doesn't say version $version"
[ -z "$(git status --porcelain --untracked-files=no)" ] || fail "commit your changes first"
git tag | grep -qx "v$version" && fail "v$version is already tagged"
update_key=${OXBOT_UPDATE_KEY:-$HOME/.oxbot-release/update.key}
[ -f "$update_key" ] || fail "no update key at $update_key (cargo tauri signer generate -w $update_key)"
pubkey=$(target/tools-bundle/node/bin/node -e 'console.log(require("./app/src-tauri/tauri.conf.json").plugins.updater.pubkey)')
[ -n "$pubkey" ] || fail "tauri.conf.json has no update key (plugins.updater.pubkey)"
[ "$pubkey" = "$(cat "$update_key.pub")" ] || fail "tauri.conf.json's update key isn't $update_key.pub"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=$(security find-generic-password -s "oxbot update key" -w) ||
  fail "no \"oxbot update key\" password in the Keychain"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
out=target/release-out/$version
rm -rf "$out"
mkdir -p "$out/packs"

# 2 · Test
if [ "$tests" = yes ]; then
  say "testing"
  OXBOT_CODEX=$PWD/target/tools-bundle/codex/bin/codex cargo test --workspace --exclude oxbot-e2e --locked -q
  cargo clippy --workspace --all-targets --locked -q -- -D warnings
  (cd crates/oxbot/ui-src && npx tsc --noEmit -p . && npx vitest run && npm run build)
  bash scripts/check-ui.sh
fi

# 3 · Mac: both chips, signed and notarized (the .dmg and the slim app)
sh scripts/make-dmg.sh arm64 x64
feed_entry() { # chip label tauri-target
  bundle=target/$1/release/bundle
  cp "$bundle/dmg/oxbot_${version}_$2.dmg" "$bundle/dmg/oxbot_${version}_$2.dmg.sha256" "$out/"
  update=$out/oxbot_${version}_$2.app.tar.gz
  COPYFILE_DISABLE=1 tar -czf "$update" -C "$bundle/slim" oxbot.app
  cargo tauri signer sign -f "$update_key" --app-version "$version" "$update" >/dev/null
  printf '"%s":{"signature":"%s","url":"https://github.com/jay-rajan/oxbot/releases/download/v%s/%s"}' \
    "$3" "$(cat "$update.sig")" "$version" "$(basename "$update")"
}
arm=$(feed_entry aarch64-apple-darwin apple-chip darwin-aarch64)
intel=$(feed_entry x86_64-apple-darwin intel darwin-x86_64)
cache=${OXBOT_PACK_CACHE:-$HOME/Library/Caches/oxbot-release/packs}
for chip in arm64 x64; do
  target/tools-bundle/node/bin/node -e '
for (const p of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).packs)
  console.log(p.name + "-" + p.version);' "$PWD/target/packs-$chip.json" |
    while read -r pack; do cp "$cache/$chip/$pack-$chip.tar.gz" "$out/packs/"; done
done

# 4 · Android: the device app, signed with your key
keystore=${OXBOT_ANDROID_KEYSTORE:-$HOME/.oxbot-release/android.jks}
if [ -f "$keystore" ]; then
  say "building the Android app"
  OXBOT_ANDROID_PASS=$(security find-generic-password -s "oxbot android key" -w) ||
    fail "no \"oxbot android key\" password in the Keychain"
  export OXBOT_ANDROID_PASS
  (
    . scripts/android-env.sh
    scripts/android-apply.sh
    cd app/src-tauri && cargo tauri android build --apk --target aarch64
  )
  tools=$(ls -d "$HOME/Library/Android/sdk/build-tools/"* | sort -V | tail -1)
  unsigned=app/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk
  apk=$out/oxbot_${version}_android.apk
  "$tools/zipalign" -f -p 4 "$unsigned" "$apk.aligned"
  "$tools/apksigner" sign --ks "$keystore" --ks-key-alias oxbot --ks-pass env:OXBOT_ANDROID_PASS \
    --out "$apk" "$apk.aligned"
  rm -f "$apk.aligned" "$apk.idsig"
  "$tools/apksigner" verify "$apk"
  (cd "$out" && shasum -a 256 "$(basename "$apk")" >"$(basename "$apk").sha256")
else
  say "no Android key at $keystore: no .apk in this release"
fi

# 5 · The feed and the notes
last=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
{
  echo "## oxbot $version"
  echo
  echo "Download the .dmg for your Mac: **apple-chip** for M1 and later, **intel** for older Macs."
  echo "Phones: install the oxbot app (Android: the .apk here), then pair it from Settings → People."
  echo
  echo "### What changed"
  git log --format='- %s' ${last:+"$last..HEAD"} | head -40
} >"$out/notes.md"
printf '{"version":"%s","notes":"See the release page.","pub_date":"%s","platforms":{%s,%s}}\n' \
  "$version" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$arm" "$intel" >"$out/beta.json"

say "done: $out"
ls -lh "$out" "$out/packs" | sed 's/^/  /'
say "edit $out/notes.md, then: sh scripts/publish-release.sh $version"
