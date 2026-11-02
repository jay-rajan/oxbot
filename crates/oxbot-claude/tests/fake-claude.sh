#!/bin/sh
# A stand-in for `claude` in tests: for every user line on stdin it prints the
# fixture named by FAKE_CLAUDE_FIXTURE; interrupts get a control_response.
# It records its arguments and one privacy variable to FAKE_CLAUDE_ARGS, and
# every line it reads to FAKE_CLAUDE_INPUT.
if [ -n "$FAKE_CLAUDE_ARGS" ]; then
  { echo "$@"; echo "DISABLE_TELEMETRY=$DISABLE_TELEMETRY"; } > "$FAKE_CLAUDE_ARGS"
fi
while IFS= read -r line; do
  if [ -n "$FAKE_CLAUDE_INPUT" ]; then printf '%s\n' "$line" >> "$FAKE_CLAUDE_INPUT"; fi
  case "$line" in
    *control_request*)
      id=$(printf '%s' "$line" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id" ;;
    *) cat "$FAKE_CLAUDE_FIXTURE" ;;
  esac
done
