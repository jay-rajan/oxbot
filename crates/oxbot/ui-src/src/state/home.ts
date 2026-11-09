// The home area: what the server says about the home as a whole, kept as
// sent (latest wins). Every screen reads it; nothing here is drawn.
import type {
  DoorState,
  EnginesView,
  DeviceInfo,
  LogLine,
  PairingCode,
  Person,
  ProfileView,
  SectionInfo,
  SetupReport,
  SiteInfo,
  UsageAlert,
} from "../net/protocol";
import type { Action } from "./store";

export interface HomeState {
  setup: SetupReport | null;
  /** Everyone, with their color, role and photo. */
  people: Person[];
  /** This person's own profile. */
  profile: ProfileView | null;
  /** Bumped when someone's profile changed (Settings → People asks again). */
  profilesChanged: number;
  /** The home browser's sign-ins. */
  homeSites: SiteInfo[];
  /** This person's own browser's sign-ins (owner only). */
  mySites: SiteInfo[];
  /** Websites set to fine-grained control. */
  fineSites: string[];
  /** This person's sidebar sections, and which section each bot is in. */
  sections: SectionInfo[];
  placed: Record<string, string>;
  engines: EnginesView | null;
  /** The OpenRouter sign-in page, if no window opened. */
  orAuthUrl: string | null;
  devices: DeviceInfo[];
  door: DoorState | null;
  pairing: PairingCode | null;
  /** Live usage alerts, by limit name. */
  usageAlerts: Record<string, UsageAlert>;
  /** When this connection's hello came (alerts before +3 s are the banner coming back). */
  helloAt: number;
  /** The home computer's log lines (owner's developer view). */
  logs: LogLine[];
}

export const initialHome: HomeState = {
  setup: null,
  people: [],
  profile: null,
  profilesChanged: 0,
  homeSites: [],
  mySites: [],
  fineSites: [],
  sections: [],
  placed: {},
  engines: null,
  orAuthUrl: null,
  devices: [],
  door: null,
  pairing: null,
  usageAlerts: {},
  helloAt: 0,
  logs: [],
};

const MAX_LOGS = 500;

/** Drops the `type` tag from a message that carries a whole struct. */
function untagged<T extends { type: string }>(msg: T): Omit<T, "type"> {
  const { type: _type, ...rest } = msg;
  return rest;
}

export function homeReducer(state: HomeState, action: Action): HomeState {
  if (action.kind !== "server") return state;
  const msg = action.msg;
  switch (msg.type) {
    case "hello":
      // Open cards and live alerts are sent again after hello.
      return { ...state, usageAlerts: {}, helloAt: Date.now(), logs: [] };
    case "setup":
      return { ...state, setup: untagged(msg) };
    case "people":
      return { ...state, people: msg.people };
    case "profile":
      return { ...state, profile: untagged(msg) };
    case "profiles_changed":
      return { ...state, profilesChanged: state.profilesChanged + 1 };
    case "sites":
      return msg.owner ? { ...state, mySites: msg.sites } : { ...state, homeSites: msg.sites };
    case "fine_grained_sites":
      return { ...state, fineSites: msg.sites };
    case "sections": {
      const placed: Record<string, string> = {};
      for (const [bot, section] of Object.entries(msg.placed)) if (section) placed[bot] = section;
      return { ...state, sections: msg.sections, placed };
    }
    case "engines":
      return { ...state, engines: untagged(msg) };
    case "open_router_auth":
      return { ...state, orAuthUrl: msg.url };
    case "devices":
      return { ...state, devices: msg.devices };
    case "door":
      return { ...state, door: untagged(msg) };
    case "pairing":
      return { ...state, pairing: untagged(msg) };
    case "pairing_closed":
      return { ...state, pairing: null };
    case "usage_alert":
      return { ...state, usageAlerts: { ...state.usageAlerts, [msg.limit]: untagged(msg) } };
    case "log": {
      const logs = [...state.logs, untagged(msg)];
      return { ...state, logs: logs.length > MAX_LOGS ? logs.slice(-MAX_LOGS) : logs };
    }
    default:
      return state;
  }
}
