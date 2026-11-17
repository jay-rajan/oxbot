// The calendar area: the Home's events as the server last sent them, the
// day being looked at, and the last change this page made (for Undo).
// Also the date work the views share: days are "2026-11-16" strings,
// counted in whole days (UTC), so a clock change never moves an event.
import type { HomeEventInfo } from "../net/protocol";
import type { Action } from "./store";

export type CalView = "day" | "week" | "month" | "list";

export interface CalendarState {
  view: CalView;
  /** The day being looked at ("2026-11-16"). Empty until the view opens. */
  day: string;
  /** The last answer: its range and events. */
  from: string;
  to: string;
  events: HomeEventInfo[];
  /** Bumped when the server says the Home changed (ask again). */
  changed: number;
  deleted: HomeEventInfo[];
  /** This page's last change, for its Undo line. */
  /** The last change made from this page, or by a bot (`bot`, its id), for Undo. */
  saved: { change: number; id: number; action: string; title: string; bot?: string } | null;
}

export const initialCalendar: CalendarState = {
  view: "week",
  day: "",
  from: "",
  to: "",
  events: [],
  changed: 0,
  deleted: [],
  saved: null,
};

export function calendarReducer(state: CalendarState, action: Action): CalendarState {
  if (action.kind === "server") {
    const msg = action.msg;
    switch (msg.type) {
      case "home_events":
        return { ...state, from: msg.from, to: msg.to, events: msg.events };
      case "deleted_events":
        return { ...state, deleted: msg.events };
      case "home_changed":
        return { ...state, changed: state.changed + 1 };
      case "home_saved":
        return {
          ...state,
          saved: { change: msg.change, id: msg.id, action: msg.action, title: msg.title, ...(msg.bot ? { bot: msg.bot } : {}) },
        };
      default:
        return state;
    }
  }
  if (action.kind !== "local") return state;
  switch (action.type) {
    case "calendar/view":
      return { ...state, view: action.view as CalView };
    case "calendar/day":
      return { ...state, day: action.day as string };
    case "calendar/clearSaved":
      return { ...state, saved: null };
    default:
      return state;
  }
}

// ---------- days

const DAY_MS = 86_400_000;

/** A day's number (days since 1970-01-01). */
export function dayNum(day: string): number {
  const [y, m, d] = day.split("-").map(Number);
  return Math.round(Date.UTC(y ?? 1970, (m ?? 1) - 1, d ?? 1) / DAY_MS);
}

/** The day `n` days from 1970-01-01. */
export function dayOf(n: number): string {
  return new Date(n * DAY_MS).toISOString().slice(0, 10);
}

export const addDays = (day: string, n: number) => dayOf(dayNum(day) + n);

/** Monday 0 … Sunday 6. */
export const weekday = (day: string) => (dayNum(day) + 3) % 7;

/** The Monday of `day`'s week. */
export const weekStart = (day: string) => addDays(day, -weekday(day));

/** `day` in `months` months, on the same day of the month or the month's last. */
export function addMonths(day: string, months: number): string {
  const [y, m, d] = day.split("-").map(Number);
  const total = (y ?? 1970) * 12 + ((m ?? 1) - 1) + months;
  const ny = Math.floor(total / 12);
  const nm = (total % 12) + 1;
  const last = new Date(Date.UTC(ny, nm, 0)).getUTCDate();
  return `${String(ny).padStart(4, "0")}-${String(nm).padStart(2, "0")}-${String(Math.min(d ?? 1, last)).padStart(2, "0")}`;
}

/** The days a view shows around `day`: [first, last]. */
export function rangeOf(view: CalView, day: string): [string, string] {
  switch (view) {
    case "day":
      return [day, day];
    case "week": {
      const mon = weekStart(day);
      return [mon, addDays(mon, 6)];
    }
    case "month": {
      // Whole weeks around the month, Monday first.
      const first = `${day.slice(0, 8)}01`;
      const last = addDays(addMonths(first, 1), -1);
      return [weekStart(first), addDays(weekStart(last), 6)];
    }
    case "list":
      return [day, addDays(day, 41)];
  }
}

/** The next or last stretch from `day`, as `view` moves. */
export function step(view: CalView, day: string, by: 1 | -1): string {
  switch (view) {
    case "day":
      return addDays(day, by);
    case "week":
      return addDays(day, 7 * by);
    case "month":
      return addMonths(`${day.slice(0, 8)}01`, by);
    case "list":
      return addDays(day, 42 * by);
  }
}

// ---------- where events fall

/** One time an event happens, on one day it covers. */
export interface Occurrence {
  ev: HomeEventInfo;
  /** The day it's shown on. */
  day: string;
  /** The day this time of it starts (for a repeat, that time's day). */
  start: string;
  /** "17:30" (a later day of a timed event that goes past midnight:
   *  "00:00"), or null for all day. */
  time: string | null;
  /** When it ends that day: "18:30", "24:00" if it goes on past
   *  midnight; null if not said. */
  endTime: string | null;
}

/** The days a repeat starts on, from `base` up to `to`, from `from - span` on. */
function starts(ev: HomeEventInfo, base: string, span: number, from: string, to: string): string[] {
  const rep = ev.event.repeat;
  const until = rep?.until ? dayNum(rep.until) : Number.POSITIVE_INFINITY;
  const lo = dayNum(from) - span;
  const hi = Math.min(dayNum(to), until);
  const b = dayNum(base);
  if (!rep) return b <= dayNum(to) && b >= lo ? [base] : [];
  const out: string[] = [];
  if (rep.every === "day" || rep.every === "week") {
    const gap = rep.every === "day" ? 1 : 7;
    const skip = Math.max(0, Math.ceil((lo - b) / gap));
    for (let n = b + skip * gap; n <= hi; n += gap) out.push(dayOf(n));
    return out;
  }
  const months = rep.every === "month" ? 1 : 12;
  for (let k = 0; k < 5000; k++) {
    const d = addMonths(base, k * months);
    // A day the month doesn't have (the 31st) is skipped, not moved.
    if (d.slice(8) !== base.slice(8)) continue;
    const n = dayNum(d);
    if (n > hi) break;
    if (n >= lo) out.push(d);
  }
  return out;
}

/** Every time the events happen from `from` to `to`, by day, earliest first
 *  (all-day ones first in a day). */
export function occurrences(events: readonly HomeEventInfo[], from: string, to: string): Map<string, Occurrence[]> {
  const byDay = new Map<string, Occurrence[]>();
  const f = dayNum(from);
  const t = dayNum(to);
  for (const ev of events) {
    const base = ev.event.starts.slice(0, 10);
    const timed = !ev.all_day;
    let end = ev.event.ends ? ev.event.ends.slice(0, 10) : base;
    let endHM = timed && ev.event.ends ? ev.event.ends.slice(11, 16) : null;
    // Ending at midnight is the end of the day before (7 PM till 12 AM is
    // one evening, not two days).
    if (timed && endHM === "00:00" && end > base) {
      end = dayOf(dayNum(end) - 1);
      endHM = "24:00";
    }
    const span = Math.max(0, dayNum(end) - dayNum(base));
    const startHM = ev.event.starts.slice(11, 16);
    // A day or longer (a trip): in each day's all-day row. Shorter, even
    // past midnight (a late party): at its hours on each day it touches.
    const long = span > 1 || (span === 1 && (endHM ?? "00:00") >= startHM);
    const hours = timed && !long;
    const time = hours ? startHM : null;
    for (const start of starts(ev, base, span, from, to)) {
      const s = dayNum(start);
      for (let n = Math.max(s, f); n <= Math.min(s + span, t); n++) {
        const day = dayOf(n);
        const list = byDay.get(day) ?? [];
        // A timed event over several days: from its start till midnight,
        // whole days, then from midnight till its end.
        const first = n === s;
        const last = n === s + span;
        list.push({
          ev,
          day,
          start,
          time: hours ? (first ? time : "00:00") : null,
          endTime: hours ? (last ? endHM : "24:00") : null,
        });
        byDay.set(day, list);
      }
    }
  }
  for (const list of byDay.values())
    list.sort((a, b) => (a.time ?? "").localeCompare(b.time ?? "") || a.ev.id - b.ev.id);
  return byDay;
}

/** A timed event placed on a day's time grid: minutes from midnight, and
 *  its lane among the events it overlaps. */
export interface Placed {
  o: Occurrence;
  top: number;
  /** How long it's drawn: its length, at least half an hour. */
  height: number;
  lane: number;
  lanes: number;
}

const minutes = (hm: string) => {
  const [h, m] = hm.split(":").map(Number);
  return (h ?? 0) * 60 + (m ?? 0);
};

/** One day's timed events on its time grid. An event without an end is an
 *  hour long. Events that overlap sit side by side, each group sharing its
 *  width evenly. All-day ones are left out. */
export function placeDay(list: readonly Occurrence[]): Placed[] {
  const timed = list
    .filter((o) => o.time)
    .map((o) => {
      const top = minutes(o.time ?? "00:00");
      const end = o.endTime ? minutes(o.endTime) : top + 60;
      return { o, top, end: Math.min(24 * 60, end > top ? end : top + 60) };
    })
    .sort((a, b) => a.top - b.top || b.end - a.end);
  const out: Placed[] = [];
  let group: Placed[] = [];
  let laneEnds: number[] = [];
  let groupEnd = -1;
  const close = () => {
    for (const p of group) p.lanes = laneEnds.length;
    group = [];
    laneEnds = [];
  };
  for (const t of timed) {
    if (t.top >= groupEnd) close();
    let lane = laneEnds.findIndex((e) => e <= t.top);
    if (lane < 0) lane = laneEnds.push(0) - 1;
    laneEnds[lane] = t.end;
    groupEnd = Math.max(groupEnd, t.end);
    const p = { o: t.o, top: t.top, height: Math.max(30, t.end - t.top), lane, lanes: 1 };
    group.push(p);
    out.push(p);
  }
  close();
  return out;
}
