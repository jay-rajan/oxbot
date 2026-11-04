# oxbot

Always-on bots on your home computer (a Mac), used through the oxbot desktop app there and the
oxbot Android app on devices on your home Wi-Fi. Bots answer with free and paid AI models through
your home's OpenRouter account, free models first (see [AI models](#ai-models)). Model calls go
only to OpenRouter, or later to a model on this computer; nothing else: no relay servers, no push
services, no analytics, and nothing goes to Anthropic.

Status: the desktop app on the home computer plus the Android app for devices (milestone
Foundations, issue #1). Shared bots share one hidden home Chrome that keeps the websites you're
signed in to: a bot that needs an account asks, a Chrome window opens, you sign in once, and
every bot on that Chrome stays signed in (a sign-in counts only if the window left a new cookie).
Settings → Sign-ins lists the websites you're signed in to (the sidebar's Signed in link opens
it); open one, or sign out of it alone. Bots take turns on the browser. oxbot checks every
browser step: links, tabs, search, filters, sorting and paging run at once; an approval request
comes before anything that changes something (Send, Post, Save, Apply…, or a filled-in POST
form's button), and before paying, booking or deleting; one comes before an upload from the home
computer; and it refuses passwords, page scripts, non-web pages and files outside the bot's
folder. oxbot names what a click aims at from every page snapshot the bot has read, and for an
unclear button it reads the button's form itself, in its own isolated world the page's scripts
can't touch. A click oxbot still can't name runs, and is logged. An approval request for a change
can offer **Always allow** (to the bot's maker or the owner at the home computer): the same bot
may then click the same button on the same exact site without asking, with a quiet line in the
chat. Paying, booking, deleting, Enter in a text box, uploads and the Assistant's requests never
offer it. The bot's ··· settings list what it may always do, each with Remove. Open approval
requests sit right above the message box, newest nearest it, whichever bot is open; that area
scrolls on its own so the box stays on screen, on a phone too.

The sidebar: search finds a bot by name as you type (Enter opens the first match). Group bots by
section or by who made them. Sections are your own: make, rename and delete them, and drag a bot
onto one or use its menu; the home computer keeps them for each person, so your Mac and phone
match, and nobody else sees yours. You may file any bot you're shown, even someone else's: it
changes only your view. Right-click a bot (long-press on a phone) to rename it, make it private
or shared, move it to a section or delete it, with the same rules as its ··· settings. The
Assistant stays at the top. The bottom row: Settings, Connect a device (the home computer only)
and Signed in.

Research reports: a bot with the browser hands back research (comparing options, looking things
up across sites) as a report, with a 2–4 line reply and a link in the chat; quick answers stay
in chat. The report has the question, a short answer first, sections and tables, and numbered
citations [1] linked to a Sources list (title, site, link, when it was read). Every source must
be a page the bot opened in the browser in that conversation, or oxbot refuses the save. oxbot
keeps reports on the home computer (`oxbot.db`) and renders them itself, safely: raw HTML shows
as text, no images, links only to the report's sources or pages the bot opened, a strict CSP, and
the app shows it in a sandboxed frame with no scripts. Reports open over the app's own connection
(no link that carries a key), for whoever may open the bot, by its rules at the time; source
links open in the system browser. The bot's ··· lists its reports (anyone who may open the bot);
managers delete them. Deleting a bot deletes them (`reports.jsonl` in the trash keeps a copy).
A phone keeps the reports it opened for reading away from home.

Files and voice: attach files to a message with the paperclip, by dropping them on the chat, or
by pasting; several per message (up to 10), and a message may be files alone. Each uploads at
once (`POST /upload`, on the home computer only; sending files from a device comes later) and
shows as a chip; oxbot checks it by its contents and says plainly when it can't take it
(archives, audio, programs, password-locked PDFs, over 25 MB, or videos over 200 MB). Files are
saved in the bot's own folder, `work/inbox/<date>/`, kept until the bot is deleted, and shown as
chips on the sent message (on the home computer, a chip opens the file). The bot reads them with
oxbot's file tool: text, CSV, JSON and code as text; Word, Excel and PowerPoint turned into text
on the home computer (in Rust); old Word, RTF and OpenDocument text with `textutil`; iPhone
photos and very large pictures as a JPEG made with `sips`. For a video oxbot takes up to 8 evenly
spaced still frames and says so in the chat. oxbot's note to the bot lists the files by path and
says their text is information, not instructions; reading one marks the task as having read
untrusted text, as web pages do. The microphone button dictates with the Mac's own on-device
speech recognition (`requiresOnDeviceRecognition`): the audio never leaves the home computer,
the text fills the box at the cursor and stays editable. It shows only on the home computer, and
only where the Mac can dictate on-device (Settings → Setup says why not), in the browser page and
the app alike, because the home computer does the listening. Frames and dictation come from
`oxbot-mac`, a small Swift helper (`mac/`, built by `scripts/build-mac-helper.sh`) inside the
app; oxbot built without Swift still works, and video and the mic then say they're not
available. macOS asks for the microphone and speech recognition the first time.

The Assistant leads your bots: it can list them, make one (after you press Create on its
approval request), give a bot a task and bring back its reply. Other bots can't. Messages show
who sent them, each person in their own color; a working bot's icon moves, and a green dot marks
unread replies.

Private bots: every bot records who made it, and its maker can make it private. A private bot is
listed for everyone (name, maker, a lock), but only its maker and the owner can open it, and it
browses in its maker's own Chrome (Settings → Sign-ins → Yours), never the home one. Your own
Assistant can give it tasks; it asks you first if it has read web content or other bots'
replies. Nobody else's Assistant can use it, and Assistants can't task or read each other. Long
chats open a page at a time.

Devices: phones (the Android app) use the home computer's bots over home Wi-Fi, as the person
they were connected for. In Settings → Devices the home computer lists the networks phones may
use ("Add this network"; oxbot knows a network by its router, not its name), then **Connect a
device** shows a one-time code and QR code for a person; the phone scans it once. The phone talks
only to the home computer's device door (port 7879, TLS with the home computer's own certificate,
which the phone pins), and only while the Mac is on a listed network. A device is never the home
computer: it sees shared bots and its person's own, answers only the approval requests from its
person's tasks, and gets its person's own Assistant. Away from home the phone shows its own copy
of its chats (the latest 300 messages per bot, 30 days) and of the last 50 reports it opened.
Remove a phone in Settings → Devices and it's cut off at once. `OXBOT_GATE_PORT=off` turns the
door off.

Who is the home computer: only oxbot's own windows. The app opens its window with the home
computer's secret (a key in the data folder, readable only by your Mac account); `oxbot chat`
and `oxbot bots` read it; `oxbot serve` prints a link with it. Other pages and programs on the
Mac can't act as the home computer.

Setup: the browser tool and Node.js ship inside the app; you need Google Chrome and Codex CLI
0.148.x (installed separately for now: oxbot looks in `PATH`, `/opt/homebrew/bin` and
`/usr/local/bin`, or where `OXBOT_CODEX` or Settings says), and the owner signs in to OpenRouter
once. Settings (the gear) shows setup, AI spending and oxbot's activity log.

Usage alerts: when the home nears or reaches one of its OpenRouter limits (free questions for the
day, AI spending, credit left, or a bot's questions a day), oxbot shows a banner (until the limit
resets, or you dismiss it) and posts a Mac notification, once per level, restarts included. In
the app, a bot waiting for your OK or a sign-in while the window is hidden also posts a
notification, with the bot's name only (just "A bot" for someone else's private bot). Both switch
off in Settings → General → Notifications (the home computer only); off, levels are still noted,
so turning alerts back on brings no late one. `oxbot serve` posts through `osascript` (shown as
Script Editor); an app showing an `oxbot serve` that was already running leaves notifications to
it. macOS asks for permission the first time.

Brand (`docs/brand/`): the "O"xbot wordmark (a round O with red horns, then xbot in bold) and
the same O with horns on a black tile as the app icon. Minimal UI: white backgrounds, white
buttons with red text. Each bot gets a little ox avatar.

New bot from the terminal: `cargo run -p oxbot -- bots new "Daily News" --instructions "..."`.

The chat page also opens in a normal browser (`cargo run -p oxbot -- serve --open`), but that's
for testing and dev mode only: the desktop app is the way to use oxbot.
Plan: https://claude.ai/artifact/32dqY3emzXCfBj9QGkF5rZ · Architecture: https://claude.ai/artifact/3mVTriA2zwH7yzSL9D98F6 (rebuild: `python3 docs/architecture/build.py`)

## AI models

Every bot answers with OpenRouter's models, using your home's OpenRouter key: free models first,
paid ones only if the owner allows, with a daily number of questions per bot and per person.
Anyone in the home may give tasks to a bot they can open. A bot runs on one of two engines (the
app doesn't show the choice; it's an API setting for testing):

- **Codex** (the default for new bots and every Assistant). Codex CLI 0.148.x runs the agent loop
  as `codex app-server`, one process per bot with its own `CODEX_HOME` in the bot's folder
  (`bots/<id>/codex`) and a config oxbot writes at every start: analytics, plugins, web search,
  update checks and its shell off, and no Codex instructions of its own. Its only model provider
  is oxbot's **relay** on 127.0.0.1, which checks the process's token, strips Codex's ids,
  headers and metadata, applies the bot's model, privacy and limits, sends the request to
  OpenRouter's Responses API with the home's key and meters it. Any other address Codex tries
  goes to a proxy that refuses it. What the relay sent is kept for 14 days in the bot's
  `context/relay` folder. A person's own `~/.codex` is never read.
- **oxbot's own loop**: oxbot runs the agent loop itself, one step at a time, on OpenRouter's
  chat API. A task that was running when oxbot stopped goes on from its last saved step when
  oxbot starts again, without repeating a click.

Both reach the same tools through the same tool service and the same checks: the browser
(chrome-devtools-mcp, in the same Chrome), the page tools, files in the bot's folder, the
Assistant's team tools, save_report and request_sign_in. Each step is checked before it runs:
approval requests, Always allow, refusing passwords, page scripts, pages on this computer or the
home network and files outside the bot's folder, and marking untrusted text (an Assistant that
has read web pages still asks before tasking a private bot). Under each reply: the model, the
questions it took and the cost.

Bots made when oxbot also ran Claude move to Codex at their first start: one plain line in their
chat ("This bot now uses free and paid AI models through your OpenRouter account. Its old Claude
sessions can't be continued."), then `bot.toml` is rewritten once. Chats are kept. Until the
owner signs in to OpenRouter, bots say the AI account isn't connected yet.

Comparing (the owner only): under a finished reply, 👍/👎 and **Test case…** keep the task;
**Compare…** in the bot's menu replays test cases on Codex and on oxbot's own loop with the
models you pick, side by side with a scoreboard measured against Codex (your marks). A replay
never acts: every step that would send, post or change something is recorded instead (no
approval request, nothing in the chat), in its own browser with a copy of the sign-ins it needs
(no history, deleted when it ends) that blocks every request but reads, and WebSockets.
**Shadow** in the message box runs your own messages on another model at the same time, the same
way. The Assistant's tasks can't be test cases yet (they pass work on to other bots).

OpenRouter: the owner signs in once in Settings (**Sign in with OpenRouter**: OpenRouter opens in
your browser and comes back to oxbot on this computer, PKCE with a one-time state). The key lives
only in the Mac's Keychain (one item per data folder), never in a file, the log, the database or
the page; signing out deletes it (to revoke the key itself, delete "oxbot on {your Mac}" at
openrouter.ai/keys). The same place shows the key's limits and today's use, the free models that
can run bots (read from OpenRouter's model list, which needs no key), questions a day per bot and
per person, an off-by-default switch for paid models when every free one is busy (with a daily
dollar cap), and an off-by-default switch to ask providers not to keep data for every bot
(private bots always ask).

Models and limits: oxbot reads OpenRouter's model list and uses free models that can call tools
and hold at least 64k tokens, the best first; when one is busy or rate-limited (429), the next
free model answers. Free models allow 20 questions a minute (oxbot keeps the home under that) and
a number a day that OpenRouter sets (50, or 1,000 once the account has bought $10 of credit).
Each bot may ask 200 questions a day (change it for all in Settings, or for one bot in its ···
settings), and each person optionally a number too; a bot that reaches a limit stops and says
so, and the usage banner shows it. When OpenRouter's free questions for the day run out, bots say
so until midnight UTC, or use paid models if the owner switched them on (up to the daily dollar
cap).

What leaves the home computer: a bot's conversation (your messages, and the pages and files it
reads) goes only to OpenRouter, through oxbot's relay (Codex) or oxbot's own loop, and on to the
provider OpenRouter routes it to; free providers may keep or learn from it. Private bots ask for
providers that don't collect data and keep nothing (`data_collection: deny` and zero retention);
if no free model qualifies, the bot says so instead of switching quietly (it may first drop zero
retention, never the deny, and says that too). Signing in, the model list and the key's limits
also go to openrouter.ai. Codex's analytics, plugins and update checks are off, Codex never
contacts OpenAI itself, and nothing goes to Anthropic. Chats, saved steps, the meter
(questions, tokens and cost, never prompt text) and what the relay sent stay on the home
computer. Nothing else leaves it.

## Layout
In the code the home computer is the **Farm**, a device is a **Gate** and the Assistant is
**Herder**; the crate and type names keep those words.

| Crate | What it is |
|---|---|
| `crates/oxbot-protocol` | Wire types shared by the server and the apps |
| `crates/oxbot-core` | Bots, people (ids), store (`store/`), hub (`hub/`, split by job; engines and the upgrade from Claude), tool rules (policy), prompts, attachments; `testing` (feature `test-bots`): stand-in bots for tests (no networking) |
| `crates/oxbot-engine` | The engine interface both engines plug into: `Engine`, `Session`, one event stream |
| `crates/oxbot-codex` | The Codex engine: finds and checks `codex`, writes its config, runs one `codex app-server` per bot, maps threads, turns and oxbot's tools (dynamic tools through oxbot-loop's tool service) onto its JSON-RPC |
| `crates/oxbot-relay` | oxbot's model relay on 127.0.0.1 for Codex: token check, stripping, model/privacy/limits, OpenRouter's Responses API, meter and what-was-sent log |
| `crates/oxbot-loop` | oxbot's own agent loop: runner, tool service and tools, gate check, model router, pacer, meter |
| `crates/oxbot-llm` | Model client for OpenAI-style chat APIs (OpenRouter now, local models later), the key, sign-in (PKCE) |
| `crates/oxbot-browser` | A hidden Chrome per owner (home, or a person's own) with a kept profile, and its sign-in window |
| `crates/oxbot` | The `oxbot` command: `serve`, `chat`, `bots`, `pair`, `devices`, `doctor`; the chat page; the device door (`src/door/`) |
| `crates/oxbot-gate` | The device side of the link: pairing link, pinned TLS client, the phone's chat copy (builds for Android) |
| `app/src-tauri` | The desktop app and the Android app for devices (Tauri 2) |
| `mac/` | `oxbot-mac`, the Swift helper inside the app: video frames and on-device dictation |

## Android app (a device)
The phone app is only ever a device (a Gate): no server, bots or Chrome are compiled in. Tools (once):
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
(`scripts/fetch-tools.sh`, pinned versions and checksums) and the Mac helper
(`scripts/build-mac-helper.sh`, which needs Xcode's Command Line Tools; without them the app
builds with no video frames or microphone), and packs the friends dmg.
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
`e2e/` holds tests a person can read (`e2e/tests/*.toml`: open, click, type, send, expect…; a test can
set `viewport = { width = 390, height = 844 }` to run at a phone's size), seed data
(`e2e/seed/`: bots plus `seed.sql` for a fresh SQLite database), reply scripts (`e2e/scripts/`; a
test picks one with `script = …`), files to attach (`e2e/fixtures/`), and approved screenshots
(`e2e/baselines/`). A step can `attach = ["e2e/fixtures/bill.pdf"]`; a test with
`dictation = true` gets a stand-in Mac helper that "hears" a sentence (no test uses the real
microphone), and video goes to the real helper for real still frames. The runner,
`crates/oxbot-e2e`, starts a throwaway oxbot in test mode (`OXBOT_E2E=1`), drives its page in
hidden Chrome (and Safari, the app's engine, when "Allow Remote Automation" is on), screenshots
every step, and checks the built Mac app. Bots run on the real Codex (0.148.x must be installed:
`needs = ["codex"]` in `e2e/pr_test.toml`; without it every test is skipped, saying why) and on
oxbot's own loop, against a stand-in OpenRouter (chat and Responses APIs; Codex reaches it
through oxbot's relay) and a stand-in browser tool that follow the reply scripts; every test ends
by checking that the stand-in's key (a canary) is in none of oxbot's files, its log or the page.
Steps can check what was sent (`expect_openrouter`, `expect_openrouter_body`) and restart oxbot
mid-test (`restart = true`).

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
