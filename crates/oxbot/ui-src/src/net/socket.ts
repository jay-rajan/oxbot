// The WebSocket to the oxbot server: the same address, key and back-off as
// the hand-written page (crates/oxbot/ui/index.html, "connection").
//
// Platform-neutral: no DOM or browser-only APIs here (only WebSocket and
// timers), so a phone app can share it. The page's address, key and
// browser events come in from main.tsx (see ../web/).
import type { ClientMsg, ServerMsg } from "./protocol";

/** Where the connection is. */
export type SocketStatus = "connecting" | "open" | "closed";

/** WebSocket's readyState values (not read from a global, to stay neutral). */
const OPEN = 1;
const CLOSED = 3;

/** The WebSocket address: `/ws` on `host`, with the key, or (test mode
 *  only) `as`, the person the page names. */
export function socketUrl(opts: {
  host: string;
  secure: boolean;
  key: string;
  as?: string | null;
}): string {
  const q = new URLSearchParams();
  if (opts.as) q.set("as", opts.as);
  else if (opts.key) q.set("k", opts.key);
  const scheme = opts.secure ? "wss" : "ws";
  const query = q.toString();
  return `${scheme}://${opts.host}/ws${query ? `?${query}` : ""}`;
}

/** How long to wait before try number `retry` (0, 1, 2…). */
export function backoffMs(retry: number, phone: boolean): number {
  return Math.min(phone ? 5000 : 10000, 500 * 2 ** retry);
}

/** How to connect. */
export interface ConnectOptions {
  /** Opens a new WebSocket (or a WebSocket-like object from the phone app). */
  open: () => WebSocket;
  /** On a phone: retries come faster. */
  phone: boolean;
  /** After a drop, whether to wait for `reconnectNow` instead of retrying
   *  on a timer (a phone app in the background can't reach the network). */
  holdRetries?: () => boolean;
}

/** A live connection that reconnects by itself. */
export interface Socket {
  /** Sends if connected; says whether it went. */
  send(msg: ClientMsg): boolean;
  /** Calls `fn` for every message from the server; returns a stop function. */
  onMessage(fn: (msg: ServerMsg) => void): () => void;
  /** Calls `fn` whenever the connection opens or closes. */
  onStatus(fn: (status: SocketStatus) => void): () => void;
  /** Tries now instead of waiting out the back-off (the network or the app
   *  is back). Does nothing while connected or connecting. */
  reconnectNow(): void;
}

/** Connects, and keeps reconnecting. */
export function connect(opts: ConnectOptions): Socket {
  const messageFns = new Set<(msg: ServerMsg) => void>();
  const statusFns = new Set<(status: SocketStatus) => void>();
  let ws: WebSocket | null = null;
  let retry = 0;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;

  const setStatus = (status: SocketStatus) => {
    for (const fn of statusFns) fn(status);
  };

  const send = (msg: ClientMsg): boolean => {
    if (!ws || ws.readyState !== OPEN) return false;
    ws.send(JSON.stringify(msg));
    return true;
  };

  const open = () => {
    setStatus("connecting");
    ws = opts.open();
    ws.onopen = () => {
      retry = 0;
      setStatus("open");
      send({ type: "list_bots" });
    };
    ws.onclose = () => {
      setStatus("closed");
      clearTimeout(retryTimer);
      if (opts.holdRetries?.()) return;
      retryTimer = setTimeout(open, backoffMs(retry++, opts.phone));
    };
    ws.onmessage = (e: MessageEvent) => {
      let msg: ServerMsg;
      try {
        msg = JSON.parse(String(e.data)) as ServerMsg;
      } catch (err) {
        console.error(err);
        return;
      }
      for (const fn of messageFns) {
        try {
          fn(msg);
        } catch (err) {
          console.error(err);
        }
      }
    };
  };

  open();
  return {
    send,
    onMessage(fn) {
      messageFns.add(fn);
      return () => messageFns.delete(fn);
    },
    onStatus(fn) {
      statusFns.add(fn);
      return () => statusFns.delete(fn);
    },
    reconnectNow() {
      if (ws && ws.readyState !== CLOSED) return;
      clearTimeout(retryTimer);
      retry = 0;
      open();
    },
  };
}
