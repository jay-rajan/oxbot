# oxbot page (React)

The oxbot page rebuilt in React 18 and TypeScript, built with Vite into
three plain files in `../ui-dist/` (`index.html`, `app.js`, `app.css`). The
oxbot program embeds those files, so building oxbot never needs Node: the
built files are committed.

Until it replaces the hand-written page (`../ui/index.html`), it's served at
`/next/`, with the same key as the main page, e.g.
`http://127.0.0.1:<port>/next/#k=<key>` (the link `oxbot serve` prints,
with `/next/` added).

## Build

```sh
cd crates/oxbot/ui-src
npm ci            # exact versions from package-lock.json, no install scripts (.npmrc)
npm run build     # type-checks, then writes ../ui-dist/
npx vitest run    # unit tests for the state reducers
```

Commit `../ui-dist/` with the change that produced it.
`scripts/check-ui.sh` (from the repo root) checks that the committed files
match a fresh build and that the protocol types are current; it needs Node,
so it isn't part of `cargo test`.

## Protocol types

`src/net/protocol/` is generated from `oxbot-protocol` (ts-rs, behind that
crate's `ts` feature). Don't edit it. After changing a message in Rust:

```sh
cargo test -p oxbot-protocol --features ts -- --ignored export_bindings   # write them
cargo test -p oxbot-protocol --features ts                                # check them
```

## Layout

- `src/main.tsx`: the browser entry: reads the address and key, connects,
  feeds the store, draws `App`.
- `src/web/`: browser-only helpers (the `#k=` key, the phone app's bridge).
- `src/net/socket.ts`: the WebSocket (address, back-off, reconnect, typed send).
- `src/state/`: the store and one reducer per area (`connection`, `bots` so far).

**`src/net/` and `src/state/` (and the generated types) must stay
platform-neutral**: no `document`, `window`, `location`, `localStorage` or
other DOM/browser-only APIs, so a React Native phone app can share them.
WebSocket, timers and `console` are fine. Anything browser-specific is passed
in from `main.tsx` (or lives in `src/web/`). `scripts/check-ui.sh` checks this.
- `src/tokens.css`: the color and type tokens, same names as the main page.

Rules: no requests to anywhere but this server, no inline scripts (the page
runs under a strict Content-Security-Policy), and dependencies pinned to
exact versions.
