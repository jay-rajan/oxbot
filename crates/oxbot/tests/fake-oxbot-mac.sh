#!/bin/sh
# A stand-in for oxbot's Mac helper in tests: it can always dictate, and
# "hears" two pieces of text, then the whole sentence once stdin closes.
case "$1" in
  can-dictate) echo '{"ok":true,"locale":"en_US"}' ;;
  dictate)
    echo '{"text":"Remind me","final":false}'
    echo '{"text":"Remind me to call","final":false}'
    cat >/dev/null
    echo '{"text":"Remind me to call the plumber","final":true}' ;;
  *) echo '{"error":"not in tests"}'; exit 1 ;;
esac
