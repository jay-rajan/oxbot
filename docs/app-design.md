# App design: oxbot v2. First proposed 2026-10-27 after deep research; updated 2026-11-04

Name: ox (oxide) + bot. Research behind every choice: `docs/research/` (4 files, sources cited).
The words people see follow `docs/glossary.md` (home computer, device, Assistant, approval
request, AI model, AI spending).

## What Jay asked for (2026-10-27)
1. Use both the **Claude Code CLI** and the **Codex CLI** (and our own loop).
2. **Switch between them seamlessly, even mid-conversation.**
3. **Always running** on the laptop.
4. A **mobile app** that discovers the bot on the network, connects, and can **add any bot**.
5. **Data goes nowhere except Anthropic (Claude) and OpenAI (Codex).**

History: that Claude + Codex plan was replaced. 2026-11-02 (Step 5, #17 and #18): Claude bots on
the owner's own plan plus OpenRouter bots on oxbot's own loop. 2026-11-04 (Step 6 part 2): Claude
is gone; every bot uses OpenRouter's models, and Codex runs the agent loop by default. Then (part
3) oxbot's own loop retired as an engine: Codex is the only one, and what the loop had that Codex
uses stays (the tool service, gate, page tools, rows, stored results, answer check, model
gateway).

## The data rule, made precise (2026-11-04)
Model calls go only to **OpenRouter**, through oxbot's relay (and, for a page question, oxbot's
own request on the same routing, limits and meter), with the home's
OpenRouter key; later also to a model on this computer. Nothing goes to Anthropic, and Codex
never contacts OpenAI itself. A person's own `~/.codex` is never read.

| What | Where it goes |
|---|---|
| Prompts, answers, files the bot reads | OpenRouter and the provider it routes to (or a model on this computer, later). Unavoidable: that's where the models run. Private bots ask providers to keep nothing. |
| What the relay sent | Kept 14 days in the bot's folder, on the home computer |
| Transcripts, history, settings, pairing keys, the OpenRouter key | Stay on the home computer (SQLite + Keychain) |
| Device ↔ home computer traffic | Home Wi-Fi only, TLS with a pinned certificate. No relay server, no push service |
| On this computer (later) | Nothing leaves the home computer at all |

Consequences:
- **Codex is locked down by oxbot.** Each bot's `codex app-server` gets its own `CODEX_HOME` in the bot's folder and a config oxbot writes at every start: analytics, plugins, web search, update checks and its shell off. Its only model provider is oxbot's relay on 127.0.0.1; any other address goes to a proxy that refuses it.
- **The relay is the one way out for Codex.** It checks a per-process token, strips Codex's ids, headers and metadata, applies the bot's model, privacy and limits, sends to OpenRouter's Responses API with the home's key, meters each request and keeps what was sent.
- **Web tools are off.** Bots browse only through oxbot's checked browser tool; scripts run with no network.
- **No push notifications.** Apple's or Google's push service would see timing and the device. The phone catches up when it opens.
- **Prior-art apps that use a cloud relay are out** (Happy's hosted relay, Omnara Cloud).
- **Check once with a network monitor** (LuLu, free) that only `openrouter.ai` is contacted (and the sites bots visit).

## Shape (2026-11-04)
```
 Android app (device) ──(home Wi-Fi, TLS pinned)──► oxbot on the home computer (Rust, always on)
   pairs once via QR code                           ├─ store: SQLite (chats, saved steps, meter)
 Desktop app / page (localhost) ────────────────────┤
                                                    ├─ Codex engine: one `codex app-server` per bot (JSON-RPC over stdio)
                                                    │     └─ model calls → oxbot's relay (127.0.0.1) → OpenRouter Responses API
                                                    └─ one tool service + gate (oxbot-loop): browser, page tools, files, team tools, approval requests
```

## One engine, one trait
Codex plugs into `oxbot-engine` (session, prompt, event stream, approval, cancel). People don't pick an engine; they see an **AI model** (free or paid models).

| Engine | How oxbot drives it | Notes |
|---|---|---|
| **Codex** (every bot) | Codex CLI 0.148.x as `codex app-server`, one per bot; `thread/start|resume`, `turn/start|interrupt`; oxbot's tools as dynamic tools | Tools go through oxbot's tool service and gate (`oxbot-loop`): approval requests, Always allow, refusals, untrusted-text marks, the answer check |

oxbot's own loop (its runner, context window, compaction and progress notes) retired as an engine; `oxbot-loop` keeps the parts Codex uses, under their old names.

Upgrade: bots that ran on Claude (`engine = "claude"`, or no engine) or on oxbot's own loop (`engine = "openrouter"`) move to Codex at their first start, with one plain line in the chat (one for each; noted in `engine_upgrades`, migration 22); `bot.toml` is rewritten once and keeps the bot's model. Chats are kept; old sessions aren't resumed, and a loop task that was running is stopped (migration 23). Without an OpenRouter sign-in, bots say the AI account isn't connected yet.

Compare and Shadow compare **models** now (one row per model on the scoreboard, each measured against the best), on Codex in practice runs.

Known gaps: pictures and PDFs reach the bot only as their text (Codex's dynamic tools take no image input yet); shipping `codex` inside oxbot.app comes later (installed separately for now).

Mid-conversation switching between vendors (the 2026-10-27 idea: oxbot owns the transcript, hands a summary to the other backend) is not needed with one engine; the transcript in SQLite stays oxbot's own.

## Devices (the phone app)
Chosen: **Tauri 2, Android** (the Android app is only ever a device; see the README). The 2026-10-27 notes:
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
- Keep the home computer awake **only while a job runs** (`keepawake` crate), on power.
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
`oxbot doctor` checks the privacy settings (Codex config, relay, web tools off) before anything runs. (This layout was the 2026-10-27 proposal; the crates today are listed in the README.)

## Crates (versions checked 2026-10-27)
axum 0.8.9 · tokio 1.54.1 · tokio-stream 0.1.19 · tower-http 0.7.1 · serde 1.0.229 · tracing 0.1.44 · rusqlite 0.41 + tokio-rusqlite 0.8 · rustls 0.23.46 · rcgen 0.14.10 · axum-server 0.8.0 · mdns-sd 0.21.6 · keyring 4.2 · qrcode 0.14.1 · async-openai 0.42.2 (local loop).
Codex protocol types: generated from `codex app-server generate-json-schema`, pinned to the CLI version.

## Risks
- Codex changes its app-server protocol often: pin the version (0.148.x), turn auto-update off, ignore unknown events, keep recorded samples as tests.
- Codex app-server is not a stable public spec.
- Free models are rate-limited and vary in quality; paid models cost per token, so they're off unless the owner allows them, with daily limits.
- Not instant: no push, and the home computer must be awake.

## Steps (the 2026-10-27 plan; the current plan is in `docs/plan/`)
0. Lessons 0–5.
1. **core:** event types + SQLite store + tests.
2. **First engine** + CLI (was Claude; removed 2026-11-04).
3. **Codex** (app-server): the default engine since 2026-11-04, on OpenRouter through oxbot's relay.
4. **Server:** WebSocket + page; approval requests.
5. **Always on:** LaunchAgent, keep-awake per job, `oxbot doctor`.
6. **Devices:** QR pairing + pinned TLS, then the phone app.
7. **On this computer** (Ollama), then v2: WireGuard.

## Decisions (taken)
1. Phone app: **Tauri 2, Android**.
2. Models: **OpenRouter** (free first, paid if the owner allows); a model on this computer later.
3. Away from home: **home Wi-Fi only** for v1.
