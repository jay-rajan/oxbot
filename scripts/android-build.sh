#!/bin/sh
# Builds the Android device app (debug APK for arm64 phones), with oxbot's
# Android settings in place first. Install it with:
#   adb install -r app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/scripts/android-env.sh"
"$root/scripts/android-apply.sh"
cd "$root/app/src-tauri"
cargo tauri android build --debug --apk --target aarch64
