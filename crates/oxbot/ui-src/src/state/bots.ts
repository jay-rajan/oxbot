// The bots area: every bot this person can see, in the server's order.
import type { BotInfo } from "../net/protocol";
import type { Action } from "./store";

export interface BotsState {
  /** Bot ids, in the order the server listed them. */
  order: string[];
  byId: Readonly<Record<string, BotInfo>>;
  /** A bot just made from this page, to open once it's listed. */
  justCreated: string | null;
}

export const initialBots: BotsState = { order: [], byId: {}, justCreated: null };

export function botsReducer(state: BotsState, action: Action): BotsState {
  if (action.kind !== "server") return state;
  const msg = action.msg;
  switch (msg.type) {
    case "bots": {
      // The full list: a bot that's gone (deleted, or now private to
      // someone else) leaves it.
      const byId: Record<string, BotInfo> = {};
      for (const info of msg.bots) byId[info.id] = info;
      return { ...state, order: msg.bots.map((b) => b.id), byId };
    }
    case "bot_created": {
      const { type: _type, ...info } = msg;
      const known = info.id in state.byId;
      return {
        order: known ? state.order : [...state.order, info.id],
        byId: { ...state.byId, [info.id]: info },
        justCreated: info.id,
      };
    }
    default:
      return state;
  }
}

/** The bots in order, for drawing a list. */
export function botList(state: BotsState): BotInfo[] {
  return state.order.flatMap((id) => {
    const info = state.byId[id];
    return info ? [info] : [];
  });
}
