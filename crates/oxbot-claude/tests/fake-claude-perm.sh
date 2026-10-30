#!/bin/sh
# A stand-in for `claude` that asks oxbot first. For every user message it
# reads a page snapshot, then, by FAKE_TOOL:
#   upload (default): asks to upload cv.pdf from its folder;
#   post: asks to click the page's "Post" button;
#   signin: asks to call oxbot's request_sign_in, then calls it;
#   createbot: asks to call create_bot ("Mail Checker"), then calls it;
#   task: asks to call give_task (bot "helper", "say hi"), then calls it;
#   list: asks to call list_bots, then calls it.
# It replies with oxbot's answer ("behavior=allow" / "behavior=deny", or
# "signed_in=true|false" for a sign-in, "tool_ok=true|false" for the other
# oxbot tools, with the tool's text after it), and reports plan limits and
# usage.
tool=${FAKE_TOOL:-upload}
n=0
finish() {
  printf '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c%s","content":"done"}]},"parent_tool_use_id":null}\n' "$n"
  printf '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","unifiedWindows":{"five_hour":{"utilization":0.25,"resetsAt":1793207400,"status":"allowed"},"seven_day":{"utilization":0.5,"resetsAt":1793754000,"status":"allowed"}}}}\n'
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
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"create_bot","arguments":{"name":"Mail Checker","instructions":"Check my mail."}}}}}\n' "$n" "$n"
      elif [ "$tool" = list ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"list_bots","arguments":{}}}}}\n' "$n" "$n"
      elif [ "$tool" = task ] && [ "$behavior" = allow ]; then
        printf '{"type":"control_request","request_id":"m%s","request":{"subtype":"mcp_message","server_name":"oxbot","message":{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"give_task","arguments":{"bot":"helper","message":"say hi"}}}}}\n' "$n" "$n"
      else
        finish "behavior=$behavior"
      fi ;;
    *'"type":"control_request"'*)
      id=$(printf '%s' "$line" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id" ;;
    *'"type":"user"'*)
      n=$((n+1))
      printf '{"type":"system","subtype":"init","session_id":"s","model":"fake"}\n'
      printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s%s","name":"mcp__chrome-devtools__take_snapshot","input":{}}]},"parent_tool_use_id":null}\n' "$n"
      printf '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"s%s","content":"## Pages\\n1: Jobs (https://jobs.example.com/apply) [selected]\\n## Latest page snapshot\\nuid=1_1 RootWebArea \\"Jobs\\"\\n  uid=1_2 button \\"Upload CV\\"\\n  uid=1_3 button \\"Post\\""}]},"parent_tool_use_id":null}\n' "$n"
      case "$tool" in
        post)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__chrome-devtools__click","input":{"uid":"1_3"}}]},"parent_tool_use_id":null}\n' "$n"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__chrome-devtools__click","input":{"uid":"1_3"},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        signin)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__oxbot__request_sign_in","input":{"url":"https://mail.example.com/login","why":"to read your mail"}}]},"parent_tool_use_id":null}\n' "$n"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__request_sign_in","input":{"url":"https://mail.example.com/login","why":"to read your mail"},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        createbot)
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__create_bot","input":{"name":"Mail Checker","instructions":"Check my mail."},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        task)
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__give_task","input":{"bot":"helper","message":"say hi"},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        list)
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__oxbot__list_bots","input":{},"tool_use_id":"c%s"}}\n' "$n" "$n" ;;
        *)
          printf '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c%s","name":"mcp__chrome-devtools__upload_file","input":{"uid":"1_2","filePaths":["%s/cv.pdf"]}}]},"parent_tool_use_id":null}\n' "$n" "$PWD"
          printf '{"type":"control_request","request_id":"r%s","request":{"subtype":"can_use_tool","tool_name":"mcp__chrome-devtools__upload_file","input":{"uid":"1_2","filePaths":["%s/cv.pdf"]},"tool_use_id":"c%s"}}\n' "$n" "$PWD" "$n" ;;
      esac ;;
  esac
done
