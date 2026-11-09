// The browser side of connecting: the key in the page's address, and the
// phone app's bridge. Browser-only, so it stays out of net/ and state/.

/** What the phone app (a paired device) gives the page: it carries the
 *  connection to the home computer itself, encrypted and checked. */
export interface GateBridge {
  socket(): WebSocket;
  farm?: string;
  /** The person this phone is paired as. */
  person?: string;
}

declare global {
  interface Window {
    oxbotGate?: GateBridge;
  }
}

/** Storage the key is kept in for this window only. */
export interface KeyStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const KEY_ITEM = "oxbotFarmKey";
const KEY_IN_HASH = /(?:^#|&)k=([0-9a-f]{32,})/;

/**
 * The home computer's key: oxbot opens its own window with `#k=…`, and only
 * a page that shows it counts as the home computer. Kept for this window
 * only (sessionStorage), and taken out of the address bar so it isn't
 * copied along with the link.
 */
export function takeKey(
  loc: { hash: string; pathname: string; search: string },
  storage: KeyStorage | null,
  replaceUrl: (url: string) => void,
): string {
  try {
    const m = KEY_IN_HASH.exec(loc.hash);
    if (m?.[1]) {
      storage?.setItem(KEY_ITEM, m[1]);
      replaceUrl(loc.pathname + loc.search);
      return m[1];
    }
    return storage?.getItem(KEY_ITEM) ?? "";
  } catch {
    // Storage blocked: the key works until this page reloads.
    return KEY_IN_HASH.exec(loc.hash)?.[1] ?? "";
  }
}

/** This window's sessionStorage, or null where it's blocked. */
export function sessionStore(): KeyStorage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}
