// The names the end-to-end tests call on the page (e2e/tests/*.toml,
// `expect_js`), as the hand-written page had them: send(), bots, toast()…
// Each screen exposes its own with `expose`. Read-only views of the store;
// nothing here is needed by the page itself.
//
// They appear only once the server says it's in test mode (its hello's
// `test`, true only in a debug build started for tests): on the app people
// use, no other script on the page finds `window.send` and the rest.

/** The names the page offers the tests, and where they go once allowed. */
export function createTestBridge(target: object) {
  const exposed = new Map<string, () => unknown>();
  let on = false;
  const define = (name: string, get: () => unknown) =>
    Object.defineProperty(target, name, { configurable: true, get });
  return {
    /** Makes `target[name]` read `get()` each time (in test mode). */
    expose(name: string, get: () => unknown): void {
      exposed.set(name, get);
      if (on) define(name, get);
    },
    /** The server said it's in test mode: every name appears, and later ones too. */
    enable(): void {
      if (on) return;
      on = true;
      for (const [name, get] of exposed) define(name, get);
    },
  };
}

const bridge = createTestBridge(globalThis);

/** Makes `window[name]` read `get()` each time, in test mode only. */
export function expose(name: string, get: () => unknown): void {
  bridge.expose(name, get);
}

/** Makes `window[name]` a function, in test mode only. */
export function exposeFn(name: string, fn: (...args: never[]) => unknown): void {
  expose(name, () => fn);
}

/** The server's hello said test mode: the tests' names appear. */
export function enableTestBridge(): void {
  bridge.enable();
}
