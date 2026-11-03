#!/bin/sh
# Checks that the app and the server are built without oxbot-core's
# test-only features. With `test-people` (or `test-support`), a build
# could run every bot as before engines, with no owner-only rule for
# Claude bots. Dev-dependencies (tests) are left out on purpose.
set -e
cd "$(dirname "$0")/.."

bad=0
for package in oxbot-desktop oxbot; do
  if cargo tree -p "$package" -e normal,build,features 2>/dev/null |
    grep -E 'oxbot-core feature "(test-people|test-support)"' >/dev/null; then
    echo "$package is built with one of oxbot-core's test-only features" >&2
    bad=1
  fi
done
[ "$bad" = 0 ] && echo "No test-only features in the app or the server."
exit "$bad"
