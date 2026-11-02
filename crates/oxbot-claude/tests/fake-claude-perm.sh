#!/bin/sh
# A stand-in for `claude` that asks oxbot first. For every user message it
# reads a page snapshot, then, by FAKE_TOOL:
#   upload (default): asks to upload cv.pdf from its folder;
#   post: asks to click the page's "Post" button;
#   signin: asks to call oxbot's request_sign_in, then calls it;
#   createbot: asks to call create_bot ("Mail Checker"), then calls it;
#   task: asks to call give_task (bot "helper", "say hi"), then calls it;
#   list: asks to call list_bots, then calls it;
#   read: asks to call read_reply (bot "helper"), then calls it;
#   report: asks to call save_report with FAKE_REPORT (JSON arguments; by
#   default one source, the page it reads), then calls it;
#   none: just replies "ok".
# FAKE_TOOLS (e.g. "list,task") picks the tool for each message in turn.
# FAKE_HELPER_TOOLS does the same for a bot whose folder is "helper".
# FAKE_TARGET names the bot for task and read instead of "helper".
# With FAKE_PRIVATE set, create_bot asks for a private bot.
# With FAKE_NO_PAGE set it skips the page snapshot, so its turn reads no web
# page before the tool. With FAKE_PAGE_ONCE (a file path) set, only the first
# message (across restarts, and across bots sharing the path) reads the page.
# With FAKE_READ_FILE set it reads a file in its folder before the tool.
# It replies with oxbot's answer ("behavior=allow" / "behavior=deny", or
# "signed_in=true|false" for a sign-in, "tool_ok=true|false" for the other
# oxbot tools, with the tool's text after it), and reports plan limits and
# usage. FAKE_LIMITS (e.g. "0.4,0.55") sets both windows' usage for each
# message in turn, resetting 3 hours and 3 days from the fake's start;
# FAKE_MODEL is the model it says it uses.
tool=${FAKE_TOOL:-upload}
model=${FAKE_MODEL:-fake}
start=$(date +%s)
five_at=$((start + 10800))
week_at=$((start + 259200))
target=${FAKE_TARGET:-helper}
report=${FAKE_REPORT:-'{"title":"Jobs","summary":"One job fits.","markdown":"The apply page has one job [1].","sources":[{"url":"https://jobs.example.com/apply","title":"Jobs"}]}'}
case "$PWD" in
  */helper/*|*/helper) [ -n "$FAKE_HELPER_TOOLS" ] && FAKE_TOOLS=$FAKE_HELPER_TOOLS ;;
esac
cb_input='{"name":"Mail Checker","instructions":"Check my mail."}'
if [ -n "$FAKE_PRIVATE" ]; then
  cb_input='{"name":"Mail Checker","instructions":"Check my mail.","private":true}'
fi
n=0
finish() {
  printf '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c%s","content":"done"}]},"parent_tool_use_id":null}\n' "$n"
  if [ -n "$FAKE_LIMITS" ]; then
    used=$(printf '%s' "$FAKE_LIMITS" | cut -d, -f"$n")
    printf '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","unifiedWindows":{"five_hour":{"utilization":%s,"resetsAt":%s,"status":"allowed"},"seven_day":{"utilization":%s,"resetsAt":%s,"status":"allowed"}}}}\n' "$used" "$five_at" "$used" "$week_at"
  else
  printf '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","unifiedWindows":{"five_hour":{"utilization":0.25,"resetsAt":1793207400,"status":"allowed"},"seven_day":{"utilization":0.5,"resetsAt":1793754000,"status":"allowed"}}}}\n'
  fi
  printf '{"type":"assistant","message":{"content":[{"type":"text","text":"%s"}]},"parent_tool_use_id":null}\n' "$1"
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s","duration_ms":5,"total_cost_usd":0.01,"usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}\n' "$1"
}
while IFS= read -r line; do
  case "$line" in
    *'"mcp_response"'*)
      text=$(printf '%s' "$line" | sed 's/\\"//g' | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
      case "$tool:$line" in
        signin:*'"isError":false'*) finish "signed_in=true" ;;
        signin:*'"isError":true'*) finish "signed_in=false" ;;
        *'"isError":false'*) finish "tool_ok=true $text" ;;
        *) finish "tool_ok=false $text" ;;
      esac ;;
    *'"type":"control_response"'*)
      behavior=$(printf '%s' "$line" | sed 's/.*"behavior":"\([a-z]*\)".*/\1/')
      if [ "$tool" = signin ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"request_sign_in","arguments":{"url":"https://mail.example.com/login","why":"to read your mail"}}}}}\n' "$n" "$n"
      elif [ "$tool" = createbot ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"create_bot","arguments":%s}}}}\n' "$n" "$n" "$cb_input"
      elif [ "$tool" = list ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"list_bots","arguments":{}}}}}\n' "$n" "$n"
      elif [ "$tool" = task ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"give_task","arguments":{"bot":"%s","message":"say hi"}}}}}\n' "$n" "$n" "$target"
      elif [ "$tool" = report ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"save_report","arguments":%s}}}}\n' "$n" "$n" "$report"
      elif [ "$tool" = read ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"read_reply","arguments":{"bot":"%s"}}}}}\n' "$n" "$n" "$target"
      else
        finish "behavior=$behavior"
      fi ;;
    *'"type":"control_request"'*)
      id=$(printf '%s' "$line" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id" ;;
    *'"type":"user"'*)
      n=$((n+1))
      if [ -n "$FAKE_TOOLS" ]; then
        tool=$(printf '%s' "$FAKE_TOOLS" | cut -d, -f"$n")
      fi
      page=1
      if [ -n "$FAKE_NO_PAGE" ]; then
        page=
      elif [ -n "$FAKE_PAGE_ONCE" ]; then
        if [ -e "$FAKE_PAGE_ONCE" ]; then page=; else : > "$FAKE_PAGE_ONCE"; fi
      fi
      printf '{"type":"system","subtype":"init","session_id":"s","model":"%s"}\n' "$model"
      if [ -n "$FAKE_READ_FILE" ]; then
      printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"f%s","name":"Read","input":{"file_path":"notes.txt"}}]},"parent_tool_use_id":null}\n' "$n"
      printf '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"f%s","content":"notes"}]},"parent_tool_use_id":null}\n' "$n"
      fi
      if [ -n "$page" ]; then
      printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s%s","name":"mcp__chrome-devtools__take_snapshot","input":{}}]},"parent_tool_use_id":null}\n' "$n"
      printf '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"s%s","content":"## Pages\\n1: Jobs (https://jobs.example.com/apply) [selected]\\n## Latest page snapshot\\nuid=1_1 RootWebArea \\"Jobs\\"\\n  uid=1_2 button \\"Upload CV\\"\\n  uid=1_3 button \\"Post\\""}]},"parent_tool_use_id":null}\n' "$n"
      fi
      case "$tool" in
        none)
          finish "ok" ;;
        post)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__chrome-devtools__click","input":{"uid":"1_3"}}]},"parent_tool_use_id":null}\n' "$n"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__chrome-devtools__click","input":{"uid":"1_3"},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        signin)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__request_sign_in","input":{"url":"https://mail.example.com/login","why":"to read your mail"}}]},"parent_tool_use_id":null}\n' "$n"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__request_sign_in","input":{"url":"https://mail.example.com/login","why":"to read your mail"},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        createbot)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__create_bot","input":%s}]},"parent_tool_use_id":null}\n' "$n" "$cb_input"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__create_bot","input":%s,"tool_use_id":"c%s"}}\n' "$n" "$cb_input" "$n" ;;
        task)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__give_task","input":{"bot":"%s","message":"say hi"}}]},"parent_tool_use_id":null}\n' "$n" "$target"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__give_task","input":{"bot":"%s","message":"say hi"},"tool_use_id":"c%s"}}\n' "$n" "$target" "$n" ;;
        read)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__read_reply","input":{"bot":"%s"}}]},"parent_tool_use_id":null}\n' "$n" "$target"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__read_reply","input":{"bot":"%s"},"tool_use_id":"c%s"}}\n' "$n" "$target" "$n" ;;
        report)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__save_report","input":%s}]},"parent_tool_use_id":null}\n' "$n" "$report"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__save_report","input":%s,"tool_use_id":"c%s"}}\n' "$n" "$report" "$n" ;;
        list)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__list_bots","input":{}}]},"parent_tool_use_id":null}\n' "$n"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__list_bots","input":{},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        *)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__chrome-devtools__upload_file","input":{"uid":"1_2","filePaths":["%s/cv.pdf"]}}]},"parent_tool_use_id":null}\n' "$n" "$PWD"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__chrome-devtools__upload_file","input":{"uid":"1_2","filePaths":["%s/cv.pdf"]},"tool_use_id":"c%s"}}\n' "$n" "$PWD" "$n" ;;
      esac ;;
  esac
done
