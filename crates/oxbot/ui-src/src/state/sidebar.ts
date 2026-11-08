// The sidebar area. OWNED BY THE Sidebar PORT: replace this stub.
import type { Action } from "./store";

// biome-ignore lint: filled in by the sidebar port
export interface SidebarState {}

export const initialSidebar: SidebarState = {};

export function sidebarReducer(state: SidebarState, _action: Action): SidebarState {
  return state;
}
