// The activity area. OWNED BY THE Activity PORT: replace this stub.
import type { Action } from "./store";

// biome-ignore lint: filled in by the activity port
export interface ActivityState {}

export const initialActivity: ActivityState = {};

export function activityReducer(state: ActivityState, _action: Action): ActivityState {
  return state;
}
