# App design: oxbot v2 (proposal, waiting for Jay's OK). Updated 2026-10-27 after deep research

Name: ox (oxide) + bot. Research behind every choice: `docs/research/` (4 files, sources cited).

## What Jay asked for (2026-10-27)
1. Use both the **Claude Code CLI** and the **Codex CLI** (and our own loop).
2. **Switch between them seamlessly, even mid-conversation.**
3. **Always running** on the laptop.
4. A **mobile app** that discovers the bot on the network, connects, and can **add any bot**.
5. **Data goes nowhere except Anthropic (Claude) and OpenAI (Codex).**

## The data rule, made precise
| What | Where it goes |
|---|---|
| Prompts, answers, files the agent reads | Anthropic (Claude) or OpenAI (Codex). Unavoidable: that's where the models run. |
| Transcripts, history, settings, pairing keys | Stay on the laptop (SQLite + Keychain) |
| Phone ↔ laptop traffic | Home Wi-Fi only, TLS with a pinned certificate. No relay, no push service |
| Local-model mode | Nothing leaves the laptop at all |

Consequences:
- **OpenRouter is dropped.** It's a third party that sees every prompt.
- **Telemetry off is required, not optional.** Out of the box, Claude Code sends metrics and error reports partly to third-party services, and Codex contacts GitHub, Homebrew, npm and CDNs (update checks, plugins, connectors). oxbot sets these off on every child process (env vars for Claude; a dedicated `CODEX_HOME/config.toml` for Codex; full lists in research files).
- **Web tools are off by default.** WebFetch/WebSearch (Claude) and web_search (Codex) reach other sites. Agents' shell commands run with sandbox network off.
- **No push notifications.** Apple's push service would see timing and the device. The phone catches up when it opens.
- **Prior-art apps that use a cloud relay are out** (Happy's hosted relay, Omnara Cloud).
- **Check once with a network monitor** (LuLu, free) that only `*.anthropic.com`, `claude.ai`, `chatgpt.com`, `*.openai.com` are contacted.

## Shape
```
 iPhone app ──(LAN, TLS pinned, WebSocket)──► oxbotd (Rust, always on, LaunchAgent)
   finds it via Bonjour _oxbot._tcp            ├─ store: SQLite event log (canonical transcript)
   pairs once via QR code                      ├─ switcher: handoff on backend change
 Laptop page (localhost) ──────────────────────┤
                                               ├─ Claude backend: long-lived `claude -p` (stream-json in/out)
                                               ├─ Codex backend:  long-lived `codex app-server` (JSON-RPC over stdio)
                                               └─ Local backend:  our own loop → Ollama / LM Studio on localhost
```

## Three backends, one trait
Shaped like ACP (Agent Client Protocol: session, prompt, update stream, permission request, cancel) so an ACP backend can be added later. We don't use ACP itself now: it adds a Node adapter per backend and can't carry history across vendors.

| Backend | How oxbot drives it | Login | Notes |
|---|---|---|---|
| **Claude** | One `claude -p --input-format stream-json --output-format stream-json --verbose --session-id <uuid>` per conversation; turns go in on stdin; `--resume <id>` later | Jay's Claude subscription, unmodified binary (allowed for personal use) | Interrupt via stdin control request; approvals via `--permission-prompt-tool stdio` |
| **Codex** | One `codex app-server` child; `thread/start|resume`, `turn/start|interrupt`; streaming deltas | Jay's ChatGPT login (own `CODEX_HOME`) | `thread/inject_items` takes prior history directly, so switching *into* Codex is smooth. Answer every approval request or the turn hangs |
| **Local** | Our own agent loop (async-openai with a localhost base URL) + a few tools | none | Fully private; weaker at agent work, so a "private mode", not the default coder |

(Our own loop could also call the Anthropic/OpenAI APIs directly with API keys. Subscriptions can't be used outside the official CLIs, so that path costs per token. Optional, later.)

## Switching mid-conversation
No existing tool switches Claude ↔ Codex inside one conversation; oxbot does it itself. Example: type `/use codex` (or tap a chip on the phone).
1. **oxbot owns the transcript.** Every event (user msg, answer, tool call, tool result, file change, switch) lands in SQLite with a sequence number.
2. **Each backend keeps its own native session**, remembered per conversation with the last event it saw.
3. **On switch to B:**
   - B has a session → resume it and send only the **delta** (what the other backend did since), as short text.
   - B has none → start fresh with a **condensed summary** (goal, decisions, files touched, last few turns). Codex gets it via `inject_items`; Claude as the first message.
   - Both are told to read `.oxbot/HANDOFF.md` (plan, decisions, next steps), regenerated from the log, and get the git HEAD + changed files.
4. Tool calls from A are passed as one-line text ("ran cargo test → 3 failed"), never as native tool items. Vendor formats don't mix, and reasoning is encrypted.

Honest limit: B gets a summary of A's work, not A's full memory. The code on disk is the real shared state.

## Phone app
- **Discovery:** oxbotd advertises `_oxbot._tcp` (crate `mdns-sd`; fallback `zeroconf` if it fights macOS Bonjour). The iPhone browses with NWBrowser. Info.plist needs `NSLocalNetworkUsageDescription` and `NSBonjourServices`. No special Apple entitlement needed.
- **Pairing (LocalSend/Syncthing style):** `oxbot pair` prints a QR code: host, port, certificate fingerprint, one-time code (5 min). The phone trades it for its own token (Keychain). `oxbot devices revoke <name>` cuts a phone off.
- **"Add any bot":** the phone keeps a list of paired servers (each laptop running oxbotd). Each server has bot profiles (backend, model, work folder, permissions) you can create from the phone.
- **Live output:** one WebSocket per server. Events are numbered, so after iOS suspends the app, it reconnects and asks "events since N". Approvals wait on the server until you answer.
- **Tech: two options (your call):**
  - **A. SwiftUI iPhone app (recommended).** Bonjour, certificate pinning and Keychain are built in. All Rust learning goes into the server, which is most of the work.
  - **B. Tauri 2 mobile.** Rust on the phone too, but no mDNS plugin (we'd write a small Swift one), and the web view can't talk to a self-signed server, so all networking moves into Rust. Better as v2 or for Android.
  - A free Apple ID installs it on your phone (re-sign every 7 days). $99/yr removes that and enables TestFlight.

## Always on
- `~/Library/LaunchAgents/com.jay.oxbot.plist` with `RunAtLoad` + `KeepAlive`.
- Keep the Mac awake **only while a job runs** (`keepawake` crate), on power.
- Limits: a closed lid sleeps the MacBook unless it's on power with an external display. A sleeping Mac answers nothing. macOS 15+ asks once to allow local-network access for the LaunchAgent.
- Away from home: v1 is **home Wi-Fi only**. v2: WireGuard into the home network (no third party). Hosted Tailscale breaks the rule (its servers see metadata and relay traffic).

## Project layout (Cargo workspace)
```
oxbot/
  Cargo.toml                 [workspace]
  crates/core/               event model, SQLite store, handoff builder (no I/O to agents; easy to test)
  crates/backends/           trait Backend + claude.rs, codex.rs, local.rs; tests with recorded JSONL
  crates/oxbotd/             the server: axum, WebSocket, pairing, mDNS, LaunchAgent install, CLI (pair, devices, doctor)
  apps/ios/                  SwiftUI app (option A)
  ui/                        laptop page
  docs/research/             the 4 research reports
```
`oxbot doctor` checks the privacy settings (telemetry off, web tools off, Codex config) before anything runs.

## Crates (versions checked 2026-10-27)
axum 0.8.9 · tokio 1.54.1 · tokio-stream 0.1.19 · tower-http 0.7.1 · serde 1.0.229 · tracing 0.1.44 · rusqlite 0.41 + tokio-rusqlite 0.8 · rustls 0.23.46 · rcgen 0.14.10 · axum-server 0.8.0 · mdns-sd 0.21.6 · keyring 4.2 · qrcode 0.14.1 · async-openai 0.42.2 (local loop).
Codex protocol types: generated from `codex app-server generate-json-schema`, pinned to the CLI version.

## Risks
- Both CLIs change their event formats often: pin versions, turn auto-update off, ignore unknown events, keep recorded samples as tests.
- Claude's stdin control protocol and Codex app-server are not stable public specs.
- Subscription use must stay inside the unmodified CLIs, for Jay only. Sharing oxbot would mean API keys.
- Heavy always-on use counts against Claude/ChatGPT plan limits.
- Not instant: no push, and the Mac must be awake.

## Steps
0. Lessons 0–5 (you're in lesson 4).
1. **core:** event types + SQLite store + tests. (Pure Rust; good next learning step.)
2. **Claude backend** + CLI: `oxbot chat --backend claude`. Privacy env applied.
3. **Codex backend** (app-server) + `/use codex` switching in the CLI. This is the heart.
4. **oxbotd server:** WebSocket + laptop page; approvals.
5. **Always on:** LaunchAgent, keep-awake per job, `oxbot doctor`.
6. **Phone:** mDNS + QR pairing + pinned TLS, then the iPhone app.
7. **Local backend** (Ollama), then v2: WireGuard, Tauri/Android.

## Decisions for Jay
1. Phone app: **SwiftUI (A)** or **Tauri (B)**?
2. Third backend: **local model** (fully private), **API keys** (costs per token), or neither for now?
3. Away from home: **home Wi-Fi only** for v1? (recommended)
