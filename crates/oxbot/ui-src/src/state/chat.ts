// The chat area. OWNED BY THE Chat PORT: replace this stub.
import type { Action } from "./store";

// biome-ignore lint: filled in by the chat port
export interface ChatState {}

export const initialChat: ChatState = {};

export function chatReducer(state: ChatState, _action: Action): ChatState {
  return state;
}
