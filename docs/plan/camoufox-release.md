# Release Oxbot with reduced Camoufox

Status: implementation started, 2026-11-12. The native runtime/package milestone is implemented and locally tested; the live bot adapter and signed release are not implemented.
Baseline: `d300046` commits the existing pacing, visible Chrome, sign-in pause, model fallback, and Mac signing work.

## Decisions

- Ship reduced Camoufox **inside oxbot.app**, replacing Chrome as the production browser. No Chrome installation, separate browser download, Python, pip, or user setup commands are required.
- Reuse Oxbot's bundled Node runtime. Use pinned `playwright-core` directly with the Camoufox engine and its native defaults; do not download Chromium or another stock Firefox alongside Camoufox. The general-purpose launcher/fingerprint generator is not needed for this native-only assistant.
- Support only the host's native macOS identity and system fonts in this package. No cross-OS identities, identity rotation, proxy rotation, or automatic browser switching after a block.
- Start a normal visible browser on a blank page, minimize it, then navigate to the requested site. Bring it forward for sign-in or explicit manual control; resume automation only after the handoff ends.
- Optimize for a few useful actions and low site load, not throughput. A grocery task is a sequence of ordinary UI interactions, not a crawler or a direct shopping API client.

## Implementation progress

The first implementation is in `scripts/camoufox/`: a checksum-pinned browser builder, locked Playwright runtime, native profile lifecycle, private window-state extension/channel, and local smoke tests. Generated binaries stay in `target/`; nothing has changed the current bots' Chrome integration or release packaging yet.

The direct native route avoids the TypeScript launcher's generated identities, extra GPU-probe browser, and ~213 MB expanded fingerprint model. The current generated bundle measures **about 389.7 MB (~390 MB)**: 376,239,295 browser bytes and 13,441,941 dependency bytes, plus small runtime files. It contains no Python, fingerprint generator/model, or Chrome engine. Node is reused from Oxbot's existing tools.

Local tests use Oxbot's bundled Node 24: minimize before navigation, stay minimized through a local form action, capture a rendered multilingual screenshot, restore for manual control, preserve site storage/native identity through restart, and avoid reloading the previous website at startup. Unit tests cover pinned-input integrity, missing assets, native-only configuration, output preservation, reduced-font enforcement, and the private window channel. No retailer was visited by these tests.

The OS `sandbox-exec` offline harness timed out with both the prototype and an unmodified upstream Camoufox baseline. It is not accepted as an offline-distribution result. A separate guarded smoke test refuses Node downloads/non-loopback Node connections, restricts fixture page requests to localhost, and checks installed bundle contents remain unchanged. A true offline clean-desktop test, signing/notarization, crash recovery, and control/isolation parity remain release gates.

Next milestone: connect the lifecycle primitive to a Rust-owned browser adapter and per-bot tool sessions, preserving policy mediation before live browsing is enabled.

## Size evidence and limits

Measured locally on Apple Silicon with Camoufox `158.0.1-beta.41`:

| Item | Measured size |
| --- | ---: |
| Original browser app, file bytes | 2,532,716,913 bytes (~2.53 GB) |
| Browser app with `Contents/Resources/fonts` omitted | 376,239,295 bytes (~376 MB) |
| ZIP of the reduced browser, before release signing | 157,670,427 bytes (~158 MB) |
| Current Chrome DevTools MCP package in the tools bundle | ~15 MiB; dependency savings require a final bundle comparison |

These are browser-only figures, not the final Oxbot installer size. Node, Codex, the bridge, Playwright dependencies, signatures, and profiles must be included in the final size report. The native implementation deliberately excludes model data. Node and Codex already ship with Oxbot and must not be duplicated. The local Python environment is a test convenience and must not enter the release.

The reduced copy passed native-Mac launch and local form interactions. Font widths and layout matched the full browser across Latin, French accents, Tamil, Hindi, Chinese, Japanese, Arabic, and emoji; final headless screenshots matched byte-for-byte. Visible-window screenshot capture was inconsistent in both copies. This experiment does **not** establish signing, minimized-window reliability, Intel support, or Walmart verification success.

Chrome is currently reused from the user's Mac, not bundled. Removing Chrome integration saves its tooling, not the user's existing Chrome installation. Do not uninstall Chrome or touch the Jayadev profile. Offer removal of **Oxbot-owned** old Chrome profiles only after migration is verified; deleting them loses their sign-ins.

## 1. Prove the production runtime and package

1. Pin a compatible browser release, Playwright version, and all npm dependencies. The current native runtime has only `playwright-core`; re-evaluate dependencies when adding the bridge. Do not infer compatibility from the Python test.
2. Run a small Node proof using the reduced browser's explicit executable path, actual macOS browser defaults, system fonts, and a persistent test profile. Verify an existing profile stays consistent across restarts; do not generate identities.
3. Inventory every launch-time dependency and metadata/update fetch. The native route deliberately excludes the fingerprint model and GPU probe; default add-ons come from the browser bundle. Package necessary assets at build time and configure explicit bundled paths. No runtime fetch, dependency installation, GeoIP lookup, or silent full-browser fallback.
4. With external network blocked, launch from a fresh Mac user with empty browser/model caches, no Python, and no Chrome. Load a local fixture, render multilingual text and emoji, type, click, capture a screenshot, and restart with saved state.
5. Measure the final dependency tree and reject duplicated engines, Node runtimes, Python environments, full-font copies, development dependencies, caches, and downloaded archives in the app.

Deliverable: reproducible, native-only reduced browser package and offline Node smoke test. A missing required asset is a setup error, not permission to fetch one at launch.

## 2. Replace the Chrome protocol without losing controls

The current browser crate is substantially Chrome-specific. A changed executable path is insufficient.

| Area | Required change |
| --- | --- |
| `crates/oxbot-browser` | Introduce backend-neutral lifecycle, page, element, screenshot, cookie, download, and visit interfaces; implement Camoufox through a Node/Playwright helper. |
| Browser ownership | Rust owns one helper and persistent context per browser owner/profile. Per-bot tool sessions share that owner through the existing exclusive browser lease, not by opening the same profile in separate processes. |
| Helper transport | Use private stdio or a capability-protected local broker. Do not expose a general unauthenticated Playwright/DevTools server. Per-session calls remain bound to the correct owner, bot, work folder, and approval gate. |
| `crates/oxbot-loop/src/tools/browser.rs`, Codex tool names/config | Replace the Chrome MCP dependency with an Oxbot-owned MCP facade. Map tools to backend-neutral actions and preserve tool restrictions, side-effect classification, result persistence, cancellation, and interrupted-call handling. Migrate Chrome-prefixed internal names together with policy tests. |
| `guard.rs`, `element.rs`, `visits.rs` | Restore element facts, stale-element rejection, approval screenshots, download destinations, and activity accounting. Prove page code cannot forge the facts used for authorization; a plain page-world evaluation is not assumed equivalent to the existing isolated-world checks. |
| `netguard.rs`, `sealed.rs`, `trial.rs` | Reimplement and test isolation and network restrictions. Account for new tabs, redirects, frames, workers, service workers, sockets, WebRTC, beacons, and close-time requests. Playwright request routing alone is not proof of parity. Unsupported isolation must refuse the operation. |
| `cookies.rs`, hub sign-ins | Preserve owner boundaries, site sign-out, cookie-change detection without logging values, and the distinction between shared and personal profiles. Do not copy Chrome profile files into Firefox. |
| Setup/runtime/UI | Replace Chrome-required setup checks and browser wording with bundled Camoufox checks and clear errors. Keep the Mac helper for its existing native functions; the browser bridge uses the existing Node. |

During sign-in/manual control, cancel pending automated reads and reject new reads/actions, including snapshots, screenshots, and model tool calls. Do not record credentials, page contents, or sensitive request data during that handoff. On resume, invalidate previous element references and re-read state.

Keep Chrome as a development reference while building parity. The release artifact and production setup must not require Chrome or ship Chrome DevTools MCP. Do not silently fall back to Chrome when Camoufox is blocked.

## 3. Enforce respectful operation in code

Existing baseline: one site's explicit navigations are spaced by 5 seconds and actions by 1.5 seconds; pacing is shared across bots in the hub. It includes user-approved actions. This is not a total HTTP request limiter and does not cover every interaction that can cause a navigation.

Proposed initial release policy:

- One active automated workflow per registrable site across all owners/bots. One deliberate interaction in flight; no speculative tabs, parallel product searches, link prefetching by the assistant, or background crawling.
- Wait at least **3 seconds between interactions**, and **8 seconds between search submissions or top-level navigation attempts**, measured at execution. Wait longer for the relevant page/cart result to settle. Include Enter submissions, form submits, navigation-producing clicks, redirects, and retries in classification. Do not use whole-page `networkidle` as the only readiness condition on a busy site.
- Apply pacing in the common action executor after approval, not only through a tool-name list. Cancellation must prevent queued actions from running. A delayed queued action must not cause a burst when execution resumes. Inspect actions' effects before proceeding.
- Fill a search field once and submit once. Observe any autocomplete requests it causes; do not simulate keystrokes or cursor movement to disguise automation. Read existing page content locally before requesting more pages.
- Reuse an existing page/result and browser cache where appropriate. Persist task progress and the last confirmed cart state. Never replay an uncertain add/remove/payment action: inspect current state or hand off before continuing.
- Track first-party document, search, and cart activity separately from normal images/scripts. Coalesce repeated searches and duplicate navigation attempts. Initial circuit breaker: pause after **3 equivalent search/navigation attempts without progress**, or **30 searches / 60 top-level navigations per task**; show progress and require an explicit continuation. These are conservative proposed budgets to validate with grocery tasks, not a promise that every site permits that traffic.
- On HTTP 429, honor `Retry-After` and suspend the site's queue. Without a usable value, pause at least 5 minutes. Allow at most one subsequent automatic retry of a safe read; repeated throttling stops the site's task. Temporary server failures back off; mutations are not automatically retried. Persist the site's cooldown across app restarts so restarting cannot reset it.
- Stop at human verification, access-denied blocks, or explicit refusal. Bring the window forward for the user when useful; no reload loop, alternate identity, proxy, or browser cycling. Resume only after the user explicitly requests it.
- Count requests and status codes with bounded, privacy-preserving activity summaries; omit cookie values, authorization headers, request bodies, and sensitive URL queries. Do not silently turn on tracing or HAR recording.

A normal site can load many resources and background requests from one navigation. Pace limits cannot guarantee a maximum total request count. Detect sustained background activity while idle; stop automation and pause/close the site's page with a clear status if it continues causing excessive activity. Validate this behavior on local fixtures with bursty requests, rather than repeatedly testing Walmart.

## 4. Package, sign, and migrate

Extend `scripts/fetch-tools.sh` and `scripts/make-dmg.sh`:

1. Fetch a pinned upstream archive in CI and verify its checksum/provenance. Make an explicit native-only derivative that omits only the tested bundled-font directory. Keep the metadata and browser fallback resources required by the native runtime. Record upstream version, checksums, packaging recipe, and licenses/notices/source references.
2. Stage `Camoufox.app`, the Node helper, locked production dependencies, and required data under `oxbot.app/Contents/Resources/tools/`. No runtime mutation of these resources; profiles and temporary files belong in the user's Oxbot data folder.
3. Produce separate Apple Silicon and Intel installers. Test the reduced build on each supported macOS/architecture combination; do not label untested Intel builds supported.
4. Sign nested browser executables, frameworks, native npm modules, and helper components inside-out using appropriate browser/JIT entitlements, then sign the enclosing apps and DMG. The upstream app is ad-hoc signed; the existing mic-helper entitlements are not assumed sufficient for Firefox. Verify, notarize, staple, and test Gatekeeper on a clean Mac without bypassing warnings.
5. Couple browser/Playwright updates to a tested Oxbot release. Replace the app as a unit; keep writable profiles outside it. Test rollback using a compatible profile backup, not by assuming an upgraded Firefox profile can be downgraded.
6. Create versioned Camoufox profiles for home and personal browser owners. Explain that websites need a fresh sign-in. Keep old Oxbot Chrome profiles until the user has checked migration. Never delete the regular Chrome installation, profiles, or the full experimental Camoufox installation as an incidental upgrade step.

The release size report must compare old and new DMG and installed app sizes, identify the added browser and removed Chrome-only dependency tree, and confirm there is exactly one shipped engine. The ~158 MB ZIP is an experimental reference, not a final DMG estimate.

## 5. Release gates and implementation order

1. **Runtime/package proof:** reduced native Node launch works offline; multilingual rendering, persisted profile, and accurate total size are verified.
2. **Lifecycle:** visible window starts blank, minimizes **before target navigation**, loads/operates correctly while minimized, comes forward for manual use, and resumes safely. Cover window close, restart, sleep/wake, lock/unlock, idle shutdown, and crash recovery. Resolve the current screenshot inconsistency.
3. **Controls:** all existing authorization, ownership, sign-in privacy, file/download, sealed-read, and practice-run tests pass with Camoufox. Browser APIs cannot bypass the common gate.
4. **Courtesy:** local server fixtures record ordered interactions, no queued-action bursts, no duplicate cart mutations, enforced budgets, shared pacing, persistent cooldowns, and stop-at-challenge behavior. Test unexpected popups, workers, redirects, and cancellation during a delay. Test task recovery without resubmitting already completed steps.
5. **Distribution:** signed/notarized per-chip DMG opens and browses on a clean Mac with no Chrome/Python/cache. Browser startup makes no dependency/update/model download requests. Install, upgrade, uninstall, and rollback preserve unrelated user data.
6. **Small manual pilot:** one user-approved grocery item with normal UI actions, then a weekly list. Use local fixtures for repeated testing; do not repeatedly load Walmart to test verification. Report requests, pauses, task completion, and rendering failures without promising challenge acceptance.
7. **Default switch:** after the gates pass, make bundled Camoufox the production default, remove Chrome setup requirements and Chrome MCP from the release, and document the profile migration. Retire remaining Chrome-specific production code once migration is validated; dev-only fixtures must not inflate release artifacts.

No release until lifecycle, control parity, and distribution gates pass. Do not weaken an existing protection to meet the switch date.

## Sources

- [Camoufox TypeScript launcher](https://github.com/daijro/camoufox/blob/main/typescript/README.md): Node/Playwright integration, persistent profiles, and launch behavior.
- [Camoufox font implementation](https://github.com/daijro/camoufox/blob/main/docs/FONTS.md): native macOS system-font behavior and bundled-font assumptions.
- [Playwright browser contexts](https://playwright.dev/docs/api/class-browsercontext): context isolation and lifecycle APIs; capability parity still requires Oxbot-specific tests.
- Local reduced-browser experiment and install on 2026-11-12. Test-only files are outside the repository; add a reproducible tracked smoke test in implementation step 1.
