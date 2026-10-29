# oxbot: research on cross-backend switching, data boundary, and Rust stack

Researched 2026-10-27. All crate versions come from the crates.io API (`/api/v1/crates/<name>`, `max_stable_version`) on that date. Paid model calls: none. Keys read: none.

**Hard constraint (updated by the user):** data may leave the LAN only to **Anthropic (Claude)** and **OpenAI (Codex)**. OpenRouter is a third party that sees prompts, so it is **excluded**. Any relay run by someone else (Happy's hosted server, Omnara Cloud, tunnels like Pinggy or ngrok) also breaks the rule.

---

## 0. TL;DR recommendation

1. **Three backend adapters behind one Rust trait**, all local processes or HTTPS to an allowed vendor:
   - `ClaudeCli`: spawns the unmodified `claude -p --output-format stream-json …` with Jay's own subscription login. This is explicitly allowed (see §3.3).
   - `CodexAppServer`: spawns `codex app-server` (JSON-RPC over stdio). It supports `thread/resume`, per-turn `model`, and **`thread/inject_items`**, which writes prior history into a thread. That makes it the best handoff target.
   - `OwnLoop`: oxbot's own tool-calling loop. It speaks to (a) **local models** over the OpenAI-compatible `/v1/chat/completions` of Ollama, LM Studio, llama.cpp or MLX (nothing leaves the Mac), and optionally (b) the Anthropic Messages API or OpenAI Responses API **with API keys** (subscription OAuth is not allowed for oxbot's own loop; see §3.3).
2. **oxbot's SQLite event log is the canonical transcript.** Each backend keeps its own native session id per conversation. To switch, oxbot resumes the native session if one exists and adds a "delta since you last saw this" preamble. Otherwise it starts fresh with a condensed handoff preamble. The shared working directory and git state, plus an oxbot-written `HANDOFF.md`, are the real shared memory.
3. **Phone ↔ Mac on the LAN only:** HTTPS/WSS served by axum and rustls with a self-signed cert from rcgen. Discovery uses mDNS (`mdns-sd`). Pairing uses a QR code that carries the URL, the cert SHA-256 fingerprint and a one-time token; the bearer token is kept in the macOS Keychain. **No third-party relay.** For access away from home, use a VPN to the home LAN (e.g. WireGuard on your own router). A hosted relay would break the constraint. Anthropic's own Remote Control is allowed, but only for Claude (§1).
4. **Turn telemetry off** in both CLIs (§3.4). Error reports from Claude Code go to a *third-party* error tracker, and metrics also go to "third-party logging infrastructure", so `DISABLE_TELEMETRY=1` and `DISABLE_ERROR_REPORTING=1` are **required** under the constraint, not optional.
5. ACP (`agent-client-protocol` 3.5.0) is worth tracking and could serve as oxbot's *internal* backend interface. But the Claude and Codex ACP adapters are Node packages, and Codex's native `app-server` is richer for handoff. Recommendation: speak native protocols in v1 and model the internal trait on ACP so ACP agents can be added later.

---

## 1. Prior art: multi-backend frontends and mobile control

| Tool | Claude CLI + Codex CLI? | How it switches backend/model | Mobile story | Data path / fits the constraint? |
|---|---|---|---|---|
| **Agent Client Protocol (ACP)**, by Zed | Yes, through adapters: `zed-industries/claude-agent-acp` (built on the Claude Agent SDK, Apache-2.0) and `agentclientprotocol/codex-acp`. Also Goose, OpenCode, Gemini CLI, Cursor, Copilot and others. | **Model** switch inside one agent's session: `session/set_config_option` with `configId: "model"`, "can be changed at any point during a session". **Switching agents inside one session is not part of the spec.** The client must handle it. Session resume, list, close and fork exist (resume and fork via RFDs; fork is unstable). | None built in. Local transport is JSON-RPC over stdio. HTTP and WebSocket remote transport is "a work in progress" (an RFD exists). | Protocol only. Data goes wherever the agent sends it. ✅ |
| **Agent Communication Protocol** (IBM/BeeAI) | n/a. This is agent-to-agent, not editor-to-agent. | n/a | n/a | Merged into Google's **A2A** in Aug 2025 under the Linux Foundation and is no longer standalone. Not relevant to oxbot. |
| **Goose** (Block, Rust) | Yes: `claude-acp` and `codex-acp` providers. The older `claude-code` and `codex` CLI providers are **deprecated**. | Provider is set by `GOOSE_PROVIDER` before a session starts. Release v1.24 added "mid-session model switching" for the old Claude Code provider. **ACP providers: `goose session resume` and `fork` are not supported yet.** Subscription login is reused from the CLIs. | Desktop app plus CLI. No official phone client found. | Local. ✅ |
| **OpenCode** (sst) | Its own agent. Claude and OpenAI models through provider APIs (75+ providers through the Vercel AI SDK), plus Ollama. It also implements ACP. | `/model` switches provider and model inside the same conversation; the history is re-sent to the new provider. **This is the closest analog to oxbot's `OwnLoop` switching.** | Client/server design; the TUI is one frontend. A third-party project (Happier) provides mobile clients. | Depends on provider. ✅ only with Anthropic, OpenAI or local providers. |
| **Crush** (Charm) | Its own agent, with OpenAI- and Anthropic-compatible providers. | README: you can "switch LLMs mid-session while preserving context". It doesn't say how. | TUI only. | ✅ with allowed providers. |
| **aider** | Its own loop; providers through LiteLLM. | `/model <name>` switches the main model mid-chat. History is kept unless `/clear` (inferred from the docs, not stated). | No. | ✅ with allowed providers. |
| **LiteLLM** | A proxy or SDK, not a frontend. | Routes one OpenAI-shaped API to many providers. | n/a | Self-hosted proxy telemetry is **off by default** (v1.107+). OTEL tracing captures prompts if you configure an exporter. Possible as a local gateway, but oxbot doesn't need it. |
| **Conductor** (Melty Labs, Mac) | Yes, with its **own bundled** Claude Code and Codex binaries. Bring your own subscription or API key. | A separate git worktree for each agent or workspace. Agents run in parallel; it doesn't hand off a conversation. | Mac only. "Conductor Cloud" now exists. | Local app. Privacy page not verified. |
| **Vibe Kanban** (BloopAI) | Yes; each agent must be installed and logged in locally. | One git worktree per task. You choose the agent per task attempt. | Web UI. | BloopAI reportedly shut down in Apr 2026, so it is community-maintained and local-only now (secondary sources). |
| **CloudCLI / claudecodeui** (siteboon) | Claude Code, Cursor CLI and Codex (some listings add OpenCode and Gemini). | Reads `~/.claude/projects/` sessions. Express plus WebSocket. | PWA in the phone browser. Remote access goes through a **tunnel such as Pinggy**. | ✅ on LAN. ❌ if a third-party tunnel is used. GPL-3.0. |
| **Happy** (slopus, happy.engineering) | Yes: `happy claude`, `happy codex`; the desktop "Happy Harness" also covers Grok. | The CLI wraps the TUI. Handing control to the phone restarts the session in "remote mode", and a keypress takes it back. Multi-model sessions are in the desktop app. | **Phone app (Expo) ↔ `happy-server` relay ↔ CLI on the laptop.** QR pairing. E2E encrypted with TweetNaCl (curve25519-xsalsa20-poly1305 and secretbox); docs also mention AES-256-GCM for session messages. | ❌ by default: the hosted relay `happy-api.slopus.com` is a third party, even though it only sees ciphertext. ✅ only if you self-host `happy-server` (MIT; `Dockerfile.server`). **Best prior art for pairing and E2E design.** |
| **Omnara** | Started as "Claude Code & Codex mobile" (iOS app). The README now describes its own agent loop that "works with any model", plus machines that "connect out to Omnara". | Its own loop. | iOS, web, Apple Watch; push notifications and approvals. | ❌ on Omnara Cloud (app.omnara.com; workers and the Postgres event log are on their side). ✅ only if self-hosted with Docker Compose (Apache-2.0; "local defaults are intentionally insecure"). |
| **Claude Code Remote Control** (Anthropic, first party) | Claude only. | n/a | The Claude iOS/Android app or claude.ai/code drives a **local** `claude` session. Outbound HTTPS only, no inbound ports. | Relay is **Anthropic**. "While Remote Control is connected, the session transcript … is stored on Anthropic servers." ✅ under the constraint (Anthropic is allowed), Claude only. It needs feature flags, so on v2.1.154–v2.1.282 `DISABLE_TELEMETRY` blocks it. Newer versions allow it unless the org requires Trusted Devices. |

**Takeaways for oxbot**
- **No existing tool switches Claude CLI ↔ Codex CLI inside one conversation.** OpenCode, Crush and aider switch *models* inside their *own* loop by re-sending history. Conductor and Vibe Kanban switch by *task or worktree*. ACP leaves cross-agent handoff to the client. oxbot has to build it.
- "Phone controls agent on my laptop" prior art: Happy (relay plus E2E), Omnara (cloud), CloudCLI (LAN web plus tunnel), and Anthropic Remote Control (Anthropic relay). Only a **LAN-direct** design (CloudCLI-style, with Happy-style QR pairing) meets "Anthropic/OpenAI only" for both backends.

---

## 2. Handoff strategies (A → B mid-conversation)

### Facts that constrain the design
- **Codex app-server** (official docs): `thread/start` takes `model`, `cwd`, `approvalPolicy` and `sandbox`. `thread/resume` takes a `threadId`; if you resume with a different model, "Codex warns and applies a one-time model-switch instruction on the next turn." `thread/fork` copies history up to a `lastTurnId`. `turn/start` takes `model`, `effort` and `cwd`, and these become the defaults for later turns. **`thread/inject_items` appends prebuilt Responses-API items to a loaded thread's model-visible history without starting a turn; they are persisted and sent on later requests.** `turn/start` also accepts `toolOutput`. Events: `turn/started|completed`, `item/started|completed` (the authoritative final state), `item/agentMessage/delta`. Approvals arrive as server requests (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`). Transports: stdio (default), Unix socket, and WebSocket (experimental).
- **Claude Code** stores sessions as plaintext JSONL under `~/.claude/projects/` (30-day default cleanup, `cleanupPeriodDays`). It resumes by session id. There is **no supported way to inject foreign tool-call history**. The only ways in are text in the user prompt or `--append-system-prompt`. Exact flags are covered by the CLI-flags agent.
- **Vendor tool-call items cannot be moved between vendors.** Claude `tool_use`/`tool_result` blocks and OpenAI `function_call`/`function_call_output` items have different ids and schemas, and reasoning items are encrypted or vendor-specific. Even with `inject_items`, injecting *synthetic* Codex tool calls invented from Claude's run is risky. Inject them as **messages** (text summaries) instead.

### Strategy comparison

| Strategy | How | Pros | Cons | Token cost |
|---|---|---|---|---|
| **(a) Condensed replay preamble** | Build a summary from the canonical log: goal, decisions, files touched, open TODOs, the last N verbatim user/assistant turns. Send it as the first prompt or appended system prompt for B. For Codex, use `thread/inject_items` with message items. | Works with any backend, including local models. Simple. | Loses fine detail. The summary costs a model call, unless you summarize locally with a local model or deterministically. | Roughly 2–8k tokens per switch, compared with re-sending a full 50–150k transcript. |
| **(b) Native-session resume** | Store `native_session_id` per (conversation, backend). When switching back to A, resume A's session and send only the **delta** (turns and file changes B made since A's last turn). | A keeps its full native context, tool history and prompt-cache warmth. Cheapest when switching back and forth. | Native sessions expire: Claude's local transcripts are cleaned after 30 days by default, and Codex rollouts are kept until history limits. A's context is stale about the files, so the delta must say "files X, Y changed; re-read before editing". | Delta only, usually under 2k tokens. |
| **(c) Shared workspace as memory** | The same `cwd` for every backend. oxbot writes `.oxbot/HANDOFF.md` (goal, plan, decisions, done/next, open questions) and records `git` HEAD and diff stat at each switch. B is told to read it. | The real state (code) is always correct. Each agent re-derives detail with its own tools, so no vendor history needs translating. Survives crashes. | Agents spend tool calls re-reading. Uncommitted work can confuse them. HANDOFF.md must not leak into commits (add `.oxbot/` to `.git/info/exclude`). | Reads happen through the agent's tools, so they cost tokens on B's side but no preamble. |

**Recommended: combine all three.** On switch to B: (1) snapshot the workspace with a git stash-like ref or `git diff --stat` plus the HEAD sha in a `file_change` event; (2) regenerate `.oxbot/HANDOFF.md` from the log; (3) if B has a native session for this conversation, resume it and send the **delta preamble**, otherwise start fresh with the **condensed preamble**. Both preambles point to HANDOFF.md. Tool calls from A are rendered as one-line text ("ran `cargo test` → 3 failed: …", "edited src/main.rs +12/-3"), never as native tool items.

### Canonical event model (SQLite)

```sql
CREATE TABLE conversation (
  id TEXT PRIMARY KEY,            -- ULID
  title TEXT, cwd TEXT NOT NULL,
  active_backend TEXT NOT NULL,   -- 'claude' | 'codex' | 'local:<model>' | 'anthropic_api' | 'openai_api'
  created_at INTEGER, updated_at INTEGER
);
CREATE TABLE native_session (     -- (b) per-backend native ids
  conversation_id TEXT, backend TEXT, native_id TEXT NOT NULL,
  model TEXT, last_event_seq INTEGER NOT NULL,  -- canonical seq this backend has "seen"
  created_at INTEGER, PRIMARY KEY (conversation_id, backend)
);
CREATE TABLE event (
  conversation_id TEXT NOT NULL, seq INTEGER NOT NULL,   -- monotonic per conversation
  ts INTEGER NOT NULL, backend TEXT, model TEXT, turn_id TEXT,
  kind TEXT NOT NULL CHECK (kind IN (
    'user_msg','assistant_msg','assistant_delta',  -- deltas optional / compacted away
    'reasoning_summary','tool_call','tool_result','approval_request','approval_decision',
    'file_change','backend_switch','handoff_summary','usage','error','turn_end')),
  call_id TEXT,                 -- links tool_call ↔ tool_result
  body JSON NOT NULL,           -- normalized payload (text, tool name+args, exit code, path+diffstat…)
  raw JSON,                     -- original vendor event, for debugging / re-normalization
  PRIMARY KEY (conversation_id, seq)
);
CREATE INDEX event_kind ON event(conversation_id, kind);
```
- `backend_switch` body: `{from, to, reason, git_head, dirty_files[], handoff_event_seq}`.
- `handoff_summary` stores the exact preamble sent, so you can audit what B was told.
- Delta for a backend that resumes = events with `seq > native_session.last_event_seq` from *other* backends, rendered as text.
- Run SQLite in WAL mode. Use one writer task that owns the connection and receives `mpsc` messages; reads go through a small pool.

### Rust trait sketch
```rust
#[async_trait]
trait Backend: Send + Sync {
    fn id(&self) -> BackendId;
    async fn start_or_resume(&self, conv: &Conversation, native: Option<&NativeSession>,
                             preamble: Option<String>) -> Result<SessionHandle>;
    async fn send_turn(&self, h: &SessionHandle, input: UserInput)
        -> Result<BoxStream<'static, CanonicalEvent>>;      // normalized events
    async fn answer_approval(&self, h: &SessionHandle, req: ApprovalId, d: Decision) -> Result<()>;
    async fn cancel(&self, h: &SessionHandle) -> Result<()>;
}
```
The trait follows ACP's shape (session, prompt, update stream, permission request, cancel), so an `AcpBackend` can be added later with the `agent-client-protocol` crate.

---

## 3. Keeping data on the network

### 3.1 What leaves the Mac, to whom

| Path | What leaves | To whom | Retention / training (cited) | Allowed under the constraint? |
|---|---|---|---|---|
| `claude -p` on a Pro/Max subscription | All prompts, model outputs, file contents and tool output read into context (TLS 1.2+) | Anthropic API | Consumer terms: **30 days** if "improve models" is off, **5 years** if on. Set this at claude.ai/settings/data-privacy-controls. | ✅ (turn training **off**) |
| `claude` telemetry: metrics | Latency, reliability and usage; "never include your code, prompts, or file paths" | Anthropic **and third-party logging infrastructure** | n/a | ❌ goes to a third party → `DISABLE_TELEMETRY=1` |
| `claude` error reports (Pro/Max, v2.1.198+) | Stack traces with secrets and paths redacted | **Third-party error-tracking service** | n/a | ❌ → `DISABLE_ERROR_REPORTING=1` |
| `claude` `/feedback`, `/bug`, `/share`, transcript-share survey | Full transcript including code | Anthropic (Google Cloud Storage); can open a **public GitHub issue** | 5 years for /feedback; 6 months for survey transcripts | Keep off → `DISABLE_FEEDBACK_COMMAND=1`, `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` |
| `claude` WebFetch preflight | Only the hostname | Anthropic | n/a | ✅ (Anthropic). Not affected by NONESSENTIAL_TRAFFIC. |
| WebFetch or WebSearch tools themselves (either CLI) | URLs and queries | **The fetched site**, and search providers via the vendor | n/a | ⚠️ Leaves to third parties by design. Deny WebFetch/WebSearch in permissions if strict. |
| Claude Remote Control (optional) | Full transcript, synced | Anthropic servers while connected | Data-usage policy | ✅ (Anthropic). Don't need it if oxbot has its own LAN UI. |
| `codex exec` / `app-server` with ChatGPT sign-in | Prompts, outputs, context | OpenAI | Follows the ChatGPT workspace's retention. For individual plans, content may train models unless you opt out (Data Controls; also covers Codex). Thumbs up/down feedback can be used for training even after opting out. | ✅ (opt out of training) |
| Codex anonymous analytics | Usage and health metrics (`auth_mode`, `model`, `app.version`; internal tool names only) | OpenAI | n/a | ✅ OpenAI, but you can turn it off: `[analytics] enabled = false` |
| Codex `/feedback` | Logs and session data | OpenAI | n/a | Turn it off: `[feedback] enabled = false` |
| Codex OTEL | Nothing by default (`exporter = "none"`; `log_user_prompt = false`) | (your collector) | n/a | ✅ |
| oxbot `OwnLoop` → Anthropic Messages API (API key) | Prompts and outputs | Anthropic | Commercial: deleted "within 30 days of receipt or generation"; not used for training. ZDR by approval. | ✅ |
| oxbot `OwnLoop` → OpenAI Responses API (API key) | Prompts and outputs | OpenAI | Abuse-monitoring logs up to 30 days; no training on API data. `store` defaults to keeping response objects about 30 days, so set `store: false`. ZDR by approval. Codex itself relies on `store:true` (per its README). | ✅ |
| oxbot `OwnLoop` → Ollama / LM Studio / llama.cpp / MLX on localhost | Nothing (inference is local) | n/a | Ollama FAQ: "We don't see your prompts or data when you run locally." | ✅✅ Fully local. **But** Ollama `:cloud` models send prompts to ollama.com. Set `OLLAMA_NO_CLOUD=1` or `"disable_ollama_cloud": true` in `~/.ollama/server.json`. Ollama on macOS auto-downloads updates (fetching an update isn't prompt data). |
| `codex --oss` (Ollama / LM Studio) | Inference is local. Analytics still go to OpenAI unless disabled. | OpenAI (metrics only) | n/a | ✅ |
| `claude` pointed at Ollama (`ANTHROPIC_BASE_URL=http://localhost:11434`, Ollama ≥ v0.14 Messages-API compatibility) | Inference is local. Telemetry still goes unless disabled. | n/a | n/a | ✅ An option for "Claude Code harness on a local model". Quality is lower and it needs 32k+ context. |
| **OpenRouter** | Everything | OpenRouter **and** the upstream provider | n/a | ❌ **Excluded** |
| Phone ↔ oxbot | Everything | Stays on the LAN (TLS, pinned self-signed cert) | Local SQLite | ✅. A hosted relay or third-party tunnel would be ❌. |
| Push notifications to the phone (if added) | Notification payload | Apple APNs | n/a | ❌ Apple is a third party. Send content-free pushes ("oxbot needs you") or use none: the PWA polls over the LAN. |

**Stays on the Mac:** the oxbot SQLite log, `~/.claude/projects/*.jsonl` (plaintext, 30 days), `~/.codex/history.jsonl` and session rollouts, the working tree, Keychain secrets, and local-model inference.

### 3.2 Environment and config oxbot should enforce when spawning the CLIs
**Claude Code** (env on the child process, or `settings.json` `env`):
```
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1   # metrics + error reports + feedback + surveys + feature flags
DISABLE_TELEMETRY=1
DISABLE_ERROR_REPORTING=1
DISABLE_FEEDBACK_COMMAND=1
CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
DISABLE_AUTOUPDATER=1        # optional; updates come from Anthropic's distribution server anyway
```
Side effect: `NONESSENTIAL_TRAFFIC` and `DISABLE_TELEMETRY` also turn off feature-flag evaluation, which can disable Remote Control. That doesn't matter if oxbot provides the remote UI.
**Codex** (`~/.codex/config.toml`, or a dedicated `CODEX_HOME` for oxbot):
```toml
cli_auth_credentials_store = "keyring"   # top-level key (must come before any [table]); don't keep plaintext ~/.codex/auth.json
[analytics]
enabled = false
[feedback]
enabled = false
[otel]
exporter = "none"
log_user_prompt = false
# optional
[history]
persistence = "none"     # oxbot's SQLite is canonical
```

### 3.3 Can oxbot's own loop reuse the subscriptions? (cited)
- **Claude:** **No for the own loop.** The Claude Code legal page says OAuth (Free/Pro/Max) is "designed to support ordinary use of Claude Code and other native Anthropic applications". Developers, "including those using the Agent SDK, should use API key authentication", and developers "may not collect, store, or intermediate Claude.ai credentials or session tokens". **Yes for driving the CLI:** the same page says this "does [not] prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription". oxbot spawning Jay's own logged-in, unmodified `claude` binary is therefore fine. Reading `~/.claude` OAuth tokens and calling `api.anthropic.com` directly is not. Support article 15036540: Agent SDK, `claude -p` and third-party-app use **draw from the subscription's usage limits**. A separate monthly Agent SDK credit announced for 15 June 2026 was **paused**.
- **OpenAI:** **No documented path for the own loop.** Codex auth docs list ChatGPT sign-in (subscription), API key (billed at API rates) and Enterprise access tokens. They say "General OpenAI API calls should use Platform API keys." Community tools that reuse ChatGPT tokens hit an undocumented `chatgpt.com/backend-api/codex/responses` endpoint, which is unsupported and fragile. Avoid it. Driving `codex app-server` with Jay's ChatGPT login is the supported way to use the subscription.
- So: **subscription use = through the CLIs; own loop = API keys (pay per token) or local models.**

### 3.4 Telemetry summary (sources: code.claude.com/docs/en/data-usage; learn.chatgpt.com config-advanced)
- Claude Code defaults with Anthropic API or subscription: metrics **on**, error reports **on** (Pro/Max, v2.1.198+), /feedback **on**, surveys **on**, WebFetch hostname preflight **on**. With Bedrock or Vertex, metrics, errors and /feedback are off by default.
- Codex defaults: anonymous analytics **on**, `/feedback` **on**, OTEL **off**, history persistence **on** (`~/.codex/history.jsonl`), credentials stored in plaintext `~/.codex/auth.json` or the OS store.

---

## 4. Rust crates (crates.io, 2026-10-27)

| Need | Crate @ version | Notes |
|---|---|---|
| HTTP/WS server | `axum 0.8.9`, `axum-extra 0.12.6` | Built-in WebSocket support (`ws` feature). SSE via `axum::response::sse`. |
| Runtime | `tokio 1.54.1` (`full`), `tokio-util 0.7.20`, `tokio-stream 0.1.19`, `futures 0.3.34` | Use `tokio::process::Command` with `kill_on_drop(true)`. Read stdout with `FramedRead` + `LinesCodec` for NDJSON (both CLIs). |
| Middleware | `tower-http 0.7.1` | `cors`, `trace`, `compression`, `fs` (serve the PWA), `auth`. |
| TLS | `rustls 0.23.46`, `rcgen 0.14.10`, `axum-server 0.8.0` (`tls-rustls`) | rcgen makes a self-signed cert for `oxbot.local` plus LAN IPs. Put its SHA-256 in the pairing QR so the phone pins it. `rustls-acme` 0.15.4 isn't useful on a LAN with no public DNS. |
| mDNS | `mdns-sd 0.21.6` | Advertise `_oxbot._tcp.local.`. Pure Rust, no Bonjour dependency. Recent iOS needs the app to declare `NSBonjourServices`; a browser PWA can't browse mDNS, so give the PWA the URL in the QR code. |
| SQLite | `rusqlite 0.41.0` (`bundled`) + `tokio-rusqlite 0.8.0` **or** `sqlx 0.9.0` (`sqlite`) | **Recommendation: rusqlite + tokio-rusqlite.** One writer and simple SQL, with full SQLite features (JSON1, FTS5 for searching transcripts), fewer dependencies and faster builds. sqlx gives compile-time-checked queries and async pooling, but at a cost to macro build time. It's fine if you prefer it. Migrations: `rusqlite_migration 2.6.0` or `refinery 0.10.0`. |
| Serde | `serde 1.0.229`, `serde_json 1.0.154` | Tag vendor events with `#[serde(tag = "type")]` and add `#[serde(other)]` plus a `raw: Value` fallback so new CLI event types don't crash the parser. |
| Logging | `tracing 0.1.44`, `tracing-subscriber 0.3.23` | **Never log prompt bodies at info level.** Redact by default. |
| IDs and time | `ulid 3.0.0` or `uuid 1.29.0` (v7), `jiff 0.2.38` or `chrono 0.4.45` | |
| OpenAI-compatible client (local models + OpenAI) | `async-openai 0.42.2` | `OpenAIConfig::new().with_api_base("http://localhost:11434/v1").with_api_key("ollama")` works (`with_api_base` and `with_header` confirmed in `src/config.rs`). It supports the **Responses API** (`client.responses()`), and the `byot` feature lets you send or receive your own JSON when local servers differ from the spec. Alternative: raw `reqwest 0.13.5` plus an SSE parser. Chat-completions plus tools is a small surface; raw reqwest gives full control over odd local-server streaming quirks. A good path: start with async-openai and fall back to `byot`. |
| Anthropic Messages client | No official Rust SDK. Community: `claudius 0.33.0` (2026-06), `adk-anthropic 2.3.0` (2026-10, a fork of claudius), `aigw-anthropic 0.7.0` | Or raw reqwest: `POST /v1/messages` with `stream: true` is simple. **Avoid** `anthropic-sdk-rust 0.1.1` (2025, stale), `async-anthropic 0.6.0` (2025-05, stale), `turboclaude` (43 recent downloads). |
| SSE parsing (raw reqwest path) | `eventsource-stream 0.2.3` (2022, stable and tiny) or hand-roll it | `reqwest-eventsource 0.6.0` is from 2024 and pins older reqwest; check compatibility with reqwest 0.13 first. |
| ACP | `agent-client-protocol 3.5.0` (2026-10-22) | Runtime-neutral. Features: `process` (spawn agent subprocesses via `AcpAgent`), `stdio`, `unstable_protocol_v2`, `unstable_session_fork`. v1 is stable. The major version moves fast (2.x → 3.5 within 2026), so pin exactly. Adapters are npm: `claude-agent-acp`, `codex-acp`. |
| Keychain | `keyring 4.2.0` **or, more simply,** `keyring-core 1.0.1` + `apple-native-keyring-store 1.0.2` | keyring v4 split into `keyring-core` plus per-platform stores (the docs mention `v1` and `cli` modes). For a Mac-only app, use `apple-native-keyring-store` directly, or `security-framework 3.7.0` for full control. Note: a binary launched by launchd still triggers Keychain ACL prompts after each re-sign; code-sign oxbot consistently. |
| QR in the terminal | `qrcode 0.14.1` (2024, still the standard; renders to `unicode::Dense1x2`) | `fast_qr 0.14.0` (2026) is a faster alternative and also renders SVG for a web pairing page. `qr2term 0.3.3` is a thin wrapper. |
| PTY (only if you ever need the interactive TUI) | `portable-pty 0.9.0` | Not needed: both CLIs have headless JSON modes. |

**Avoid or watch out for:**
- OpenRouter and any third-party relay or tunnel (constraint).
- Reusing Claude OAuth tokens outside the `claude` binary (ToS), or ChatGPT tokens against undocumented endpoints.
- `codex app-server` WebSocket transport is "experimental and unsupported". Use stdio.
- ACP's draft v2 and `unstable_*` features break between versions; pin them.
- Codex's generated app-server schema is version-specific (`codex app-server generate-json-schema`). Regenerate it on each Codex upgrade and parse tolerantly.
- A Claude `stream-json` and Codex JSONL parser must ignore unknown event types.
- `axum-server 0.8.0` was last released 2025-12. That's OK, but you could also use `tokio-rustls` with `hyper-util` directly.
- Plaintext secrets on disk: `~/.claude/projects/*.jsonl` and `~/.codex/auth.json`. Set Codex to `keyring` and keep the Mac's FileVault on.

---

## 5. Recommended architecture (one paragraph)

`oxbotd` is a launchd agent running axum on `https://0.0.0.0:8443` with an rcgen cert, advertised over mDNS. Pairing: `oxbot pair` prints a QR code in the terminal containing `{url, cert_sha256, one_time_code}`. The phone's PWA or app exchanges the code for a device token, which is stored hashed in SQLite, and its master secret lives in the Keychain. A `ConversationActor` per conversation (a tokio task and an mpsc inbox) owns the active `Backend` handle. Backends: `ClaudeCli` (spawn per turn with resume), `CodexAppServer` (one long-lived stdio process with many threads), and `OwnLoop` (async-openai against localhost models, or API-key Anthropic/OpenAI). All events are normalized into the `event` table and fanned out to the phone over WebSocket. A switch runs this procedure: finish or cancel the current turn → write `backend_switch` with the git snapshot → regenerate `.oxbot/HANDOFF.md` → resume B's native session with the delta preamble, or start a new one with the condensed preamble (Codex: `thread/inject_items` message items; Claude: first prompt or `--append-system-prompt`). Child processes get the telemetry-off env and config from §3.2.

## 6. Risks
1. **Handoff fidelity:** B doesn't see A's tool traces or reasoning, only summaries. Mitigation: HANDOFF.md plus the git snapshot, and the delta preamble tells B to re-read files.
2. **CLI churn:** Codex app-server and Claude stream-json schemas change often. Mitigation: tolerant parsers, store `raw`, contract tests against a pinned CLI version.
3. **ToS:** keep subscription use inside the unmodified binaries. The own loop uses API keys or local models only.
4. **Subscription limits:** `claude -p` and Agent SDK use count against Pro/Max limits; the separate credit is paused.
5. **Telemetry egress to third parties** if the env vars are missing. Mitigation: oxbot sets them on every spawn and runs a startup self-check (e.g. warn if `~/.codex/config.toml` lacks `[analytics] enabled=false`).
6. **Web tools leak URLs and queries** to third parties. Mitigation: an oxbot setting to deny WebFetch/WebSearch in strict mode.
7. **Remote access off the LAN:** no relay is allowed. Use a VPN into your own network, or Anthropic Remote Control for Claude-only sessions.
8. **Retention:** prompts persist at the vendor for 30 days (or 5 years if Claude training is on). Check both privacy toggles.
9. **Local-model quality:** good for private and offline work, but weaker at agentic tool use. Treat the local backend as the "private mode", not the default coder.

---

## Sources
- ACP intro and transports: https://agentclientprotocol.com/overview/introduction ; doc index https://agentclientprotocol.com/llms.txt
- ACP agents list (claude-agent-acp, codex-acp, Goose, OpenCode): https://agentclientprotocol.com/get-started/agents.md
- ACP session config options (model switching): https://agentclientprotocol.com/protocol/v1/session-config-options.md
- ACP Rust library: https://agentclientprotocol.com/libraries/rust.md ; https://docs.rs/agent-client-protocol/3.5.0/agent_client_protocol/
- claude-agent-acp: https://github.com/zed-industries/claude-agent-acp ; Zed blog: https://zed.dev/blog/claude-code-via-acp
- Goose ACP providers: https://goose-docs.ai/docs/guides/acp-providers ; CLI providers deprecated: https://goose-docs.ai/docs/guides/cli-providers ; v1.24 notes: https://newreleases.io/project/github/block/goose/release/v1.24.0
- Happy: https://github.com/slopus/happy ; encryption: https://www.mintlify.com/slopus/happy/features/encryption ; Show HN: https://hn.svelte.dev/item/44904039
- Omnara: https://github.com/omnara-ai/omnara
- CloudCLI / claudecodeui: https://github.com/siteboon/claudecodeui ; tunnel guide: https://pinggy.io/blog/remotely_manage_claude_code_from_phone/
- Conductor: https://docs.conductor.build ; https://www.conductor.build/docs/faq
- Vibe Kanban: https://virtuslab.com/blog/ai/vibe-kanban/ ; https://www.issoh.co.jp/tech/details/10075/
- OpenCode switching: https://realpython.com/lessons/understanding-opencode-differentiators/ ; https://github.com/sst/opencode
- Crush: https://next-docs.charm.land/crush/
- aider commands: https://aider.chat/docs/usage/commands.html
- LiteLLM telemetry: https://docs.litellm.ai/docs/proxy/telemetry
- Agent Communication Protocol → A2A: https://www.bigdatawire.com/2025/05/08/open-source-ai-gets-a-boost-as-ibm-hands-over-key-projects-to-linux-foundation ; https://dev.to/czmilo/integrating-a2a-protocol-intelligent-agent-communication-solution-for-beeai-framework-4e53
- Claude Code data usage and telemetry: https://code.claude.com/docs/en/data-usage
- Claude Code Remote Control: https://code.claude.com/docs/en/remote-control
- Claude Code legal and compliance (auth and credential use): https://code.claude.com/docs/en/legal-and-compliance
- Agent SDK with your plan (credit paused): https://support.claude.com/en/articles/15036540-use-the-agent-sdk-with-your-claude-plan
- Anthropic API retention: https://platform.claude.com/docs/en/manage-claude/api-and-data-retention ; https://privacy.claude.com/en/articles/7996866-how-long-do-you-store-my-organization-s-data
- Codex config (analytics, feedback, otel, oss, history): https://learn.chatgpt.com/docs/config-file/config-advanced
- Codex auth: https://learn.chatgpt.com/docs/auth.md
- Codex app-server: https://learn.chatgpt.com/docs/app-server.md (redirect from developers.openai.com/codex/app-server)
- OpenAI data training controls: https://help.openai.com/en/articles/5722486-how-your-data-is-used-to-improve-model-performance
- OpenAI API data controls: https://developers.openai.com/api/docs/guides/your-data
- Ollama Anthropic compatibility: https://docs.ollama.com/api/anthropic-compatibility ; FAQ (cloud and local-only mode, OLLAMA_HOST): https://docs.ollama.com/faq
- async-openai: https://docs.rs/async-openai/latest/async_openai/ ; config source: https://github.com/64bit/async-openai/blob/main/async-openai/src/config.rs
- keyring 4: https://docs.rs/keyring/latest/keyring/
- Crate versions: https://crates.io/api/v1/crates/<name> (queried 2026-10-27)
