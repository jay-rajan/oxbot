// The chat area: each bot's chat as this page has it (a window of its
// history, what's streaming in, what it's doing), the files waiting to be
// sent, the message box, dictation, and the research reports opened here.
// Pure: sending to the server is done by the chat screen, which tells this
// reducer what it asked for (`chat/subscribed`, `chat/olderAsked`, …).
import type { Attachment, BotEvent, ReportView, ServerMsg } from "../net/protocol";
import type { Action } from "./store";

/** Events per history page. */
export const PAGE = 200;
/** Pages kept for a chat that isn't open. */
export const KEEP_PAGES = 3;
/** Opened reports kept in this page, to read again offline. */
export const KEEP_REPORTS = 20;

/** The chat's footer after a turn on the AI account: model, requests, cost. */
export interface Meter {
  model: string;
  requests: number;
  cost_usd: number;
}

/** One bot's chat. */
export interface BotChat {
  events: BotEvent[];
  lastSeq: number;
  firstSeq: number;
  hasOlder: boolean;
  loadingOlder: boolean;
  /** The `gen` of each older page asked for, oldest ask first. */
  olderAsks: number[];
  /** Bumped when the chat is trimmed: older pages asked before then are dropped. */
  gen: number;
  busy: boolean;
  /** Whether we follow it (opened once on this page). */
  subscribed: boolean;
  /** Text streamed in for the reply being written. */
  stream: string | null;
  /** What it's doing, e.g. "Working…". */
  activity: string | null;
  /** Meters shown in the open chat, each after the event it came after. */
  meters: { after: number; meter: Meter }[];
  /** Counts history pages received (the screen pages back after each). */
  histN: number;
}

/** A file attached to the message box: uploading, ready, sending, or refused. */
export interface Chip {
  key: number;
  bot: string;
  name: string;
  size: number;
  state: "uploading" | "ready" | "sending" | "error";
  progress: number;
  att: Attachment | null;
  error: string;
  /** The words sent with it (to put back if the send fails). */
  text?: string;
}

/** The owner's thumbs and kept test cases, per bot. */
export interface Ratings {
  ratings: Record<number, boolean>;
  cases: number[];
}

export interface ChatState {
  /** The open bot (a copy of ui.current, kept here for trimming and sendFailed). */
  current: string | null;
  bots: Readonly<Record<string, BotChat>>;
  /** Files waiting to be sent, per bot. */
  drafts: Readonly<Record<string, Chip[]>>;
  chipSeq: number;
  /** The message box's text. */
  input: string;
  /** While dictating: the text before and after the cursor. */
  dictating: { before: string; after: string } | null;
  /** Where the cursor goes after dictation changed the box; `n` counts. */
  caret: { at: number; focus: boolean; n: number } | null;
  /** A message for the toast from here; `n` counts. */
  notice: { text: string; error: boolean; n: number } | null;
  /** Whether a connection attempt ever failed (the offline bar shows then). */
  failedOnce: boolean;
  ratings: Readonly<Record<string, Ratings>>;
  /** Kept reports by "bot:id", oldest opened first. */
  reports: Readonly<Record<string, ReportView>>;
  reportOrder: string[];
  /** The report asked for, until it comes. */
  wantReport: string | null;
  /** A report that just came for the ask: the screen opens it. */
  reportReady: string | null;
}

export const initialChat: ChatState = {
  current: null,
  bots: {},
  drafts: {},
  chipSeq: 0,
  input: "",
  dictating: null,
  caret: null,
  notice: null,
  failedOnce: false,
  ratings: {},
  reports: {},
  reportOrder: [],
  wantReport: null,
  reportReady: null,
};

export function newBotChat(): BotChat {
  return {
    events: [],
    lastSeq: 0,
    firstSeq: 0,
    hasOlder: false,
    loadingOlder: false,
    olderAsks: [],
    gen: 0,
    busy: false,
    subscribed: false,
    stream: null,
    activity: null,
    meters: [],
    histN: 0,
  };
}

export function reportKey(bot: string, id: string): string {
  return `${bot}:${id}`;
}

/** "bot:id" back into its parts (bot ids have no colon). */
export function splitReportKey(key: string): { bot: string; id: string } | null {
  const i = key.indexOf(":");
  return i > 0 ? { bot: key.slice(0, i), id: key.slice(i + 1) } : null;
}

/** Whether files are still uploading or being sent for this bot. */
export function uploading(state: ChatState, bot: string | null): boolean {
  return !!bot && (state.drafts[bot] ?? []).some((c) => c.state === "uploading" || c.state === "sending");
}

function chatOf(state: ChatState, bot: string): BotChat {
  return state.bots[bot] ?? newBotChat();
}

function withBot(state: ChatState, bot: string, b: BotChat): ChatState {
  return { ...state, bots: { ...state.bots, [bot]: b } };
}

/** What a stored event says about the bot's state (and the files it took). */
function applyEvent(state: ChatState, b: BotChat, e: BotEvent): { state: ChatState; b: BotChat } {
  if (e.kind === "user_message") {
    b = { ...b, busy: true, activity: "Working…" };
    if (e.files?.length) state = filesSent(state, e.bot, e.files);
  }
  if (e.kind === "turn_done") b = { ...b, busy: false, activity: null };
  if (e.kind === "assistant_message") b = { ...b, stream: null };
  return { state, b };
}

/** The message with these files is in the chat: their chips go. */
function filesSent(state: ChatState, bot: string, files: Attachment[]): ChatState {
  const ids = new Set(files.map((f) => f.id));
  const chips = state.drafts[bot];
  if (!ids.size || !chips) return state;
  return { ...state, drafts: { ...state.drafts, [bot]: chips.filter((c) => !(c.att && ids.has(c.att.id))) } };
}

/** A send didn't go through: its files can be sent again, and its words
 *  come back if the box is still empty. */
function sendFailed(state: ChatState): ChatState {
  let drafts = state.drafts;
  let input = state.input;
  for (const [bot, chips] of Object.entries(state.drafts)) {
    const sending = chips.filter((c) => c.state === "sending");
    if (!sending.length) continue;
    drafts = { ...drafts, [bot]: chips.map((c) => (c.state === "sending" ? { ...c, state: "ready" as const } : c)) };
    const text = sending[0]?.text;
    if (bot === state.current && text && !input) input = text;
  }
  return drafts === state.drafts && input === state.input ? state : { ...state, drafts, input };
}

/** Keeps a chat we've left to its last few pages. */
function trim(b: BotChat): BotChat {
  const keep = KEEP_PAGES * PAGE;
  if (b.events.length <= keep) return b;
  const events = b.events.slice(-keep);
  return { ...b, events, firstSeq: events[0]?.seq ?? 0, hasOlder: true, loadingOlder: false, gen: b.gen + 1, meters: [] };
}

function onEvent(state: ChatState, e: BotEvent): ChatState {
  let b = chatOf(state, e.bot);
  if (e.seq <= b.lastSeq) return state;
  // A chat that starts with live events can still page back.
  if (!b.events.length) b = { ...b, firstSeq: e.seq, hasOlder: e.seq > 1 };
  b = { ...b, lastSeq: e.seq, events: [...b.events, e] };
  const r = applyEvent(state, b, e);
  return withBot(r.state, e.bot, r.b);
}

function onHistory(state: ChatState, m: Extract<ServerMsg, { type: "history" }>): ChatState {
  let b = chatOf(state, m.bot);
  const ev = m.events;
  const first = b.events[0];
  const lastNew = ev[ev.length - 1];
  b = { ...b, histN: b.histN + 1 };
  // Replies come in the order asked. An older page asked for before the
  // chat was trimmed no longer joins what we have: drop it.
  const olderShaped = lastNew ? !!first && lastNew.seq < first.seq : b.loadingOlder || b.olderAsks.length > 0;
  if (olderShaped && b.olderAsks.length) {
    const [gen, ...rest] = b.olderAsks;
    b = { ...b, olderAsks: rest };
    if (gen !== b.gen) return withBot(state, m.bot, b);
  }
  const askedOlder = b.loadingOlder;
  b = { ...b, loadingOlder: false };
  // An earlier page nobody asked for can't be placed: ignore it.
  if (olderShaped && ev.length && !askedOlder) return withBot(state, m.bot, b);
  // A page that starts after what we have, with a gap between: start afresh.
  if (b.events.length && ev[0] && ev[0].seq > b.lastSeq + 1) b = { ...b, events: [], lastSeq: 0, meters: [] };
  const head = b.events[0];
  if (head && lastNew && lastNew.seq < head.seq) {
    // An earlier page goes in front.
    b = { ...b, events: [...ev, ...b.events], firstSeq: m.first_seq, hasOlder: m.has_older };
  } else if (askedOlder && !ev.length) {
    // An earlier page with nothing readable still moves us back.
    b = { ...b, firstSeq: m.first_seq || b.firstSeq, hasOlder: !!m.first_seq && m.has_older };
  } else {
    if (!b.events.length) b = { ...b, firstSeq: m.first_seq, hasOlder: m.has_older };
    else if (!ev.length) b = { ...b, hasOlder: b.hasOlder && m.has_older };
    const added: BotEvent[] = [];
    let lastSeq = b.lastSeq;
    for (const e of ev) if (e.seq > lastSeq) { lastSeq = e.seq; added.push(e); }
    if (added.length) {
      b = { ...b, lastSeq, events: [...b.events, ...added] };
      for (const e of added) { const r = applyEvent(state, b, e); state = r.state; b = r.b; }
    }
  }
  return withBot(state, m.bot, b);
}

function keepReport(state: ChatState, m: ReportView): ChatState {
  const key = reportKey(m.bot, m.id);
  const order = [...state.reportOrder.filter((k) => k !== key), key];
  const reports: Record<string, ReportView> = { ...state.reports, [key]: m };
  while (order.length > KEEP_REPORTS) { const old = order.shift(); if (old) delete reports[old]; }
  const ready = state.wantReport === key;
  return { ...state, reports, reportOrder: order, wantReport: ready ? null : state.wantReport, reportReady: ready ? key : state.reportReady };
}

function forgetReports(state: ChatState, bots: Set<string>): ChatState {
  const gone = state.reportOrder.filter((k) => bots.has(k.slice(0, k.indexOf(":"))));
  if (!gone.length) return state;
  const reports = { ...state.reports };
  for (const k of gone) delete reports[k];
  return { ...state, reports, reportOrder: state.reportOrder.filter((k) => !gone.includes(k)) };
}

function onServer(state: ChatState, msg: ServerMsg): ChatState {
  switch (msg.type) {
    case "event": {
      const { type: _type, ...e } = msg;
      return onEvent(state, e as BotEvent);
    }
    case "history":
      return onHistory(state, msg);
    case "delta": {
      const b = chatOf(state, msg.bot);
      return withBot(state, msg.bot, { ...b, stream: (b.stream ?? "") + msg.text });
    }
    case "activity": {
      const b = chatOf(state, msg.bot);
      // A new request: text streamed before it and never kept goes.
      const stream = msg.text === "Working…" ? null : b.stream;
      return withBot(state, msg.bot, { ...b, activity: msg.text, stream });
    }
    case "meter": {
      // Shown in the open chat only, like the hand-written page.
      if (msg.bot !== state.current || !msg.requests) return state;
      const b = chatOf(state, msg.bot);
      const meter = { model: msg.model, requests: msg.requests, cost_usd: msg.cost_usd };
      return withBot(state, msg.bot, { ...b, meters: [...b.meters, { after: b.lastSeq, meter }] });
    }
    case "bots": {
      const listed = new Set(msg.bots.map((b) => b.id));
      const gone = Object.keys(state.bots).filter((id) => !listed.has(id));
      const bots: Record<string, BotChat> = {};
      for (const [id, b] of Object.entries(state.bots)) if (listed.has(id)) bots[id] = b;
      for (const info of msg.bots) bots[info.id] = { ...(bots[info.id] ?? newBotChat()), busy: info.busy };
      // Gone, or someone else's private bot now: their reports go.
      const forget = new Set([...gone, ...msg.bots.filter((b) => b.locked).map((b) => b.id)]);
      const current = state.current && listed.has(state.current) ? state.current : null;
      return forgetReports({ ...state, bots, current }, forget);
    }
    case "report": {
      const { type: _type, ...r } = msg;
      return keepReport(state, r);
    }
    case "ratings":
      return { ...state, ratings: { ...state.ratings, [msg.bot]: { ratings: msg.ratings as Record<number, boolean>, cases: msg.cases } } };
    case "dictation": {
      const d = state.dictating;
      if (!d) return state;
      const at = (d.before + msg.text).length;
      const n = (state.caret?.n ?? 0) + 1;
      const next: ChatState = { ...state, input: d.before + msg.text + d.after, caret: { at, focus: msg.done, n } };
      if (!msg.done) return next;
      return { ...next, dictating: null, notice: msg.error ? notice(state, msg.error, true) : state.notice };
    }
    case "settings":
      return !msg.dictation && state.dictating ? { ...state, dictating: null } : state;
    case "error":
      return sendFailed(state);
    default:
      return state;
  }
}

function notice(state: ChatState, text: string, error: boolean) {
  return { text, error, n: (state.notice?.n ?? 0) + 1 };
}

function setChips(state: ChatState, bot: string, f: (chips: Chip[]) => Chip[]): ChatState {
  return { ...state, drafts: { ...state.drafts, [bot]: f(state.drafts[bot] ?? []) } };
}

export function chatReducer(state: ChatState, action: Action): ChatState {
  if (action.kind === "server") return onServer(state, action.msg);
  if (action.kind === "status") {
    if (action.status === "open") {
      // Each followed chat catches up again; pages asked before are void.
      const bots: Record<string, BotChat> = {};
      for (const [id, b] of Object.entries(state.bots)) bots[id] = b.subscribed ? { ...b, loadingOlder: false, olderAsks: [] } : b;
      return { ...state, bots };
    }
    if (action.status === "closed") {
      // A send in flight may not have arrived: its files can be sent again.
      return sendFailed({ ...state, dictating: null, failedOnce: true });
    }
    return state;
  }
  switch (action.type) {
    case "ui/select": {
      const id = (action.bot as string | null) ?? null;
      let next = state;
      const prev = state.current && state.bots[state.current];
      if (prev && state.current !== id) next = withBot(next, state.current as string, trim(prev));
      if (id) next = withBot(next, id, { ...chatOf(next, id), meters: id === state.current ? chatOf(next, id).meters : [] });
      return { ...next, current: id };
    }
    case "chat/subscribed": {
      const bot = action.bot as string;
      return withBot(state, bot, { ...chatOf(state, bot), subscribed: true });
    }
    case "chat/olderAsked": {
      const bot = action.bot as string;
      const b = chatOf(state, bot);
      return withBot(state, bot, { ...b, loadingOlder: true, olderAsks: [...b.olderAsks, b.gen] });
    }
    case "chat/input":
      return { ...state, input: action.text as string };
    case "chat/dictating":
      return { ...state, dictating: (action.at as ChatState["dictating"]) ?? null };
    case "chat/notice":
      return { ...state, notice: notice(state, action.text as string, Boolean(action.error)) };
    case "chat/addChip": {
      const c = action.chip as Omit<Chip, "key">;
      const key = state.chipSeq + 1;
      return { ...setChips(state, c.bot, (chips) => [...chips, { ...c, key }]), chipSeq: key };
    }
    case "chat/chip": {
      const bot = action.bot as string;
      const key = action.key as number;
      const patch = action.patch as Partial<Chip>;
      return setChips(state, bot, (chips) => chips.map((c) => (c.key === key ? { ...c, ...patch } : c)));
    }
    case "chat/removeChip": {
      const key = action.key as number;
      return setChips(state, action.bot as string, (chips) => chips.filter((c) => c.key !== key));
    }
    case "chat/sent": {
      // The chips stay, "sending…", until the message shows in the chat;
      // refused ones go.
      const text = action.text as string;
      return {
        ...setChips(state, action.bot as string, (chips) =>
          chips.filter((c) => c.state !== "error").map((c) => (c.state === "ready" ? { ...c, state: "sending" as const, text } : c)),
        ),
        input: "",
      };
    }
    case "chat/wantReport":
      return { ...state, wantReport: (action.key as string | null) ?? null };
    case "chat/reportShown":
      return { ...state, reportReady: null };
    default:
      return state;
  }
}
