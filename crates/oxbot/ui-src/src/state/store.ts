// One store for the page, fed by the server's messages and by the page's own
// actions, with a reducer per area. Components read the parts they need with
// `useStore`. Every reducer sees every action and returns its own state
// unchanged when the action isn't for it.
import { useSyncExternalStore } from "react";
import type { SocketStatus } from "../net/socket";
import type { ServerMsg } from "../net/protocol";
import { type BotsState, botsReducer, initialBots } from "./bots";
import { type ConnectionState, connectionReducer, initialConnection } from "./connection";
import { type HomeState, homeReducer, initialHome } from "./home";
import { type UiState, initialUi, uiReducer } from "./ui";
import { type ChatState, chatReducer, initialChat } from "./chat";
import { type CardsState, cardsReducer, initialCards } from "./cards";
import { type SidebarState, initialSidebar, sidebarReducer } from "./sidebar";
import { type ActivityState, activityReducer, initialActivity } from "./activity";

/** A page action: `type` is "<area>/<what>", e.g. "ui/select". */
export interface LocalAction {
  kind: "local";
  type: string;
  [field: string]: unknown;
}

/** Something that changes the state: a server message, the socket, or the page. */
export type Action =
  | { kind: "server"; msg: ServerMsg }
  | { kind: "status"; status: SocketStatus }
  | LocalAction;

/** Everything the page knows. */
export interface State {
  connection: ConnectionState;
  bots: BotsState;
  home: HomeState;
  ui: UiState;
  chat: ChatState;
  cards: CardsState;
  sidebar: SidebarState;
  activity: ActivityState;
}

export const initialState: State = {
  connection: initialConnection,
  bots: initialBots,
  home: initialHome,
  ui: initialUi,
  chat: initialChat,
  cards: initialCards,
  sidebar: initialSidebar,
  activity: initialActivity,
};

/** Runs every area's reducer; an area that didn't change keeps its object. */
export function reduce(state: State, action: Action): State {
  const next: State = {
    connection: connectionReducer(state.connection, action),
    bots: botsReducer(state.bots, action),
    home: homeReducer(state.home, action),
    ui: uiReducer(state.ui, action),
    chat: chatReducer(state.chat, action),
    cards: cardsReducer(state.cards, action),
    sidebar: sidebarReducer(state.sidebar, action),
    activity: activityReducer(state.activity, action),
  };
  const same = (Object.keys(next) as (keyof State)[]).every((k) => next[k] === state[k]);
  return same ? state : next;
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

/** A page action for `dispatch`. */
export function local(type: string, fields: Record<string, unknown> = {}): LocalAction {
  return { kind: "local", type, ...fields };
}
