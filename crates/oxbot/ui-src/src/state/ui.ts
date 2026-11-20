// The ui area: which bot is open, which dialog is open, and the toast.
// Shared by every screen; each screen's own details live in its area.
import { HOME_LEADS } from "./sidebar";
import type { Action } from "./store";

/** The Home view a bot leads (its Calendar, Meal Plan…), else the plain chat:
 *  a Home bot's chat always opens in its view, whose switcher leads back. */
function pageFor(bot: string | null): UiState["page"] {
  return HOME_LEADS.find((h) => h.id === bot)?.page ?? "chat";
}

/** The page's dialogs, one open at a time (the toast shows inside it). */
export type DialogName =
  | "newBot"
  | "botSettings"
  | "report"
  | "settings"
  | "welcome"
  | "name"
  | "case"
  | "compare"
  | "pieces";

export interface UiState {
  /** The open bot's id. */
  current: string | null;
  /** A bot to open as soon as it's listed (just made, or remembered). */
  pendingSelect: string | null;
  dialog: DialogName | null;
  /** What the open dialog is about (e.g. the Settings tab, a report id). */
  dialogArg: string | null;
  /** The toast: `n` counts up so the same text shown twice still restarts its timer. */
  toast: { text: string; error: boolean; n: number } | null;
  /** The sidebar on a narrow screen. */
  sideOpen: boolean;
  /** What the main column shows: the open bot's chat, or a Home view. */
  page: "chat" | "calendar" | "notes" | "meals" | "groceries";
  /** How a Home view and its bot's chat share the screen. Each view keeps its own (none yet: Split). */
  homeMode: Record<string, HomeMode>;
  /** In a chat on a phone the tab bar steps away; a tap on the chat, or
   *  finishing typing, brings it back (until the view changes). */
  tabsUp: boolean;
}

/** Split: the view and the chat side by side (stacked on a phone); full:
 *  the view alone; chat: the chat alone. */
export type HomeMode = "split" | "full" | "chat";



export const initialUi: UiState = {
  current: null,
  pendingSelect: null,
  dialog: null,
  dialogArg: null,
  toast: null,
  sideOpen: false,
  page: "chat",
  homeMode: {},
  tabsUp: false,
};

export function uiReducer(state: UiState, action: Action): UiState {
  if (action.kind === "server") {
    const msg = action.msg;
    if (msg.type === "bots" && state.current && !msg.bots.some((b) => b.id === state.current))
      return { ...state, current: null };
    if (msg.type === "notice") return toast(state, msg.message, false);
    if (msg.type === "notify") return toast(state, msg.text, false);
    if (msg.type === "error") return toast(state, msg.message, true);
    return state;
  }
  if (action.kind !== "local") return state;
  switch (action.type) {
    case "ui/select":
      return { ...state, current: action.bot as string | null, pendingSelect: null, sideOpen: false, tabsUp: false, page: action.keepPage ? state.page : pageFor(action.bot as string | null) };
    case "ui/page":
      return { ...state, page: action.page as UiState["page"], sideOpen: false, tabsUp: false };
    case "ui/pendingSelect":
      return { ...state, pendingSelect: action.bot as string | null };
    case "ui/open":
      return { ...state, dialog: action.dialog as DialogName, dialogArg: (action.arg as string | undefined) ?? null };
    case "ui/close":
      return action.dialog && action.dialog !== state.dialog ? state : { ...state, dialog: null, dialogArg: null };
    case "ui/toast":
      return toast(state, action.text as string, Boolean(action.error));
    case "ui/hideToast":
      return state.toast && state.toast.n === action.n ? { ...state, toast: null } : state;
    case "ui/side":
      return { ...state, sideOpen: Boolean(action.open) };
    case "ui/homeMode":
      return { ...state, homeMode: { ...state.homeMode, [action.page as string]: action.mode as HomeMode }, tabsUp: false };
    case "ui/tabsUp":
      return { ...state, tabsUp: Boolean(action.up) };
    default:
      return state;
  }
}

function toast(state: UiState, text: string, error: boolean): UiState {
  return { ...state, toast: { text, error, n: (state.toast?.n ?? 0) + 1 } };
}
