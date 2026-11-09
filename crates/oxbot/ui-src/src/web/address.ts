// The browser side of connecting: the key in the page's address, and the
// phone app's bridge. Browser-only, so it stays out of net/ and state/.

/** What the phone app (a paired device) gives the page: it carries the
 *  connection to the home computer itself, encrypted and checked. */
export interface DeviceBridge {
  socket(): WebSocket;
  homeComputer?: string;
  /** The person this phone is paired as. */
  person?: string;
}

declare global {
  interface Window {
    oxbotDevice?: DeviceBridge;
    /** The same bridge from a phone app made before the rename to "device". */
    oxbotGate?: { socket: () => WebSocket; farm?: string; person?: string };
  }
}

/** The phone app's bridge, from this app or one made before the rename. */
export function deviceBridge(w: Window): DeviceBridge | undefined {
  if (w.oxbotDevice) return w.oxbotDevice;
  const old = w.oxbotGate;
  if (!old) return undefined;
  return { socket: old.socket, ...(old.farm ? { homeComputer: old.farm } : {}), ...(old.person ? { person: old.person } : {}) };
}

/** Storage the key is kept in for this window only. */
export interface KeyStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const KEY_ITEM = "oxbotHomeComputerKey";
/** Where tabs opened before the rename keep the key. */
const OLD_KEY_ITEM = "oxbotFarmKey";
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
    return storage?.getItem(KEY_ITEM) ?? storage?.getItem(OLD_KEY_ITEM) ?? "";
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
