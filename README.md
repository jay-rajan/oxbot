# oxbot

Always-on Claude bots on your Mac, used through the oxbot desktop app (and, later, an Android
app on your home Wi-Fi). Data goes only to Anthropic (and, later, OpenAI for Codex): no relay
servers, no push services, telemetry off.

Status: desktop app on the Mac (milestone 2.11.4). All bots share one hidden Chrome that keeps
your sign-ins: a bot that needs an account asks, a Chrome window opens, you sign in once, and
every bot stays signed in. The sidebar lists the signed-in sites; open one, or sign out of it
alone. Bots take turns on the browser. oxbot checks every browser step: a red card before
anything that changes something (Send, Post, Delete, Buy…), a card before an upload from your
Mac, and it refuses passwords, page scripts, non-web pages and files outside the bot's folder.

The Assistant is the central bot: it can list your bots, create one (after you press Create on a
card), give a bot a task and bring back its reply. Other bots can't. Messages show who sent
them, each person in their own color; a working bot's icon moves, and a green dot marks unread
replies.

Setup: oxbot installs Claude Code for you (Anthropic's installer) and signs you in
(`claude auth login`); the browser tool and Node.js ship inside the app. You only need Google
Chrome. Settings (the gear) shows setup, your Claude usage and oxbot's activity log.

Brand (`docs/brand/`): the "O"xbot wordmark (a round O with red horns, then xbot in bold) and
the same O with horns on a black tile as the app icon. Minimal UI: white backgrounds, white
buttons with red text. Each bot gets a little ox avatar.

New bot from the terminal: `cargo run -p oxbot -- bots new "Daily News" --instructions "..."`.

The chat page also opens in a normal browser (`cargo run -p oxbot -- serve --open`), but that's
for testing and dev mode only: the desktop app is the way to use oxbot.
Plan: https://claude.ai/artifact/32dqY3emzXCfBj9QGkF5rZ · Architecture: https://claude.ai/artifact/3mVTriA2zwH7yzSL9D98F6 (rebuild: `python3 docs/architecture/build.py`)

## Layout
| Crate | What it is |
|---|---|
| `crates/oxbot-protocol` | Wire types shared by the server and the apps |
| `crates/oxbot-core` | Bots, store, hub, tool rules (policy), prompts (no networking) |
| `crates/oxbot-claude` | Runs `claude -p` with privacy settings |
| `crates/oxbot-browser` | The shared, hidden Chrome with a kept profile, and its sign-in window |
| `crates/oxbot` | The `oxbot` command: `serve`, `chat`, `bots`, `pair`, `devices`, `doctor`; the chat page |
| `app/src-tauri` | The desktop app (Tauri 2) |

The Android app (Tauri 2) joins in milestone 4.

## Desktop app
`sh scripts/make-dmg.sh` builds the universal app with the browser tool and Node.js inside
(`scripts/fetch-tools.sh`, pinned versions and checksums) and packs the friends dmg.
`cd app && cargo tauri build` makes `target/release/bundle/macos/oxbot.app` without them. `cargo tauri dev`
runs it from source. The app runs the server inside it; closing the window keeps oxbot in the
menu bar (Open, Launch at login, Quit).

For friends: `scripts/make-dmg.sh` builds one universal app (Apple chip + Intel) with a local
signature and packs it in `target/universal-apple-darwin/release/bundle/dmg/oxbot_<version>_universal.dmg`.
There's no Apple developer account, so the first time they open it via System Settings →
Privacy & Security → Open Anyway. Setup steps for friends: the oxbot Setup Guide doc.

## Dev mode (auto-rebuild)
`brew install watchexec` once, then `scripts/dev.sh`. Rust edits rebuild and restart the server;
page edits (`crates/oxbot/ui/index.html`) show on refresh; an open page reloads itself after each
restart. Use `OXBOT_HOME=/tmp/oxdev scripts/dev.sh` to keep dev data apart from your real bots.

## Develop
The toolchain is pinned in `rust-toolchain.toml`. Before calling any step done:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Lints live in `[workspace.lints]` in `Cargo.toml`: no `unsafe`, clippy pedantic, no `unwrap`/`expect`
outside tests. Libraries use `thiserror`; only the binary uses `anyhow`.

## Docs
- `docs/plan/` build plan · `docs/architecture/` living architecture page (C4 diagrams)
- `docs/brand/` branding: logos, avatars and palette (all SVG)
- `docs/app-design.md` design · `docs/research/` research with sources
- `PROMPTS.md` the prompts that shaped this project

The Rust lessons that started here moved to `~/src/rust-lessons`.

## License
Dual-licensed under MIT or Apache-2.0, at your option.
