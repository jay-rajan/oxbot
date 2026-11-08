// The cards area: open approval requests and sign-ins (the cards above the
// message box), schedule cards being changed, the open bot's reports and
// schedules (its ··· settings), and the owner's developer tools for
// comparing models (ratings, test cases, runs, scoreboard).
import type {
  Approval,
  BatchEstimate,
  BotEvent,
  CaseDraft,
  Day,
  ReportInfo,
  RunInfo,
  ScheduleInfo,
  ScheduleRule,
  ScoreRow,
  SignInRequest,
  TestCaseInfo,
} from "../net/protocol";
import type { Action } from "./store";

/** A card numbered by when it first came (`at`); one sent again keeps its place. */
export type CardApproval = Approval & { at: number };
export type CardSignIn = SignInRequest & { at: number };

/** A schedule card being changed before Create. */
export interface SchedEdit {
  open: boolean;
  task: string;
  kind: ScheduleRule["kind"];
  time: string;
  date: string;
  days: Day[];
  day: number | string;
}

export interface BatchProgress {
  batch: string;
  bot: string;
  done: number;
  total: number;
  status: string;
  paused_reason: string | null;
}

export interface RunDetail {
  run: RunInfo;
  events: BotEvent[];
  original: RunInfo | null;
  original_events: BotEvent[];
}

export type CmpTab = "cmpScore" | "cmpCases" | "cmpRuns";

/** The Compare dialog's bot and what the server sent about it. */
export interface CompareState {
  bot: string | null;
  tab: CmpTab;
  cases: TestCaseInfo[];
  runs: RunInfo[];
  rows: ScoreRow[];
  batch: BatchProgress | null;
  detail: RunDetail | null;
  /** The estimate line, and whether Start may be pressed. */
  estimate: string;
  canStart: boolean;
}

export interface CardsState {
  seq: number;
  approvals: Readonly<Record<number, CardApproval>>;
  signIns: Readonly<Record<number, CardSignIn>>;
  schedEdits: Readonly<Record<number, SchedEdit>>;
  /** The bot whose ··· settings are open, with its reports and schedules (null: loading). */
  settingsFor: string | null;
  reports: ReportInfo[] | null;
  schedules: ScheduleInfo[] | null;
  /** Bumped on `schedules_changed`: the open settings ask again. */
  schedulesChanged: number;
  /** Per bot: the owner's thumbs (by turn_done seq) and turns kept as test cases. */
  rated: Readonly<Record<string, { ratings: Record<number, boolean>; cases: number[] }>>;
  /** The "Keep as a test case" sheet; `n` counts drafts so each one opens it. */
  caseDraft: (CaseDraft & { n: number }) | null;
  cmp: CompareState;
  /** Free questions kept for live bots, in percent (null until sent). */
  reservePct: number | null;
}

const initialCmp: CompareState = {
  bot: null,
  tab: "cmpScore",
  cases: [],
  runs: [],
  rows: [],
  batch: null,
  detail: null,
  estimate: "",
  canStart: false,
};

export const initialCards: CardsState = {
  seq: 0,
  approvals: {},
  signIns: {},
  schedEdits: {},
  settingsFor: null,
  reports: null,
  schedules: null,
  schedulesChanged: 0,
  rated: {},
  caseDraft: null,
  cmp: initialCmp,
  reservePct: null,
};

function without<T>(map: Readonly<Record<number, T>>, id: number): Record<number, T> {
  const next = { ...map };
  delete next[id];
  return next;
}

/** The estimate in words, as the Compare dialog shows it. */
export function estimateText(e: BatchEstimate): string {
  const parts = [`${e.runs} run${e.runs === 1 ? "" : "s"}`];
  if (e.openrouter_requests)
    parts.push(
      `≈ ${e.openrouter_requests} free questions${e.openrouter_left != null ? ` of ${e.openrouter_left} left today (${e.reserve} kept for live bots)` : ""}`,
    );
  parts.push(e.paid ? "paid models count against your daily limit" : "0 paid");
  let text = parts.join(" · ");
  if (e.refused.length) text += `. Not run: ${e.refused.join("; ")}.`;
  if (!e.fits && e.runs) text += " That doesn't fit what's left today: pick fewer cases or models.";
  return text;
}

function onServer(state: CardsState, action: Extract<Action, { kind: "server" }>): CardsState {
  const msg = action.msg;
  switch (msg.type) {
    case "hello":
      // Open cards are sent again after hello.
      return { ...state, approvals: {}, signIns: {} };
    case "approval_needed": {
      const { type: _t, ...a } = msg;
      const old = state.approvals[a.id];
      const seq = old ? state.seq : state.seq + 1;
      return { ...state, seq, approvals: { ...state.approvals, [a.id]: { ...a, at: old ? old.at : seq } } };
    }
    case "approval_resolved": {
      const approvals = without(state.approvals, msg.id);
      // Changes to a schedule card go with it.
      const schedEdits: Record<number, SchedEdit> = {};
      for (const [id, e] of Object.entries(state.schedEdits)) if (Number(id) in approvals) schedEdits[Number(id)] = e;
      return { ...state, approvals, schedEdits };
    }
    case "sign_in_needed": {
      const { type: _t, ...s } = msg;
      const old = state.signIns[s.id];
      const seq = old ? state.seq : state.seq + 1;
      return { ...state, seq, signIns: { ...state.signIns, [s.id]: { ...s, at: old ? old.at : seq } } };
    }
    case "sign_in_resolved":
      return { ...state, signIns: without(state.signIns, msg.id) };
    case "reports":
      return msg.bot === state.settingsFor ? { ...state, reports: msg.reports } : state;
    case "schedules":
      return msg.bot && msg.bot === state.settingsFor ? { ...state, schedules: msg.schedules } : state;
    case "schedules_changed":
      return { ...state, schedulesChanged: state.schedulesChanged + 1 };
    case "ratings":
      return { ...state, rated: { ...state.rated, [msg.bot]: { ratings: { ...msg.ratings } as Record<number, boolean>, cases: msg.cases } } };
    case "case_draft": {
      const { type: _t, ...d } = msg;
      return { ...state, caseDraft: { ...d, n: (state.caseDraft?.n ?? 0) + 1 } };
    }
    case "test_cases":
      return msg.bot === state.cmp.bot ? { ...state, cmp: { ...state.cmp, cases: msg.cases } } : state;
    case "batch_estimate": {
      if (msg.bot !== state.cmp.bot) return state;
      const { type: _t, ...e } = msg;
      return { ...state, cmp: { ...state.cmp, estimate: estimateText(e), canStart: e.fits } };
    }
    case "batch_progress": {
      if (msg.bot !== state.cmp.bot) return state;
      const { type: _t, ...p } = msg;
      return { ...state, cmp: { ...state.cmp, batch: p } };
    }
    case "trial_run": {
      const { type: _t, ...run } = msg;
      let cmp = state.cmp;
      if (run.bot === cmp.bot) {
        const i = cmp.runs.findIndex((r) => r.id === run.id);
        const runs = i >= 0 ? cmp.runs.map((r, k) => (k === i ? run : r)) : [run, ...cmp.runs];
        cmp = { ...cmp, runs };
      }
      if (cmp.detail && cmp.detail.run.id === run.id) cmp = { ...cmp, detail: { ...cmp.detail, run } };
      return cmp === state.cmp ? state : { ...state, cmp };
    }
    case "trial_event": {
      const d = state.cmp.detail;
      if (!d || d.run.id !== msg.run) return state;
      return { ...state, cmp: { ...state.cmp, detail: { ...d, events: [...d.events, msg.event] } } };
    }
    case "runs":
      return msg.bot === state.cmp.bot ? { ...state, cmp: { ...state.cmp, runs: msg.runs } } : state;
    case "run_detail":
      return {
        ...state,
        cmp: {
          ...state.cmp,
          detail: { run: msg.run, events: msg.events, original: msg.original, original_events: msg.original_events },
        },
      };
    case "scoreboard":
      return msg.bot === state.cmp.bot ? { ...state, cmp: { ...state.cmp, rows: msg.rows } } : state;
    case "compare_prefs":
      return { ...state, reservePct: msg.reserve_pct };
    default:
      return state;
  }
}

export function cardsReducer(state: CardsState, action: Action): CardsState {
  if (action.kind === "server") return onServer(state, action);
  if (action.kind !== "local") return state;
  switch (action.type) {
    case "ui/open": {
      // Compare opens on a bot ("bot" or "bot:tab"), fresh each time.
      if (action.dialog !== "compare") return state;
      const arg = String(action.arg ?? "");
      const at = arg.lastIndexOf(":");
      const tail = at >= 0 ? arg.slice(at + 1) : "";
      const isTab = tail === "cmpScore" || tail === "cmpCases" || tail === "cmpRuns";
      const bot = isTab ? arg.slice(0, at) : arg;
      return { ...state, cmp: { ...initialCmp, bot: bot || null, tab: isTab ? (tail as CmpTab) : "cmpScore" } };
    }
    case "cards/settingsFor":
      return { ...state, settingsFor: (action.bot as string | null) ?? null, reports: null, schedules: null };
    case "cards/schedEdit":
      return { ...state, schedEdits: { ...state.schedEdits, [action.id as number]: action.edit as SchedEdit } };
    case "cards/cmpTab":
      return { ...state, cmp: { ...state.cmp, tab: action.tab as CmpTab } };
    case "cards/cmpEstimate":
      // A changed pick: the old estimate no longer holds.
      return { ...state, cmp: { ...state.cmp, estimate: (action.text as string | undefined) ?? "", canStart: false } };
    case "cards/cmpStarted":
      return { ...state, cmp: { ...state.cmp, canStart: false, tab: "cmpRuns" } };
    case "cards/cmpBack":
      return { ...state, cmp: { ...state.cmp, detail: null } };
    default:
      return state;
  }
}

/** Sign-ins and approvals together, oldest first, so the newest sits right above the box. */
export type CardItem =
  | { key: string; kind: "sign_in"; m: CardSignIn }
  | { key: string; kind: "approval"; m: CardApproval };

export function cardList(state: CardsState): CardItem[] {
  const list: CardItem[] = [];
  for (const m of Object.values(state.signIns)) list.push({ key: `s:${m.id}`, kind: "sign_in", m });
  for (const m of Object.values(state.approvals)) list.push({ key: `a:${m.id}`, kind: "approval", m });
  return list.sort((x, y) => x.m.at - y.m.at);
}

export const SCHED_KINDS: [ScheduleRule["kind"], string][] = [
  ["once", "Once"],
  ["daily", "Every day"],
  ["weekdays", "Every weekday"],
  ["weekly", "Some days of the week"],
  ["monthly", "Once a month"],
];
export const DAYS: [Day, string][] = [
  ["mon", "Mon"],
  ["tue", "Tue"],
  ["wed", "Wed"],
  ["thu", "Thu"],
  ["fri", "Fri"],
  ["sat", "Sat"],
  ["sun", "Sun"],
];

/** A schedule card's changes so far, or its proposal as it came. */
export function editOf(state: CardsState, a: Approval): SchedEdit {
  const kept = state.schedEdits[a.id];
  if (kept) return kept;
  const r = a.schedule?.rule;
  if (!r) return { open: false, task: "", kind: "daily", time: "09:00", date: "", days: [], day: 1 };
  const at = r.kind === "once" ? r.at.split("T") : ["", ""];
  return {
    open: false,
    task: a.schedule?.task ?? "",
    kind: r.kind,
    time: ("time" in r ? r.time : "") || at[1] || "09:00",
    date: at[0] || "",
    days: r.kind === "weekly" ? [...r.days] : [],
    day: r.kind === "monthly" ? r.day : 1,
  };
}

/** The rule a changed schedule card sends. */
export function ruleOf(e: SchedEdit): ScheduleRule {
  switch (e.kind) {
    case "once":
      return { kind: "once", at: `${e.date}T${e.time}` };
    case "weekly":
      return { kind: "weekly", days: DAYS.map((d) => d[0]).filter((d) => e.days.includes(d)), time: e.time };
    case "monthly":
      return { kind: "monthly", day: Number(e.day) || 1, time: e.time };
    default:
      return { kind: e.kind, time: e.time };
  }
}
