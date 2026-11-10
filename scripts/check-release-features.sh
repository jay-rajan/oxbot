#!/bin/sh
# Checks that the app and the server are built without any crate's
# test-only features (every feature named `test-…`, such as oxbot-core's
# `test-people` and `test-support`). With one, a build could run every bot
# as before engines, with no owner-only rule. Dev-dependencies (tests) are
# left out on purpose. make-dmg.sh runs it before building.
set -e
cd "$(dirname "$0")/.."

bad=0
for package in oxbot-desktop oxbot; do
  if cargo tree -p "$package" -e normal,build,features 2>/dev/null |
    grep -E 'feature "test-' >/dev/null; then
    echo "$package is built with a test-only feature:" >&2
    cargo tree -p "$package" -e normal,build,features 2>/dev/null | grep -E 'feature "test-' | sort -u >&2
    bad=1
  fi
done
[ "$bad" = 0 ] && echo "No test-only features in the app or the server."
exit "$bad"
