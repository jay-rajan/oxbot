// What every component can reach: the store, sending to the server, and a
// few shared helpers. Given once by main.tsx through <AppContext.Provider>.
import { createContext, useContext } from "react";
import type { ClientMsg } from "../net/protocol";
import { type Action, type State, type Store, useStore } from "../state/store";

export interface App {
  store: Store;
  /** Sends if connected; says whether it went. */
  send(msg: ClientMsg): boolean;
  dispatch(action: Action): void;
  /** Shows a message at the bottom (inside the open dialog, if any). */
  toast(text: string, error?: boolean): void;
  /** Whether this page came with the home computer's key, or a test `?as=` name. */
  hasKey: boolean;
  /** The home computer's key for this window ("" if none), for uploads. */
  key: string;
  /** The phone app's connection, when this page runs inside it. */
  phone: { homeComputer?: string; person?: string } | null;
}

export const AppContext = createContext<App | null>(null);

/** The app, in a component. */
export function useApp(): App {
  const app = useContext(AppContext);
  if (!app) throw new Error("useApp outside <AppContext.Provider>");
  return app;
}

/** Reads part of the state in a component. */
export function useSel<T>(select: (state: State) => T): T {
  return useStore(useApp().store, select);
}
