// One store for the page, fed by the server's messages, with a reducer per
// area. Components read the parts they need with `useStore`.
import { useSyncExternalStore } from "react";
import type { SocketStatus } from "../net/socket";
import type { ServerMsg } from "../net/protocol";
import { type BotsState, botsReducer, initialBots } from "./bots";
import {
  type ConnectionState,
  connectionReducer,
  initialConnection,
} from "./connection";

/** Something that changes the state: a server message or the socket. */
export type Action =
  | { kind: "server"; msg: ServerMsg }
  | { kind: "status"; status: SocketStatus };

/** Everything the page knows. */
export interface State {
  connection: ConnectionState;
  bots: BotsState;
}

export const initialState: State = {
  connection: initialConnection,
  bots: initialBots,
};

/** Runs every area's reducer; an area that didn't change keeps its object. */
export function reduce(state: State, action: Action): State {
  const connection = connectionReducer(state.connection, action);
  const bots = botsReducer(state.bots, action);
  if (connection === state.connection && bots === state.bots) return state;
  return { connection, bots };
}

/** A tiny store: the state, a way to change it, and who to tell. */
export interface Store {
  getState(): State;
  dispatch(action: Action): void;
  subscribe(fn: () => void): () => void;
}

export function createStore(init: State = initialState): Store {
  let state = init;
  const fns = new Set<() => void>();
  return {
    getState: () => state,
    dispatch(action) {
      const next = reduce(state, action);
      if (next === state) return;
      state = next;
      for (const fn of fns) fn();
    },
    subscribe(fn) {
      fns.add(fn);
      return () => fns.delete(fn);
    },
  };
}

/** Reads part of the state; the component redraws when that part changes. */
export function useStore<T>(store: Store, select: (state: State) => T): T {
  return useSyncExternalStore(store.subscribe, () => select(store.getState()));
}
