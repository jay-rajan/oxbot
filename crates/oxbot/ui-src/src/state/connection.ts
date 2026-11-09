// The connection area: whether we're connected, to which server, and the
// app settings it sent.
import type { AppSettings, ServerMsg } from "../net/protocol";
import type { Action } from "./store";

export interface ConnectionState {
  status: "connecting" | "open" | "closed";
  /** Whether a connection ever opened (a lost one shows as lost). */
  everConnected: boolean;
  /** The server's name, e.g. the Mac's name; empty until `hello`. */
  serverName: string;
  serverVersion: string;
  /** When the server started; a change means it restarted. */
  startedMs: number | null;
  settings: AppSettings | null;
}

export const initialConnection: ConnectionState = {
  status: "connecting",
  everConnected: false,
  serverName: "",
  serverVersion: "",
  startedMs: null,
  settings: null,
};

/** Drops the `type` tag from a message that carries a whole struct. */
function untagged<T extends { type: string }>(msg: T): Omit<T, "type"> {
  const { type: _type, ...rest } = msg;
  return rest;
}

function onServer(state: ConnectionState, msg: ServerMsg): ConnectionState {
  switch (msg.type) {
    case "hello":
      return {
        ...state,
        serverName: msg.server_name,
        serverVersion: msg.server_version,
        startedMs: msg.started_ms,
      };
    case "settings":
      return { ...state, settings: untagged(msg) };
    default:
      return state;
  }
}

export function connectionReducer(
  state: ConnectionState,
  action: Action,
): ConnectionState {
  if (action.kind === "server") return onServer(state, action.msg);
  if (action.kind !== "status") return state;
  if (action.status === state.status) return state;
  return {
    ...state,
    status: action.status,
    everConnected: state.everConnected || action.status === "open",
  };
}
