# Oxbot native Camoufox runtime

First implementation milestone: a pinned, reduced native-Mac package and a
Node lifecycle primitive. This is **not yet a live bot backend or a signed
release**. Chrome remains the production integration until authorization,
sign-in privacy, isolated reads, practice-run network isolation, and task pacing
have been ported and tested. See `docs/plan/camoufox-release.md`.

From the repository root:

```sh
npm ci --prefix scripts/camoufox --ignore-scripts --no-audit --no-fund
node scripts/camoufox/build.mjs
npm test --prefix scripts/camoufox
OXBOT_CAMOUFOX_BUNDLE="$PWD/target/camoufox-bundle" npm run smoke --prefix scripts/camoufox
OXBOT_CAMOUFOX_BUNDLE="$PWD/target/camoufox-bundle" OXBOT_CAMOUFOX_SMOKE_NO_DOWNLOADS=1 npm run smoke --prefix scripts/camoufox
```

The build runs on the target Mac architecture. It fetches a checksum-pinned
upstream browser archive, omits `Contents/Resources/fonts`, and copies locked
`playwright-core` dependencies. It emits `bundle.json` and `sizes.json`. The
package reuses Oxbot's existing Node: no Python, Chrome, Chromium, second
Firefox, fingerprint generator, or fingerprint model is included. Nothing is
installed into the app or the user's browser profiles by this build command.
The original archive is large, but is a build cache, not a release resource.

The runtime preflights the explicit browser executable and bundled add-on,
fails on missing/corrupt assets instead of downloading, and keeps its profile
outside the read-only app. It launches the Camoufox engine directly through
Playwright with native defaults. No generated navigator/font/GPU identity is
needed for a personal assistant on its actual host OS. Inherited Camoufox
configuration is removed; only the bundled add-on paths are configured. The
existing bundled uBlock extension is retained, along with its packaged lists.

This differs from using Camoufox's general-purpose TypeScript launcher, whose
fingerprint model expands to ~213 MB and which probes the GPU in an additional
browser process. Those components are unnecessary for native operation. The
only browser dependency here is the pinned `playwright-core`; browser downloads
are disabled by construction because an explicit, verified executable is used.

A duplicate profile launch is refused. Stale locks after a hard process kill
currently require recovery; production crash recovery is a later lifecycle
gate. Profile metadata records only backend/platform/architecture, not process
environment or a synthetic identity. Normal browser site storage persists.

A small extension controls only the browser's own window state. It has no
website permissions, content scripts, external messaging, or page access.
Its only network permission is loopback, to this launch's random-capability
window channel; that channel accepts only window-state commands/results. The
extension keeps automation minimized; restoring for manual use disables that
behavior. Startup accepts no target URL and must confirm minimized state before
returning control. The adapter must pause all bot reads/actions before calling
`showForUser()`; that policy is not implemented by this primitive itself.

The smoke test uses a local fixture, a new profile, and no retailer traffic.
It tests minimized navigation/interactions, rendering, storage and native
browser identity across restart, zero page restoration requests on startup,
and duplicate-profile refusal. The no-download mode rejects Node fetches and non-loopback connections and compares every bundle file before/after the run. `OXBOT_CAMOUFOX_SMOKE_HEADLESS=1` is an explicit
asset-test mode that skips window tests; it is never a fallback for failed
minimization. Signed distribution, Windows, and human verification still need
separate validation.

The browser remains derived from Camoufox/Firefox (MPL-2.0); preserve the
browser's notices and source references when distributing. Playwright retains
its own license and notices in `node_modules`.

Sources:
- https://github.com/daijro/camoufox/blob/main/docs/FONTS.md
- https://playwright.dev/docs/api/class-browsertype#browser-type-launch-persistent-context
- https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/windows/update

The current generated bundle is about 390 MB (browser ~376 MB, Playwright
~13 MB, small runtime files), excluding the Node/Codex tools already shipped
with Oxbot. An OS-level offline sandbox test is not certified: the same
sandbox also prevented the unmodified upstream browser from starting here.
Clean-machine offline distribution remains a release gate.
