#!/bin/sh
# Puts a release built by release.sh on GitHub Releases (jay-rajan/oxbot),
# in an order that never points anyone at a file that isn't there yet:
#   1. the version's own release (vVERSION, a pre-release while in beta):
#      the .dmgs, the updates and their signatures, the .apk, checksums;
#   2. any new packs, into the fixed "packs" release (each made once);
#   3. last, the feed (beta.json in the fixed "feed" release), which is what
#      every oxbot checks: from then on they update.
#
# Usage: publish-release.sh VERSION   (the release commit pushed first)
set -e
cd "$(dirname "$0")/.."
version=$1
[ -n "$version" ] || { echo "usage: publish-release.sh VERSION" >&2; exit 2; }
repo=jay-rajan/oxbot
out=target/release-out/$version
[ -f "$out/beta.json" ] || { echo "publish: no release built in $out (release.sh $version)" >&2; exit 1; }
commit=$(git rev-parse HEAD)
git fetch -q origin
git merge-base --is-ancestor "$commit" origin/main ||
  { echo "publish: push $commit to main first" >&2; exit 1; }

# 1 · The version's release
set -- "$out"/oxbot_"$version"_*
echo "publish: v$version ($# files)"
gh release create "v$version" --repo "$repo" --target "$commit" --prerelease \
  --title "oxbot $version" --notes-file "$out/notes.md" "$@"

# 2 · New packs (the fixed "packs" release holds every version's)
fixed() { # tag title notes
  gh release view "$1" --repo "$repo" >/dev/null 2>&1 ||
    gh release create "$1" --repo "$repo" --prerelease --title "$2" --notes "$3"
}
fixed packs "oxbot packs" "The browser, Codex and Node.js, downloaded by oxbot's updates. Not for installing by hand."
have=$(gh release view packs --repo "$repo" --json assets -q '.assets[].name')
for f in "$out"/packs/*.tar.gz; do
  name=$(basename "$f")
  if printf '%s\n' "$have" | grep -qx "$name"; then
    echo "publish: pack $name (already there)"
  else
    echo "publish: pack $name"
    gh release upload packs "$f" --repo "$repo"
  fi
done

# 3 · The feed, last
fixed feed "oxbot update feed" "What oxbot checks for updates. Not for installing by hand."
gh release upload feed "$out/beta.json" --repo "$repo" --clobber
echo "publish: done. https://github.com/$repo/releases/tag/v$version"
