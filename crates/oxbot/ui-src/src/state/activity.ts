// The Settings window's own state: which tab is open, the Add a device form,
// whose row is open in People, and Settings → Activity (what left the home
// computer, newest first). Sending is the screen's job; this only keeps
// what the page and the server said.
import type { NetKind, NetRow, NetTotals } from "../net/protocol";
import type { Action } from "./store";

export type SettingsTab = "setGeneral" | "setSignins" | "setPeople" | "setSchedules" | "setActivity";
export const SETTINGS_TABS: readonly SettingsTab[] = ["setGeneral", "setSignins", "setPeople", "setSchedules", "setActivity"];

/** `ui/open {dialog:"settings", arg}`: a tab id, or this to open People with Add a device. */
export const ADD_DEVICE = "addDevice";

export interface NetLog {
  /** The chip: "" for All. */
  kind: NetKind | "";
  rows: NetRow[];
  /** Today's sums; null while loading. */
  totals: NetTotals | null;
  /** Older rows to page in. */
  more: boolean;
  /** The row opened for its details. */
  open: number | null;
  /** An older page was asked for (its rows go after the ones we have). */
  askingOlder: boolean;
}

export interface ActivityState {
  tab: SettingsTab;
  /** People → Add a device's form is showing. */
  pairFormOpen: boolean;
  /** Why a pairing code can't be shown (no Wi-Fi phones can join). */
  pairWhy: string | null;
  /** Whose row is open in People. */
  personOpen: string | null;
  net: NetLog;
}

export const initialNet: NetLog = { kind: "", rows: [], totals: null, more: false, open: null, askingOlder: false };

export const initialActivity: ActivityState = {
  tab: "setGeneral",
  pairFormOpen: false,
  pairWhy: null,
  personOpen: null,
  net: initialNet,
};

function isTab(t: unknown): t is SettingsTab {
  return typeof t === "string" && (SETTINGS_TABS as readonly string[]).includes(t);
}

export function activityReducer(state: ActivityState, action: Action): ActivityState {
  if (action.kind === "server") {
    const msg = action.msg;
    switch (msg.type) {
      case "net_log": {
        const older = state.net.askingOlder;
        return {
          ...state,
          net: {
            ...state.net,
            rows: older ? state.net.rows.concat(msg.rows) : msg.rows,
            totals: msg.totals,
            more: msg.more,
            askingOlder: false,
          },
        };
      }
      // A code came: the form gives way to it.
      case "pairing":
        return state.pairFormOpen ? { ...state, pairFormOpen: false } : state;
      default:
        return state;
    }
  }
  if (action.kind !== "local") return state;
  switch (action.type) {
    case "ui/open": {
      if (action.dialog !== "settings") return state;
      const arg = action.arg;
      if (arg === ADD_DEVICE)
        return { ...state, tab: "setPeople", pairFormOpen: true, pairWhy: null };
      // No tab named: it opens where it was left.
      return isTab(arg) ? { ...state, tab: arg } : state;
    }
    case "activity/tab":
      return isTab(action.tab) && action.tab !== state.tab ? { ...state, tab: action.tab } : state;
    case "activity/pairForm":
      return { ...state, pairFormOpen: Boolean(action.open), pairWhy: null };
    case "activity/pairWhy":
      return { ...state, pairWhy: (action.why as string | null) ?? null };
    case "activity/person": {
      const name = action.name as string;
      return { ...state, personOpen: state.personOpen === name ? null : name };
    }
    case "activity/netKind":
      return { ...state, net: { ...initialNet, kind: (action.netKind as NetKind | "") ?? "" } };
    case "activity/netOlder":
      return { ...state, net: { ...state.net, askingOlder: true } };
    case "activity/netToggle": {
      const id = action.id as number;
      return { ...state, net: { ...state.net, open: state.net.open === id ? null : id } };
    }
    default:
      return state;
  }
}
