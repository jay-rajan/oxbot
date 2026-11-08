// The cards area. OWNED BY THE Cards PORT: replace this stub.
import type { Action } from "./store";

// biome-ignore lint: filled in by the cards port
export interface CardsState {}

export const initialCards: CardsState = {};

export function cardsReducer(state: CardsState, _action: Action): CardsState {
  return state;
}
