// The names the end-to-end tests call on the page (e2e/tests/*.toml,
// `expect_js`), as the hand-written page had them: send(), bots, toast()…
// Each screen exposes its own with `expose`. Read-only views of the store;
// nothing here is needed by the page itself.
const exposed: Record<string, () => unknown> = {};

/** Makes `window[name]` read `get()` each time. */
export function expose(name: string, get: () => unknown): void {
  exposed[name] = get;
  Object.defineProperty(window, name, { configurable: true, get });
}

/** Makes `window[name]` a function. */
export function exposeFn(name: string, fn: (...args: never[]) => unknown): void {
  expose(name, () => fn);
}
