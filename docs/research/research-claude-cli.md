# oxbot: Claude Code CLI backend research

Date: 2026-10-27. Installed: Claude Code **2.1.311** at `~/.local/bin/claude`, signed in with claude.ai OAuth (`authMethod: claude.ai`, `subscriptionType: max`, `apiKeySource: none`).
Status legend: **[V]** verified on this machine; **[D]** from official docs (URL cited); **[S]** read from official SDK source; **[3P]** third-party/community.

Three real calls were made (all in `scratchpad/cctest`). Raw outputs: `scratchpad/sample1.jsonl`, `sample2.jsonl`, `sample3.jsonl`.

---

## 1. Headless mode and the stream-json wire format

### Flags (from `claude --help`, v2.1.311) [V]
| Flag | Notes |
|---|---|
| `-p, --print` | Non-interactive. Skips the workspace-trust dialog. Settings that fail validation are silently ignored. |
| `--output-format text\|json\|stream-json` | `stream-json` = NDJSON, one event per line. |
| `--input-format text\|stream-json` | `stream-json` = read NDJSON user messages + control requests from stdin ("realtime streaming input"). |
| `--verbose` | Required with `-p --output-format stream-json`. |
| `--include-partial-messages` | Adds `stream_event` lines (raw Anthropic SSE events: `message_start`, `content_block_start`, `content_block_delta` with `text_delta`, …). Needs `-p` + stream-json. |
| `--replay-user-messages` | Echoes stdin user messages back on stdout (`isReplay: true`) as an ack. Needs stream-json in and out. |
| `--include-hook-events`, `--forward-subagent-text` | Extra events for hooks and subagent text. |
| `--max-turns N`, `--max-budget-usd X` | Limits (budget only with `-p`). |
| `--json-schema '<schema>'` | Structured output to `structured_output`. |
| `--model`, `--fallback-model`, `--effort low..max` | Model control. |
| `--no-session-persistence` | Don't write a transcript (only with `-p`). |
| `--system-prompt`, `--append-system-prompt` (+ `-file` variants per docs) | Prompt control. **See the snapshot caveat in §2.** |
| `--system-prompt-snapshot on\|off` | Default `on`: the system prompt (incl. `--append-system-prompt`) is recorded on the first request and **reused verbatim on every resume even if a later launch passes different text**, until compaction. |
| `--bare` | Minimal mode, but **auth is API-key only (no OAuth/keychain)**, so it doesn't work with the subscription. |
| `--safe-mode` | Disables CLAUDE.md, skills, plugins, hooks, MCP servers, custom agents, etc.; auth, built-in tools and permissions still work. Usable with subscription. |

Docs: https://code.claude.com/docs/en/headless , https://code.claude.com/docs/en/cli-reference

### Real sample 1 (one-shot) [V]
`claude -p "reply with the word ok" --output-format stream-json --verbose --max-turns 1 --session-id 11111111-2222-4333-8444-555555555555` → exit 0, 4 lines:

```jsonc
// 1. system/init
{"type":"system","subtype":"init","cwd":"…/cctest","session_id":"11111111-…","tools":[25 names],
 "mcp_servers":[],"model":"claude-opus-5-5","permissionMode":"default","slash_commands":[…],
 "apiKeySource":"none","claude_code_version":"2.1.311","output_style":"default","agents":[…],"skills":[…],
 "plugins":[{"name":"cc-plugin-telemetry","path":"builtin",…},…],
 "capabilities":["interrupt_receipt_v1","interrupt_cancel_queued_v1","interrupt_send_now_v1",
   "interrupt_requeue_taken_v1","msg_lifecycle_v1",…],
 "uuid":"…","memory_paths":{…},"additional_directories":[],"fast_mode_state":"off",…}
// 2. assistant (full Anthropic Messages API message object)
{"type":"assistant","message":{"model":"claude-opus-5-5","id":"msg_…","role":"assistant",
  "content":[{"type":"text","text":"ok"}],"stop_reason":null,"usage":{"input_tokens":2,
  "cache_creation_input_tokens":7587,"cache_read_input_tokens":11383,"output_tokens":4,…}},
 "parent_tool_use_id":null,"session_id":"11111111-…","uuid":"…","timestamp":"…","request_id":"req_…"}
// 3. rate_limit_event (subscription windows!)
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour",
  "unifiedWindows":{"five_hour":{"utilization":0.03,…},"seven_day":{"utilization":0.05,…}}},…}
// 4. result
{"type":"result","subtype":"success","is_error":false,"result":"ok","num_turns":1,
 "session_id":"11111111-…","stop_reason":"end_turn","terminal_reason":"completed",
 "total_cost_usd":0.0630606,"usage":{…},"modelUsage":{"claude-opus-5-5":{…,"costUSD":…,
 "contextWindow":1000000}},"permission_denials":[],"duration_ms":1680,"ttft_ms":1664,…}
```
Tool use (not exercised here) appears as `assistant` messages with `tool_use` content blocks, followed by `user` messages whose content is `tool_result` blocks; subagent traffic carries a non-null `parent_tool_use_id` [D headless]. Other documented events: `system/api_retry`, `system/permission_denied` (with `--permission-prompts none`), `system/status`, `system/plugin_install`, hook events. Result subtypes include `success`, `error_max_turns`, `error_max_budget_usd`, `error_during_execution` [D sessions/agent-loop]. `total_cost_usd` is a client-side **list-price estimate**; on a subscription it is not what you pay. `rate_limit_event` is the useful signal on subscription.

### Real sample 2: long-lived multi-turn over stdin + resume [V]
```
claude -p --input-format stream-json --output-format stream-json --verbose --replay-user-messages \
  --resume 11111111-… --tools "" --strict-mcp-config --permission-mode dontAsk
stdin:  {"type":"user","message":{"role":"user","content":"What single word did you reply with earlier? …"}}
        (12 s later) {"type":"user","message":{"role":"user","content":"Now reply with the word two"}}
```
Output: `system/init` → replayed `user` → `assistant "ok"` → `result` → `system/init` → replayed `user` → `assistant "two"` → `result`. So:
- **One process handles many turns**; each turn emits its own `system/init` and `result`. Process exits when stdin closes (exit 0).
- **`--resume` restored context** (it remembered "ok" from call 1); `session_id` stayed the same (no fork). `total_cost_usd` is cumulative over the session.
- User message line format: `{"type":"user","message":{"role":"user","content": "<string>" | [content blocks incl. image base64]}}` (`parent_tool_use_id: null` optional; a `uuid` may be set) [D streaming-vs-single-mode].

### Real sample 3: interrupt + partial messages [V, partial]
Sent `{"type":"control_request","request_id":"req_int_1","request":{"subtype":"interrupt"}}` mid-session (Haiku, `--include-partial-messages`). Reply: `{"type":"control_response","response":{"subtype":"success","request_id":"req_int_1","response":{"still_queued":[]}}}`, then the next user message ran normally in the same process. Haiku finished the turn before the interrupt landed, so this verified the **wire format and that the process survives**, not the mid-generation cut. Partial events seen: `message_start`, `content_block_start`, `content_block_delta/text_delta`, `content_block_stop`, `message_delta`, `message_stop` (112 lines).

---

## 2. Sessions, transcripts and handing over a conversation

### Flags [V/D]
- `--session-id <uuid>`: choose the ID up front (verified: the init event echoed it). Good for oxbot: oxbot can mint the UUID and store it.
- `-r, --resume <id|name|/abs/path.jsonl>`: continue a specific session; works from any directory on the same machine (v2.1.223+) and also accepts the absolute path to a `.jsonl` [D headless].
- `-c, --continue`: most recent session in the cwd. Avoid in oxbot (ambiguous).
- `--fork-session` (with `--resume`/`--continue`): new session ID with a copy of history. SDK also has `resumeSessionAt <message uuid>` (CLI flag `--resume-session-at=` per SDK source) to truncate/branch [D typescript, S subprocess_cli.py].
- `--no-session-persistence` or env `CLAUDE_CODE_SKIP_PROMPT_HISTORY=1`: write nothing to disk.
- `CLAUDE_CONFIG_DIR`: relocate everything (settings, transcripts) e.g. per-oxbot config dir. Note credentials then need to be present there (or use `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`).

### On-disk transcripts [V/D]
- Path: `~/.claude/projects/<cwd with every non-alphanumeric → "-">/<session-id>.jsonl` (>200 chars: truncated + hash). Plaintext, kept 30 days by default (`cleanupPeriodDays`). [D sessions, data-usage]
- Format [V]: one JSON object per line, linked by `uuid`/`parentUuid`. Entry types seen in a single 1-turn session (161 KB!): `queue-operation`, `user`, `attachment` (environment, model identity, deferred tools, agent listing, skill listing, session_context, date, credential_org, prompt_snapshot…), `atis-latch`, `api-request-shape`, `api-request-blob`, `api-request`, `last-prompt`, `assistant`, `cost-state`. This is an **internal, versioned, undocumented format** (fields like `promptId`, `turnPosition`, `requestRef`, `shapeHash`).

### Can you seed/inject foreign history?
- **Writing a synthetic JSONL: technically `--resume /abs/path.jsonl` reads a file, but the format is internal and churns between versions.** Not recommended (risk: silent breakage on auto-update, cache/shape mismatches). No docs describe hand-authoring it.
- **SessionStore adapter (TS/Python SDK only)**: `load()` returns entries the SDK writes into a temp config dir, but docs say treat entries as **opaque** values the SDK itself appended; it's for mirroring, not importing. [D session-storage]
- **ACP**: no way for a client to supply history; `session/load` only replays agent-owned history. [D ACP session-setup]
- **Anthropic's own advice** for cross-host: "Don't rely on session resume. Capture the results you need … as application state and pass them into a fresh session's prompt. This is often more robust." [D sessions#resume-across-hosts]
- `claude import codex|gemini|cursor` exists but imports **config**, not conversations [V help].

### Recommended handover design for oxbot
1. oxbot owns the canonical transcript (its own schema: user/assistant text, tool calls+results summarised, files touched, decisions, open TODOs).
2. Per Claude conversation, oxbot keeps a `claude_session_id` (minted via `--session-id`).
3. **Switching TO Claude, first time**: start a new session; put the handover as the **first user message** (e.g. `<prior_conversation backend="codex">…condensed transcript…</prior_conversation>` + the new user request). Optionally a short stable instruction via `--append-system-prompt` ("You are continuing a conversation that began in another agent; the prior turns are in <prior_conversation>").
4. **Switching BACK to Claude later**: `--resume <claude_session_id>` and prepend only the **delta** (turns that happened in Codex/OpenRouter since Claude's last turn) to the next user message. This keeps Claude's own native history (tool results, cache) intact and avoids double-feeding.
5. Do **not** rely on changing `--append-system-prompt` between resumes: with the default `--system-prompt-snapshot on`, the first-request system prompt is reused verbatim on resume. Put dynamic context in user messages.
6. Large prior transcripts: write them to a file in the workspace (e.g. `.oxbot/handover.md`) and tell Claude to read it; stdin pipe is capped at 10 MB [D headless].
7. Cost/caching: a fresh session re-pays the prompt cache write for the handover; resume keeps the cache warm only within the cache TTL (sample shows 1h ephemeral cache).

---

## 3. Permissions and safety for unattended use

### Permission modes (`--permission-mode`) [V help, D headless/permission-modes]
`manual` (was "default"; since v2.1.302 `-p` runs start in Manual when nothing sets a mode: anything needing approval is **denied** unless a hook/permission tool approves), `acceptEdits` (file writes + mkdir/touch/mv/cp auto-approved), `auto` (classifier reviews actions), `dontAsk` (deny anything that would prompt; allow rules still apply), `plan` (read-only planning), `bypassPermissions` (everything; also `--dangerously-skip-permissions`).

### Other controls
- `--tools "Read,Edit,Bash"` = **which built-in tools exist** (`""` = none; verified `tools:[]`).
- `--allowedTools` / `--disallowedTools` = permission rules, e.g. `"Bash(git diff *)" "Edit"`; the space before `*` matters. `--disallowedTools WebFetch WebSearch` to block network tools.
- `--permission-prompts host|none`: `none` auto-denies anything that would prompt and tells Claude not to retry; removes `AskUserQuestion` [D headless].
- **Interactive approvals from oxbot UI**: pass `--permission-prompt-tool stdio`; the CLI then sends `{"type":"control_request","request_id":…,"request":{"subtype":"can_use_tool","tool_name":…,"input":{…},"tool_use_id":…,"permission_suggestions":…}}` on stdout; oxbot answers on stdin with `{"type":"control_response","response":{"subtype":"success","request_id":…,"response":{"behavior":"allow","updatedInput":{…}}}}` or `{"behavior":"deny","message":"…","interrupt":false}`. This is exactly what the official Python SDK does (`permission_prompt_tool_name="stdio"` when `can_use_tool` is set) [S types.py, _internal/query.py]. The control protocol is **used by the SDKs but not formally documented as a public spec**; treat as semi-stable and feature-detect via `system/init.capabilities`.
- Other control requests the SDK sends: `initialize`, `interrupt`, `set_permission_mode {mode}`, `set_model {model}`, `rewind_files`, `mcp_status`, `get_context_usage`, `mcp_reconnect`, `mcp_toggle`, `stop_task`. CLI→host requests: `can_use_tool`, `hook_callback`, `mcp_message`; CLI may send `control_cancel_request` [S].
- `--add-dir <dirs>`: extra dirs file tools may access. `--max-turns`, `--max-budget-usd`.
- `--mcp-config <json|file>` + `--strict-mcp-config` (ignore user/project `.mcp.json`) — verified `mcp_servers: []`.
- `--setting-sources user,project,local` to restrict which settings load; `--settings <file|json>` to inject oxbot's own settings (incl. sandbox and env).
- **Untrusted folders**: `-p` skips the trust dialog and **runs project hooks (`.claude/settings.json`) and connects `.mcp.json` servers** in any folder unless you prevent it [D headless]. For oxbot: `--safe-mode` or `--setting-sources user` + `--strict-mcp-config`.
- **`--restricted`**: removes Bash/code-running tools and WebFetch unless `--tools` names them, ignores user/project/local settings, confines file tools to working dirs, refuses bypassPermissions [V help].

### Sandbox (settings `sandbox.*`) [D https://code.claude.com/docs/en/sandboxing]
macOS Seatbelt wraps **Bash/PowerShell/Monitor and their children only**. Keys: `sandbox.enabled`, `sandbox.autoAllowBashIfSandboxed` (default true), `sandbox.allowUnsandboxedCommands` (set **false**), `sandbox.network.allowedDomains` (starts empty → no network), `sandbox.network.deniedDomains`, `sandbox.filesystem.allowWrite/denyWrite/allowRead/denyRead`, `sandbox.excludedCommands`. Network goes via a local proxy that checks hostnames. Read/Edit/Write/WebFetch/WebSearch, hooks, MCP servers are **not** sandboxed; they follow permission rules.

Suggested oxbot settings JSON (pass via `--settings`):
```json
{
  "sandbox": { "enabled": true, "allowUnsandboxedCommands": false,
               "network": { "allowedDomains": [] } },
  "permissions": { "deny": ["WebFetch", "WebSearch"] },
  "skipWebFetchPreflight": true,
  "env": { "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_TELEMETRY": "1",
           "DISABLE_ERROR_REPORTING": "1", "DISABLE_FEEDBACK_COMMAND": "1",
           "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1", "DISABLE_AUTOUPDATER": "1",
           "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL": "1" }
}
```
(Better to also set those env vars on the child process directly so they apply before settings load.)

### Auth and ToS [V/D]
- Works with the existing subscription login: no API key needed (verified `apiKeySource: none`, `authMethod: claude.ai`). For a headless daemon, `claude setup-token` creates a long-lived token usable as `CLAUDE_CODE_OAUTH_TOKEN` [D env-vars]. `--bare` can't use OAuth.
- Legal page (https://code.claude.com/docs/en/legal-and-compliance): OAuth is "intended exclusively for purchasers of Claude … subscription plans and is designed to support ordinary use of Claude Code"; developers building products "should use API key authentication … Anthropic does not permit third-party developers to offer Claude.ai login into their own applications, or to route requests through Free, Pro, or Max plan credentials on behalf of their users." It also says this does not "prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription." And: "Advertised usage limits for Pro and Max plans assume ordinary, individual usage of Claude Code and the Agent SDK."
- Agent SDK overview: "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK." Branding: can't call your product "Claude Code".
- **Reading for oxbot**: a personal tool on Jay's own Mac that spawns the **unmodified** `claude` binary signed in with **his own** Max login, for his own use, fits the "end user signing in to the unmodified binary" wording. It would **not** be OK to ship oxbot to others with it routing their requests through Jay's subscription, or to extract/reuse the OAuth token outside the binary. Always-on, high-volume automation could collide with "ordinary, individual usage" limits. Not legal advice; if oxbot ever becomes a product, switch to API keys.

---

## 4. Agent SDK, Rust options, and ACP

### Claude Agent SDK [D https://code.claude.com/docs/en/agent-sdk/overview]
- Official in **TypeScript** (`@anthropic-ai/claude-agent-sdk`) and **Python** (`claude-agent-sdk`) only. Both spawn the `claude` binary with `--output-format stream-json --verbose --input-format stream-json` and speak the control protocol over stdio [S subprocess_cli.py]. Docs: for other languages, "run the CLI as a subprocess with the `-p` flag".
- Extras vs raw CLI: `canUseTool` callback, hooks as callbacks, in-process SDK MCP tools, `SessionStore`, `listSessions`/`getSessionMessages`/`forkSession`, `interrupt()` with receipts.
- **No official Rust SDK.** Community crates [3P, unvetted]: `claude-code`, `claude-agent-sdk` / `cc-agent-sdk`, `claude_agents_sdk`, `apiari-claude-sdk`, `rusty_claw` (all wrap the CLI over stream-json). Given the protocol is small, oxbot can implement it directly in Rust (tokio `Command` + line-delimited serde_json) and use the Python SDK source as the reference.

### ACP (Agent Client Protocol, Zed) [D https://agentclientprotocol.com]
- JSON-RPC 2.0 over stdio; agent runs as subprocess. Methods: `initialize`, `authenticate`, `session/new {cwd, mcpServers}`, `session/prompt`, `session/cancel` (notification), optional `session/load` (replays history as `session/update`), `session/resume` (restores without replay), `session/set_mode`. Agent→client: `session/request_permission`, `session/update` (agent/user/thought chunks, tool_call + updates, plans, available commands, mode changes, compactions), optional `fs/read_text_file`, `fs/write_text_file`, `terminal/*`.
- **Claude adapter**: `@agentclientprotocol/claude-agent-acp` (https://github.com/agentclientprotocol/claude-agent-acp, Apache-2.0; formerly `@zed-industries/claude-code-acp`, then `@zed-industries/claude-agent-acp`). TypeScript, **built on the Claude Agent SDK** (so it still drives the `claude` binary; Zed docs say it vendors a CLI, overridable with `CLAUDE_CODE_EXECUTABLE`). Supports new/list/resume/close/prompt/cancel, permission requests, terminals, MCP, images, diffs; fork/delete experimental. Zed docs: login via `/login` with API key or Pro/Max subscription.
- **Codex adapter**: `agentclientprotocol/codex-acp` (Node/TS per repo files), auth via ChatGPT login or `CODEX_API_KEY`/`OPENAI_API_KEY`.
- **Rust crate**: `agent-client-protocol` on crates.io/docs.rs (v2.x; implement `Client` trait), powers Zed's integration.
- **Fit for oxbot**: attractive as a *uniform* layer (one Rust client for Claude + Codex, normalized events, permission requests, cancel). Downsides: adds a Node runtime + two npm adapters to the trust/egress surface; adapters lag CLI features; ACP has **no client-supplied history**, so the backend-switch handover still has to be a prompt (same as raw CLI); `session/load` support varies. Also note the ToS wording above about SDK-based agents and claude.ai login (personal use of own login through an Anthropic-SDK-based adapter is a greyer area than spawning the unmodified CLI directly).
- **Recommendation**: model oxbot's internal backend trait on ACP's concepts (session, prompt, update stream, permission request, cancel) but talk to Claude **directly via `claude -p` stream-json** (fewer moving parts, full flag control, no extra Node deps). Keep ACP as an optional backend later, e.g. via the `agent-client-protocol` crate.

---

## 5. Cancel/interrupt and data flow

### Interrupt [D headless + V]
- Preferred: stdin `{"type":"control_request","request_id":"…","request":{"subtype":"interrupt"}}` → `control_response` with `{"still_queued":[…]}` (verified), arrives before the interrupted turn's `result`. Optional `"cancel_queued": true` (capability `interrupt_cancel_queued_v1`) also drops queued messages. Process keeps running for the next turn (verified).
- **SIGINT** ends the current turn cleanly (docs: "To end the turn instead, send SIGINT … before you stop the process").
- **SIGTERM** → exit 143, turn left unfinished with no result; kills running Bash process trees; runs `SessionEnd` hooks. On resume the interrupted turn stays as-is (`CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` to continue it).
- Closing stdin ends the session after the current turn; `-p` may wait up to 10 min for background tasks (`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`).
- oxbot: spawn the child in its own process group; interrupt via control_request, fallback SIGINT, then SIGTERM after a timeout, then SIGKILL the group.

### What leaves the machine [D https://code.claude.com/docs/en/data-usage , env-vars]
| Traffic | Destination | Default (Max sign-in) | Off switch |
|---|---|---|---|
| Prompts, model output, tool results, file contents Claude reads | Anthropic API (TLS) | always (essential) | n/a |
| Usage metrics (no code/prompts/paths) | Anthropic **and third-party logging infrastructure** | on | `DISABLE_TELEMETRY=1` (also disables feature-flag fetch) or `DO_NOT_TRACK=1` |
| Error reports (redacted stack traces) | **third-party error tracking service** | on for Pro/Max on v2.1.198+ | `DISABLE_ERROR_REPORTING=1` |
| `/feedback`, `/bug`, `/share` transcripts | Anthropic (stored in Google Cloud Storage; optional public GitHub issue) | user-initiated | `DISABLE_FEEDBACK_COMMAND=1` |
| Session quality survey + optional transcript share | Anthropic | on (interactive) | `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` |
| Auto-update, release notes, PR badge, fast-mode checks, feature flags | Anthropic distribution/API | on | `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` (covers all of the above incl. telemetry/errors/feedback); `DISABLE_AUTOUPDATER=1`, `DISABLE_UPDATES=1` |
| Official plugin marketplace auto-install | marketplace source (GitHub) | on | `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1` (not covered by NONESSENTIAL) |
| WebFetch domain safety check (hostname only) | `api.anthropic.com` | on, not covered by NONESSENTIAL | `skipWebFetchPreflight: true` in settings |
| **WebFetch / WebSearch tools** | **arbitrary third-party sites** / search provider | available | `--disallowedTools WebFetch WebSearch` (or `--tools` without them; `CLAUDE_CODE_DISABLE_WEB_FETCH=1` disables WebFetch only) |
| **Bash commands** (curl, git, npm…) | anywhere | allowed if permitted | sandbox with empty `network.allowedDomains`, `allowUnsandboxedCommands:false` |
| MCP servers (user/project `.mcp.json`, claude.ai connectors) | whatever the server talks to | loaded if configured | `--strict-mcp-config` with oxbot-supplied (or empty) `--mcp-config`, or `--safe-mode` |
| Project hooks / plugins | whatever they do | run under `-p` even in untrusted dirs | `--safe-mode` or `--setting-sources` |

Note: "NONESSENTIAL", `DISABLE_TELEMETRY`, `DISABLE_ERROR_REPORTING` are presence flags: `0`/`false` still disables; unset to re-enable. Disabling feature flags makes Remote Control unavailable (irrelevant to oxbot). The init event lists a builtin `cc-plugin-telemetry` plugin; with the env vars set it should be inert, but verify with a network monitor (e.g. Little Snitch / `nettop`) once.

Consumer data policy (Max): training on your data only if the claude.ai privacy toggle is on; retention 30 days if off, 5 years if on. Check https://claude.ai/settings/data-privacy-controls. Local transcripts are plaintext under `~/.claude/projects/` for 30 days.

**Under oxbot's "Anthropic + OpenAI only" rule, required for the Claude backend:** set `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` (+ explicit `DISABLE_TELEMETRY=1`, `DISABLE_ERROR_REPORTING=1` belt-and-braces), `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1`, deny `WebFetch`/`WebSearch`, `--strict-mcp-config`, sandbox Bash with no network, and `--safe-mode` (or tight `--setting-sources`) so repo hooks/plugins/MCP can't add egress.

---

## Recommended spawn line for oxbot (per Claude conversation)
```
env CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 \
    DISABLE_FEEDBACK_COMMAND=1 CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 DISABLE_AUTOUPDATER=1 \
    CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1 \
claude -p --input-format stream-json --output-format stream-json --verbose \
  --include-partial-messages --replay-user-messages \
  (--session-id <uuid> | --resume <uuid>) \
  --permission-mode acceptEdits --permission-prompt-tool stdio \
  --disallowedTools WebFetch WebSearch \
  --strict-mcp-config --safe-mode \
  --settings /path/to/oxbot-claude-settings.json \
  --max-budget-usd <cap> --model <alias>
```
cwd = the project workspace; `--add-dir` for extras. Keep one child per active Claude conversation; idle-kill after N minutes and `--resume` on next message.

## Open risks
1. The stdin control protocol (`control_request`/`can_use_tool`/`interrupt`) is SDK-internal, not a documented public spec; may change. Mitigate: feature-detect `capabilities` in `system/init`, pin/disable auto-update (`DISABLE_AUTOUPDATER`), integration tests on upgrade.
2. Transcript JSONL is internal; never hand-author it. Handover = prompt text.
3. Mid-generation interrupt wasn't observed (the turn finished first); test with a long tool-using turn.
4. `--permission-prompt-tool stdio` + `acceptEdits`/`--safe-mode` combination not run live here; verify once.
5. ToS: fine for Jay's own use of the unmodified binary with his own login; not for distributing oxbot to others on his subscription; heavy always-on automation may hit "ordinary individual usage" limits (watch `rate_limit_event`).
6. Egress: the builtin telemetry plugin and any undocumented calls should be confirmed with a network monitor after setting the env vars.
7. `--bare` (the documented "recommended for scripts" mode) is unusable with the subscription because it is API-key only.

## Sources
- CLI help output, `claude auth status` (local, v2.1.311)
- https://code.claude.com/docs/en/headless
- https://code.claude.com/docs/en/cli-reference
- https://code.claude.com/docs/en/agent-sdk/overview
- https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode
- https://code.claude.com/docs/en/agent-sdk/sessions
- https://code.claude.com/docs/en/agent-sdk/session-storage
- https://code.claude.com/docs/en/agent-sdk/typescript
- https://code.claude.com/docs/en/legal-and-compliance
- https://code.claude.com/docs/en/data-usage
- https://code.claude.com/docs/en/env-vars
- https://code.claude.com/docs/en/sandboxing
- https://github.com/anthropics/claude-agent-sdk-python (src/claude_agent_sdk/_internal/query.py, _internal/transport/subprocess_cli.py, types.py)
- https://agentclientprotocol.com/protocol/overview , https://agentclientprotocol.com/protocol/session-setup , https://agentclientprotocol.com/get-started/agents
- https://github.com/agentclientprotocol/claude-agent-acp , https://github.com/agentclientprotocol/codex-acp , https://zed.dev/docs/ai/external-agents
- https://docs.rs/agent-client-protocol , https://agentclientprotocol.com/libraries/rust.md
- Community Rust crates: https://docs.rs/claude-code , https://docs.rs/claude-agent-sdk , https://docs.rs/apiari-claude-sdk , https://docs.rs/rusty_claw
