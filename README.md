# oxbot

Always-on Claude bots on your Mac, used through the oxbot desktop app and the oxbot Android app
on your home Wi-Fi. Data goes only to Anthropic (and, later, OpenAI for Codex): no relay
servers, no push services, telemetry off.

Status: desktop app on the Mac plus the Android Gate app (milestone Foundations, issue #1). Shared
bots share one hidden home Chrome that keeps your sign-ins: a bot that needs an account asks, a
Chrome window opens, you sign in once, and every bot on that Chrome stays signed in (a sign-in
counts only if the window left a new cookie). The sidebar lists the signed-in sites; open one, or sign out of it
alone. Bots take turns on the browser. oxbot checks every browser step: a red card before
anything that changes something (Send, Post, Delete, Buy…), a card before an upload from your
Mac, and it refuses passwords, page scripts, non-web pages and files outside the bot's folder.

Herder (the central bot, once called the Assistant) leads your bots: it can list them, create
one (after you press Create on a card), give a bot a task and bring back its reply. Other bots
can't. Messages show who sent them, each person in their own color; a working bot's icon moves,
and a green dot marks unread replies.

Private bots: the Mac that runs the bots is the Farm. Every bot records who made it, and its
maker can make it private. A private bot is listed for everyone (name, maker, a lock), but only
its maker and the Farm's owner can open it, and it browses in its maker's own Chrome
(Settings → Sign-ins → Yours), never the home one. Your own Herder can give it tasks; it asks you
first if it has read web content or other bots' replies. Nobody else's Herder can use it, and
Herders can't task or read each other. Long chats open a page at a time.

Gates: phones (the Android app) use the Farm's bots over home Wi-Fi, as the person they were
paired for. In Settings → Gates the Farm lists the networks phones may use ("Add this network";
oxbot knows a network by its router, not its name), then shows a one-time code and QR code for a
person; the phone scans it once. The phone talks only to the Farm's Gate door (port 7879, TLS with
the Farm's own certificate, which the phone pins), and only while the Mac is on a listed network.
A Gate is never the Farm: it sees shared bots and its person's own, answers only the red cards
from its person's requests, and gets its person's own Herder. Away from home the phone shows its
own copy of its chats (the latest 300 messages per bot, 30 days). Remove a phone in Settings →
Gates and it's cut off at once. `OXBOT_GATE_PORT=off` turns the door off.

Who is the Farm: only oxbot's own windows. The app opens its window with the Farm secret (a key
in the data folder, readable only by your Mac account); `oxbot chat` and `oxbot bots` read it;
`oxbot serve` prints a link with it. Other pages and programs on the Mac can't act as the Farm.

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
| `crates/oxbot-core` | Bots, people (ids), store (`store/`), hub (`hub/`, split by job), tool rules (policy), prompts (no networking) |
| `crates/oxbot-claude` | Runs `claude -p` with privacy settings |
| `crates/oxbot-browser` | A hidden Chrome per owner (home, or a person's own) with a kept profile, and its sign-in window |
| `crates/oxbot` | The `oxbot` command: `serve`, `chat`, `bots`, `pair`, `devices`, `doctor`; the chat page; the Gate door (`src/door/`) |
| `crates/oxbot-gate` | The Gate side of the link: pairing link, pinned TLS client, the phone's chat copy (builds for Android) |
| `app/src-tauri` | The desktop app and the Android Gate app (Tauri 2) |

## Android app (a Gate)
The phone app is only ever a Gate: no server, bots or Chrome are compiled in. Tools (once):
Homebrew `openjdk@17` and `android-commandlinetools`, then the SDK in `~/Library/Android/sdk`
(platform-tools, `platforms;android-36`, `build-tools;36.0.0`, an NDK) and
`rustup target add aarch64-linux-android`.

```sh
scripts/android-build.sh       # puts oxbot's Android settings back, then builds the debug APK
adb install -r app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

oxbot's Android settings (no backups, the local-network and camera permissions, MainActivity
asking for local network access) live in `app/src-tauri/android/` and `scripts/android-apply.sh`,
because `cargo tauri android init` rewrites `gen/android`. `cargo test` fails if they're missing
there.

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

## End-to-end tests (`/pr_test`)
`e2e/` holds tests a person can read (`e2e/tests/*.toml`: open, click, type, send, expect…), seed data
(`e2e/seed/`: bots plus `seed.sql` for a fresh SQLite database), the stand-in Claude's replies
(`e2e/claude/`), and approved screenshots (`e2e/baselines/`). The runner, `crates/oxbot-e2e`, starts a
throwaway oxbot in test mode (`OXBOT_E2E=1`), drives its page in hidden Chrome (and Safari, the app's
engine, when "Allow Remote Automation" is on), screenshots every step, and checks the built Mac app.

```sh
cargo build -p oxbot -p oxbot-e2e
target/debug/oxbot-e2e pick --base main          # which tests this change needs, and why
target/debug/oxbot-e2e run --out /tmp/run e2e/tests/*.toml
target/debug/oxbot-e2e app --out /tmp/run        # the Mac app check (build it first)
target/debug/oxbot-e2e report --out /tmp/run     # /tmp/run/index.html
target/debug/oxbot-e2e approve --out /tmp/run    # make those screens the baselines
```

In Claude Code, `/pr_test` does all of it, fixes what broke, and publishes the report.

## Docs
- `docs/plan/` build plan · `docs/architecture/` living architecture page (C4 diagrams)
- `docs/brand/` branding: logos, avatars and palette (all SVG)
- `docs/app-design.md` design · `docs/research/` research with sources
- `PROMPTS.md` the prompts that shaped this project

The Rust lessons that started here moved to `~/src/rust-lessons`.

## License
Dual-licensed under MIT or Apache-2.0, at your option.
