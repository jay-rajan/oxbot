# oxbot

Always-on bots on your Mac, used through the oxbot desktop app and the oxbot Android app on your
home Wi-Fi. Each bot runs on Claude (your own Claude plan) or OpenRouter (free models first; see
[Engines](#engines)). Model calls go only to Anthropic (Claude bots), OpenRouter (OpenRouter bots),
or a local model; nothing else: no relay servers, no push services, telemetry off.

Status: desktop app on the Mac plus the Android Gate app (milestone Foundations, issue #1). Shared
bots share one hidden home Chrome that keeps your sign-ins: a bot that needs an account asks, a
Chrome window opens, you sign in once, and every bot on that Chrome stays signed in (a sign-in
counts only if the window left a new cookie). Settings → Sign-ins lists the signed-in sites (the
sidebar's Signed in link opens it); open one, or sign out of it alone. Bots take turns on the browser. oxbot checks every browser step: links, tabs, search,
filters, sorting and paging run at once; a red card comes before anything that changes something
(Send, Post, Save, Apply…, or a filled-in POST form's button), and before paying, booking or
deleting; a card comes before an upload from your Mac; and it refuses passwords, page scripts,
non-web pages and files outside the bot's folder. oxbot names what a click aims at from every page
snapshot the bot has read (also the large ones Claude saves to a file), and for an unclear button
it reads the button's form itself, in its own isolated world the page's scripts can't touch. A
click oxbot still can't name runs, and is logged. A change card can offer **Always allow** (to the
bot's owner or the Farm): the same bot may then click the same button on the same exact host
without asking, with a quiet line in the chat. Paying, booking, deleting, Enter in a text box,
uploads and Herder's cards never offer it. The bot's ··· settings list its rules, each with Remove.
Open cards sit right above the message box, newest nearest it, whichever bot is open; the card
area scrolls on its own so the box stays on screen, on a phone too.

The sidebar: search finds a bot by name as you type (Enter opens the first match). Group bots by
section or by who made them. Sections are your own: make, rename and delete them, and drag a bot
onto one or use its menu; the Farm keeps them for each person, so your Mac and phone match, and
nobody else sees yours. You may file any bot you're shown, even someone else's: it changes only
your view. Right-click a bot (long-press on a phone) to rename it, make it private or shared,
move it to a section or delete it, with the same rules as its ··· settings. Herder stays at the
top. The bottom row: Settings, Add a Gate (the Farm only) and Signed in.

Research reports: a bot with the browser hands back research (comparing options, looking things
up across sites) as a report, with a 2–4 line reply and a card in the chat; quick answers stay
in chat. The report has the question, a short answer first, sections and tables, and numbered
citations [1] linked to a Sources list (title, site, link, when it was read). Every source must
be a page the bot opened in the browser in that conversation, or oxbot refuses the save. oxbot
keeps reports on the Farm (`oxbot.db`) and renders them itself, safely: raw HTML shows as text,
no images, links only to the report's sources or pages the bot opened, a strict CSP, and the app
shows it in a sandboxed frame with no scripts. Reports open over the app's own connection (no
link that carries a key), for whoever may open the bot, by its rules at the time; source links
open in the system browser. The bot's ··· lists its reports (anyone who may open the bot);
managers delete them. Deleting a bot deletes them (`reports.jsonl` in the trash keeps a copy).
A phone keeps the reports it opened for reading away from home.

Files and voice: attach files to a message with the paperclip, by dropping them on the chat, or
by pasting; several per message (up to 10), and a message may be files alone. Each uploads at
once (`POST /upload`, on this Mac only; sending files from a Gate comes later) and shows as a
chip; oxbot checks it by its contents and says plainly when it can't take it (archives, audio,
programs, password-locked PDFs, over 25 MB, or videos over 200 MB). Files are saved in the bot's
own folder, `work/inbox/<date>/`, kept until the bot is deleted, and shown as chips on the sent
message (on the Farm, a chip opens the file on the Mac). Claude opens them with its own Read
tool: pictures and PDFs as they are; text, CSV, JSON and code as text; Word, Excel and PowerPoint
turned into text on the Mac (in Rust); old Word, RTF and OpenDocument text with `textutil`; iPhone
photos and very large pictures as a JPEG made with `sips`. Claude can't watch video, so oxbot
takes up to 8 evenly spaced still frames and says so in the chat. oxbot's note to Claude lists
the files by path and says their text is information, not instructions; reading one marks the
session as having read untrusted text, as web pages do. The microphone button dictates with the
Mac's own on-device speech recognition (`requiresOnDeviceRecognition`): the audio never leaves
the Mac, the text fills the box at the cursor and stays editable. It shows only on the Farm, and
only where the Mac can dictate on-device (Settings → Setup says why not), in the browser page
and the app alike, because the Farm does the listening. Frames and dictation come from
`oxbot-mac`, a small Swift helper (`mac/`, built by `scripts/build-mac-helper.sh`) inside the
app; oxbot built without Swift still works, and video and the mic then say they're not
available. macOS asks for the microphone and speech recognition the first time.

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
own copy of its chats (the latest 300 messages per bot, 30 days) and of the last 50 reports it
opened. Remove a phone in Settings →
Gates and it's cut off at once. `OXBOT_GATE_PORT=off` turns the door off.

Who is the Farm: only oxbot's own windows. The app opens its window with the Farm secret (a key
in the data folder, readable only by your Mac account); `oxbot chat` and `oxbot bots` read it;
`oxbot serve` prints a link with it. Other pages and programs on the Mac can't act as the Farm.

Setup: oxbot installs Claude Code for you (Anthropic's installer) and signs you in
(`claude auth login`); the browser tool and Node.js ship inside the app. You only need Google
Chrome. Settings (the gear) shows setup, your Claude usage and oxbot's activity log.

Usage alerts: when Claude usage passes 50% and again at 80% of your plan's 5-hour or weekly
limit, oxbot shows a banner (until the window resets, or you dismiss it) and posts a Mac
notification, once per level per window, restarts included. Each alert names the limit, when it
resets and the models your bots use; model-specific limits don't alert. Usage is as Claude last
reported it, after a bot's reply. In the app, a bot waiting for your OK or a sign-in while the
window is hidden also posts a notification, with the bot's name only (just "A bot" for someone
else's private bot). Both switch off in Settings → General → Notifications (the Farm only); off,
levels are still noted, so turning alerts back on brings no late one. `oxbot serve` posts through
`osascript` (shown as Script Editor); an app showing an `oxbot serve` that was already running
leaves notifications to it. macOS asks for permission the first time.

Brand (`docs/brand/`): the "O"xbot wordmark (a round O with red horns, then xbot in bold) and
the same O with horns on a black tile as the app icon. Minimal UI: white backgrounds, white
buttons with red text. Each bot gets a little ox avatar.

New bot from the terminal: `cargo run -p oxbot -- bots new "Daily News" --instructions "..."`.

The chat page also opens in a normal browser (`cargo run -p oxbot -- serve --open`), but that's
for testing and dev mode only: the desktop app is the way to use oxbot.
Plan: https://claude.ai/artifact/32dqY3emzXCfBj9QGkF5rZ · Architecture: https://claude.ai/artifact/3mVTriA2zwH7yzSL9D98F6 (rebuild: `python3 docs/architecture/build.py`)

## Engines

Every bot runs on an engine. **OpenRouter** (the default) runs oxbot's own agent loop on
OpenRouter's models, free ones first, with your household's OpenRouter key; anyone in the
household can give these bots tasks. **Claude** runs Claude Code on the Farm owner's own Claude
plan.

> **Claude bots run on your own Claude plan. Anthropic's terms don't let others use your
> account, so only you can give them tasks. Everyone else uses OpenRouter bots.**

oxbot enforces this itself: other people, their phones, their Herders, and tasks they started
can't reach a Claude bot (others still read what it shares; their message box says who can
give it tasks). Herder (yours) runs on Claude; everyone else's Herder runs on OpenRouter. A
bot's ··· settings switch its engine; switching a bot others use to Claude says first that it
becomes owner-only. Bots made before engines stay on Claude: on the first start the owner gets a
banner, and Settings → Engines lists the Claude bots other people used in the last 30 days, each
with **Switch to OpenRouter** (or **Switch all**).

Comparing engines (the owner only): under a finished reply, 👍/👎 and **Test case…** keep the
task; **Compare…** in the bot's menu replays test cases on Claude and on OpenRouter models, side
by side with a scoreboard (your marks, or Claude as judge on request). A replay never acts: every
step that would send, post or change something is recorded instead (no card, nothing in the
chat), in its own browser with a copy of the sign-ins it needs (no history, deleted when it ends)
that blocks every request but reads, and WebSockets. **Shadow** in the message box runs your own
messages on another model at the same time, the same way. Comparisons the owner starts may send a
test case to any engine the owner picks; Claude runs and judges only tasks the owner started.
Herder's tasks can't be test cases yet (they pass work on to other bots).

OpenRouter: the Farm signs in once in Settings → Engines (**Sign in with OpenRouter**: OpenRouter
opens in your browser and comes back to oxbot on this Mac, PKCE with a one-time state). The key
lives only in your Mac's Keychain (one item per data folder), never in a file, the log, the
database or the page; signing out deletes it (to revoke the key itself, delete "oxbot on {your
Mac}" at openrouter.ai/keys). The same tab shows the key's limits and today's use, the free
models that can run bots (read from OpenRouter's model list, which needs no key), daily request
limits per bot and per person, an off-by-default switch for paid models when every free one is
busy (with a daily cap), and an off-by-default switch to ask providers not to keep data for
every bot (private bots always ask).

How OpenRouter bots work: oxbot runs the agent loop itself, one step at a time. It asks a model,
checks the tool the model picked with the same rules as for Claude bots before it runs (red cards,
Always allow, refusing passwords, page scripts, pages on this Mac or the home network and files
outside the bot's folder; a Herder that has read web pages still asks before tasking a private
bot), runs it and saves the step. A task that was running when oxbot stopped goes on from its last
saved step when oxbot starts again, without repeating a click. The tools are the same as Claude's:
the browser (chrome-devtools-mcp, in the same Chrome), files in the bot's folder, Herder's team
tools and save_report, plus ask_user. Under each reply: the model, the requests it took and the
cost.

Models and limits: oxbot reads OpenRouter's model list and uses free models that can call tools
and hold at least 64k tokens, the best first; when one is busy or rate-limited (429), the next
free model answers. Free models allow 20 requests a minute (oxbot keeps the household under that)
and a number a day that OpenRouter sets (50, or 1,000 once the account has bought $10 of credit),
shown in Settings → Engines. Each bot may make 200 model requests a day (change it for all in
Settings → Engines, or for one bot in its ··· settings), and each person optionally a number too;
a bot that reaches a limit stops and says so, and the usage banner shows it. When OpenRouter's
free requests for the day run out, bots say so until midnight UTC, or use paid models if the owner
switched them on (up to the daily dollar cap).

What leaves the Mac: a Claude bot's conversation (your messages, and the pages and files it reads)
goes to Anthropic through Claude Code, with telemetry and error reports off. An OpenRouter bot's
conversation goes to OpenRouter and the provider it routes to, and free providers may keep or
learn from it. Private bots ask for providers that don't collect data and keep nothing
(`data_collection: deny` and zero retention); if no free model qualifies, the bot says so instead
of switching quietly (it may first drop zero retention, never the deny, and says that too).
Signing in, the model list and the key's limits also go to openrouter.ai. Chats, saved steps and
the meter (requests, tokens and cost, never prompt text) stay on the Farm in `oxbot.db`. Nothing
else leaves the Mac.

## Layout
| Crate | What it is |
|---|---|
| `crates/oxbot-protocol` | Wire types shared by the server and the apps |
| `crates/oxbot-core` | Bots, people (ids), store (`store/`), hub (`hub/`, split by job; engines and the owner-only rule), tool rules (policy), prompts, the Claude engine (no networking) |
| `crates/oxbot-engine` | The engine interface both engines plug into: `Engine`, `Session`, one event stream |
| `crates/oxbot-claude` | Runs `claude -p` with privacy settings (Claude bots) |
| `crates/oxbot-loop` | oxbot's own agent loop (OpenRouter bots): runner, tools, gate check, model router, pacer, meter |
| `crates/oxbot-llm` | Model client for OpenAI-style chat APIs (OpenRouter now, local models later), the key, sign-in (PKCE) |
| `crates/oxbot-browser` | A hidden Chrome per owner (home, or a person's own) with a kept profile, and its sign-in window |
| `crates/oxbot` | The `oxbot` command: `serve`, `chat`, `bots`, `pair`, `devices`, `doctor`; the chat page; the Gate door (`src/door/`) |
| `crates/oxbot-gate` | The Gate side of the link: pairing link, pinned TLS client, the phone's chat copy (builds for Android) |
| `app/src-tauri` | The desktop app and the Android Gate app (Tauri 2) |
| `mac/` | `oxbot-mac`, the Swift helper inside the app: video frames and on-device dictation |

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
(`e2e/seed/`: bots plus `seed.sql` for a fresh SQLite database), the stand-in Claude's replies
(`e2e/claude/`), files to attach (`e2e/fixtures/`), and approved screenshots (`e2e/baselines/`).
A step can `attach = ["e2e/fixtures/bill.pdf"]`; a test with `dictation = true` gets a stand-in
Mac helper that "hears" a sentence (no test uses the real microphone), and video goes to the real
helper for real still frames. The runner, `crates/oxbot-e2e`, starts a
throwaway oxbot in test mode (`OXBOT_E2E=1`), drives its page in hidden Chrome (and Safari, the app's
engine, when "Allow Remote Automation" is on), screenshots every step, and checks the built Mac app.
Bots on OpenRouter run against a stand-in OpenRouter and a stand-in browser tool that read the
same reply scripts as the stand-in Claude; every test ends by checking that the stand-in's key (a
canary) is in none of oxbot's files, its log or the page. Steps can check what was sent
(`expect_openrouter`, `expect_openrouter_body`) and restart oxbot mid-test (`restart = true`).

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
