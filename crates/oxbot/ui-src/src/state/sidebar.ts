// The sidebar area: what the bot list needs beyond the bots themselves.
// - Each bot's working state and latest reply, kept from `bots`, `event` and
//   `history` (so a working bot's piece moves, and a new reply shows a green
//   dot until the user opens the bot).
// - The "seen" marks, grouping, collapsed groups and the search: this
//   device's alone. The page loads and saves them (`sidebar/load`); this
//   file never touches storage.
// - The Settings gear's warning count.
// - Counters the page watches to pick which bot to open.
import type { BotEvent, BotInfo, SectionInfo, ServerMsg } from "../net/protocol";
import type { Action } from "./store";

export type GroupBy = "sections" | "people";

export interface SidebarState {
  /** Whether each bot is in the middle of a turn. */
  busy: Readonly<Record<string, boolean>>;
  /** The latest stored event we've applied, per bot. */
  lastSeq: Readonly<Record<string, number>>;
  /** The bot's latest reply's number. */
  lastReply: Readonly<Record<string, number>>;
  /** The latest reply the user has seen, per bot. */
  seen: Readonly<Record<string, number>>;
  /** Our copy of the open bot (ui.current). */
  current: string | null;
  /** Whether the page is in view (a reply to the open bot counts as seen only then). */
  visible: boolean;
  query: string;
  groupBy: GroupBy;
  /** Group keys folded shut ("s:<section id>", "p:<name>"). */
  collapsed: readonly string[];
  /** Warnings and errors logged while Settings was closed. */
  warnings: number;
  /** Our copy of the open dialog (ui.dialog). */
  dialog: string | null;
  /** Bumped on every `bots` list, so the page can pick a bot to open. */
  botsN: number;
  /** The latest bot made from this page; `n` counts up so each one is noticed. */
  created: { id: string; n: number } | null;
}

export const initialSidebar: SidebarState = {
  busy: {},
  lastSeq: {},
  lastReply: {},
  seen: {},
  current: null,
  visible: true,
  query: "",
  groupBy: "sections",
  collapsed: [],
  warnings: 0,
  dialog: null,
  botsN: 0,
  created: null,
};

/** Case- and accent-insensitive: "Cafe" finds "Café". */
export function fold(text: string | null | undefined): string {
  return String(text || "")
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase();
}

export interface Group {
  key: string;
  title: string;
  /** Sections mode: the section's id, or null for bots not in one. */
  section?: string | null;
  /** People mode: who made them ("" for the home computer's). */
  person?: string;
  ids: string[];
}

/** The sidebar's groups, top to bottom: Assistant first (no title), then by
 *  section (bots not in one first, without a title) or by who made them
 *  (you first, then A-Z, then the home computer's). Bots keep the server's
 *  order. A search keeps the bots whose names match and drops groups left
 *  empty. */
export function groupBots(
  list: readonly BotInfo[],
  secs: readonly SectionInfo[],
  where: Readonly<Record<string, string>>,
  mode: GroupBy,
  query: string,
  you: string | null | undefined,
): Group[] {
  const q = fold(query).trim();
  const hit = (b: BotInfo) => !q || fold(b.name || b.id).includes(q);
  const herder = list.filter((b) => b.central);
  const rest = list.filter((b) => !b.central);
  const groups: Group[] = [{ key: "herder", title: "", ids: herder.filter(hit).map((b) => b.id) }];
  if (mode === "people") {
    const by = new Map<string, BotInfo[]>();
    for (const b of rest) {
      const who = b.created_by || "";
      const l = by.get(who);
      if (l) l.push(b);
      else by.set(who, [b]);
    }
    const names = [...by.keys()]
      .filter((n) => n)
      .sort((x, y) => (x === you ? -1 : y === you ? 1 : x.localeCompare(y)));
    if (by.has("")) names.push("");
    for (const n of names)
      groups.push({
        key: `p:${n}`,
        title: n || "Home computer",
        person: n,
        ids: (by.get(n) ?? []).filter(hit).map((b) => b.id),
      });
  } else {
    const known = new Set(secs.map((x) => x.id));
    const inSection = (b: BotInfo) => {
      const s = where[b.id];
      return s && known.has(s) ? s : null;
    };
    groups.push({
      key: "none",
      title: "",
      section: null,
      ids: rest.filter((b) => !inSection(b) && hit(b)).map((b) => b.id),
    });
    for (const x of secs)
      groups.push({
        key: `s:${x.id}`,
        title: x.name,
        section: x.id,
        ids: rest.filter((b) => inSection(b) === x.id && hit(b)).map((b) => b.id),
      });
  }
  return q ? groups.filter((g) => g.ids.length) : groups.filter((g) => g.title || g.ids.length);
}

/** Whether a bot has a reply the user hasn't seen (never the open one). */
export function isUnread(state: SidebarState, id: string): boolean {
  return id !== state.current && (state.lastReply[id] ?? 0) > (state.seen[id] ?? 0);
}

function markSeen(seen: Record<string, number>, lastReply: Readonly<Record<string, number>>, id: string): void {
  if (!(id in lastReply)) return;
  seen[id] = Math.max(seen[id] ?? 0, lastReply[id] ?? 0);
}

function viewing(state: SidebarState, id: string): boolean {
  return id === state.current && state.visible;
}

/** What stored events say about the bots (no drawing). */
function applyEvents(state: SidebarState, events: readonly BotEvent[], onlyNew: boolean): SidebarState {
  let busy: Record<string, boolean> | null = null;
  let lastSeq: Record<string, number> | null = null;
  let lastReply: Record<string, number> | null = null;
  let seen: Record<string, number> | null = null;
  for (const e of events) {
    const known = (lastSeq ?? state.lastSeq)[e.bot] ?? 0;
    if (onlyNew && e.seq <= known) continue;
    if (e.seq > known) {
      lastSeq ??= { ...state.lastSeq };
      lastSeq[e.bot] = e.seq;
    }
    if (e.kind === "user_message") {
      busy ??= { ...state.busy };
      busy[e.bot] = true;
    } else if (e.kind === "turn_done") {
      busy ??= { ...state.busy };
      busy[e.bot] = false;
    } else if (e.kind === "assistant_message") {
      lastReply ??= { ...state.lastReply };
      lastReply[e.bot] = Math.max(lastReply[e.bot] ?? 0, e.seq);
      if (viewing(state, e.bot)) {
        seen ??= { ...state.seen };
        markSeen(seen, lastReply, e.bot);
      }
    }
  }
  if (!busy && !lastSeq && !lastReply && !seen) return state;
  return {
    ...state,
    busy: busy ?? state.busy,
    lastSeq: lastSeq ?? state.lastSeq,
    lastReply: lastReply ?? state.lastReply,
    seen: seen ?? state.seen,
  };
}

function onBots(state: SidebarState, list: readonly BotInfo[]): SidebarState {
  // A bot that's gone (deleted, or now private to someone else) leaves.
  // Its "seen" mark goes too: a new bot can get the same id and starts
  // counting from 1.
  const ids = new Set(list.map((b) => b.id));
  const keep = <T>(m: Readonly<Record<string, T>>) => {
    const out: Record<string, T> = {};
    for (const [k, v] of Object.entries(m)) if (ids.has(k)) out[k] = v;
    return out;
  };
  const busy: Record<string, boolean> = {};
  const lastReply: Record<string, number> = {};
  const seen = keep(state.seen);
  const current = state.current && ids.has(state.current) ? state.current : null;
  const next: SidebarState = { ...state, current };
  for (const info of list) {
    busy[info.id] = info.busy;
    lastReply[info.id] = info.last_reply_seq;
  }
  for (const info of list) {
    if (!(info.id in seen)) markSeen(seen, lastReply, info.id); // first time: old replies aren't new
    if (viewing(next, info.id)) markSeen(seen, lastReply, info.id);
  }
  return { ...next, busy, lastReply, seen, lastSeq: keep(state.lastSeq), botsN: state.botsN + 1 };
}

function onServer(state: SidebarState, msg: ServerMsg): SidebarState {
  switch (msg.type) {
    case "bots":
      return onBots(state, msg.bots);
    case "bot_created":
      // Opened once it's listed (or now, if it already is).
      return { ...state, created: { id: msg.id, n: (state.created?.n ?? 0) + 1 } };
    case "event": {
      const { type: _t, ...e } = msg;
      return applyEvents(state, [e as BotEvent], true);
    }
    case "history":
      return applyEvents(state, msg.events, true);
    case "sections": {
      // A section deleted (here or on another device) forgets it was collapsed.
      const ids = new Set(msg.sections.map((x) => `s:${x.id}`));
      const collapsed = state.collapsed.filter((k) => !k.startsWith("s:") || ids.has(k));
      return collapsed.length === state.collapsed.length ? state : { ...state, collapsed };
    }
    case "log":
      return msg.level !== "info" && state.dialog !== "settings" ? { ...state, warnings: state.warnings + 1 } : state;
    default:
      return state;
  }
}

export function sidebarReducer(state: SidebarState, action: Action): SidebarState {
  if (action.kind === "server") return onServer(state, action.msg);
  if (action.kind !== "local") return state;
  switch (action.type) {
    case "sidebar/load": {
      // What this device kept: seen marks, grouping, collapsed groups. Marks
      // already set (a list that came first) are replaced by the kept ones.
      const seen = { ...state.seen, ...((action.seen as Record<string, number> | undefined) ?? {}) };
      const groupBy: GroupBy = action.groupBy === "people" ? "people" : "sections";
      const collapsed = Array.isArray(action.collapsed) ? (action.collapsed as unknown[]).filter((k): k is string => typeof k === "string") : state.collapsed;
      return { ...state, seen, groupBy, collapsed };
    }
    case "sidebar/visible": {
      const visible = Boolean(action.visible);
      if (visible === state.visible) return state;
      const next = { ...state, visible };
      if (visible && state.current && state.current in state.lastReply) {
        const seen = { ...state.seen };
        markSeen(seen, state.lastReply, state.current);
        next.seen = seen;
      }
      return next;
    }
    case "sidebar/query":
      return { ...state, query: String(action.query ?? "") };
    case "sidebar/groupBy":
      return { ...state, groupBy: action.groupBy === "people" ? "people" : "sections" };
    case "sidebar/toggle": {
      const key = String(action.key);
      const collapsed = state.collapsed.includes(key) ? state.collapsed.filter((k) => k !== key) : [...state.collapsed, key];
      return { ...state, collapsed };
    }
    case "ui/select": {
      const id = (action.bot as string | null) ?? null;
      const next = { ...state, current: id };
      if (id && id in state.lastReply) {
        const seen = { ...state.seen };
        markSeen(seen, state.lastReply, id);
        next.seen = seen;
      }
      return next;
    }
    case "ui/open": {
      const dialog = String(action.dialog);
      return { ...state, dialog, warnings: dialog === "settings" ? 0 : state.warnings };
    }
    case "ui/close":
      return action.dialog && action.dialog !== state.dialog ? state : state.dialog === null ? state : { ...state, dialog: null };
    default:
      return state;
  }
}
