#!/bin/sh
# oxbot's own Android settings, kept outside the generated project
# (app/src-tauri/gen/android), which `cargo tauri android init` rewrites.
#
#   scripts/android-apply.sh          put them back into the generated project
#   scripts/android-apply.sh --check  only say whether they're all there
#
# The settings: no backups or device-to-device copies of the app's data
# (the device key and the chat copy), the local-network permission Android 17
# needs, the camera for scanning the pairing code, and MainActivity asking
# for the local-network permission. `cargo test` checks them too.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
gen="$root/app/src-tauri/gen/android/app/src/main"
manifest="$gen/AndroidManifest.xml"
# The app id stays dev.oxbot.gate, from before the rename: a new id would be
# a different app, and phones would lose their pairing and chat copy.
activity="$gen/java/dev/oxbot/gate/MainActivity.kt"
ours="$root/app/src-tauri/android/MainActivity.kt"
mode=${1:-apply}

if [ ! -f "$manifest" ]; then
  echo "No Android project yet: run 'cargo tauri android init' in app/src-tauri first." >&2
  exit 1
fi

python3 - "$manifest" "$mode" <<'PY'
import re, sys
path, mode = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
wanted = text
perms = [
    ("android.permission.ACCESS_LOCAL_NETWORK", "oxbot talks only to the home computer on home Wi-Fi."),
    ("android.permission.CAMERA", "Only to scan the home computer's pairing QR code; frames never leave the phone."),
]
for name, why in perms:
    if f'android:name="{name}"' not in wanted:
        wanted = wanted.replace(
            '<uses-permission android:name="android.permission.INTERNET" />',
            f'<uses-permission android:name="android.permission.INTERNET" />\n    <!-- {why} -->\n    <uses-permission android:name="{name}" />',
            1,
        )
camera = '<uses-feature android:name="android.hardware.camera" android:required="false" />'
if camera not in wanted:
    wanted = wanted.replace('<uses-permission android:name="android.permission.CAMERA" />',
                            '<uses-permission android:name="android.permission.CAMERA" />\n    ' + camera, 1)
for attr, value in [("allowBackup", "false"), ("fullBackupContent", "false")]:
    if re.search(rf'android:{attr}="[^"]*"', wanted):
        wanted = re.sub(rf'android:{attr}="[^"]*"', f'android:{attr}="{value}"', wanted, count=1)
    else:
        wanted = wanted.replace("<application", f'<application\n        android:{attr}="{value}"', 1)
if wanted == text:
    sys.exit(0)
if mode == "--check":
    print("The Android manifest is missing oxbot's settings: run scripts/android-apply.sh", file=sys.stderr)
    sys.exit(1)
open(path, "w", encoding="utf-8").write(wanted)
print("Put oxbot's settings back into AndroidManifest.xml")
PY

if ! cmp -s "$ours" "$activity"; then
  if [ "$mode" = "--check" ]; then
    echo "MainActivity.kt differs from app/src-tauri/android/MainActivity.kt: run scripts/android-apply.sh" >&2
    exit 1
  fi
  mkdir -p "$(dirname "$activity")"
  cp "$ours" "$activity"
  echo "Put oxbot's MainActivity.kt back"
fi
