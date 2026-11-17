// Records: the notes as the server last sent them, and a count bumped
// when the Home changed (the Notes screen then asks again). Also the search
// the screen and its tests share.
import type { HomeNoteInfo } from "../net/protocol";
import type { Action } from "./store";

export interface NotesState {
  notes: HomeNoteInfo[];
  /** Whether a list came yet. */
  loaded: boolean;
  /** Bumped when the server says the Home changed (ask again). */
  changed: number;
}

export const initialNotes: NotesState = { notes: [], loaded: false, changed: 0 };

export function notesReducer(state: NotesState, action: Action): NotesState {
  if (action.kind !== "server") return state;
  const msg = action.msg;
  switch (msg.type) {
    case "home_notes":
      return { ...state, notes: msg.notes, loaded: true };
    case "home_changed":
      return { ...state, changed: state.changed + 1 };
    default:
      return state;
  }
}

/** A note's topic as its folder shows it. */
export const topicOf = (n: HomeNoteInfo) => n.topic || "Unsorted";

/** The topics in use, A-Z (Unsorted last). */
export function topics(notes: readonly HomeNoteInfo[]): string[] {
  const all = [...new Set(notes.map((n) => n.topic).filter(Boolean))].sort((a, b) => a.localeCompare(b));
  return notes.some((n) => !n.topic) ? [...all, "Unsorted"] : all;
}

const MONTHS = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

/** The notes that have every word of `query` somewhere (title, topic,
 *  tags, people, summary, file name, year or month), latest day first. */
export function search(notes: readonly HomeNoteInfo[], query: string, topic = ""): HomeNoteInfo[] {
  const words = query.toLowerCase().split(/[^\p{L}\p{N}-]+/u).filter(Boolean).map((w) => (w.length > 3 ? w.replace(/'?s$/, "") : w));
  return notes
    .filter((n) => !topic || topicOf(n) === topic)
    .filter((n) => {
      if (!words.length) return true;
      const hay = [n.title, n.topic, ...n.tags, ...n.people, n.summary, n.name, n.day, MONTHS[Number(n.day.slice(5, 7)) - 1] ?? ""].join(" ").toLowerCase();
      return words.every((w) => hay.includes(w));
    })
    .sort((a, b) => b.day.localeCompare(a.day) || b.id - a.id);
}

/** Notes by year, latest first, then by topic: [year, [topic, notes][]][]. */
export function byYearTopic(notes: readonly HomeNoteInfo[]): [string, [string, HomeNoteInfo[]][]][] {
  const years = new Map<string, Map<string, HomeNoteInfo[]>>();
  for (const n of notes) {
    const y = n.day.slice(0, 4);
    const t = topicOf(n);
    const topicsOfYear = years.get(y) ?? new Map<string, HomeNoteInfo[]>();
    topicsOfYear.set(t, [...(topicsOfYear.get(t) ?? []), n]);
    years.set(y, topicsOfYear);
  }
  return [...years.entries()]
    .sort(([a], [b]) => b.localeCompare(a))
    .map(([y, ts]) => [y, [...ts.entries()].sort(([a], [b]) => (a === "Unsorted" ? 1 : b === "Unsorted" ? -1 : a.localeCompare(b)))]);
}

/** Where a note's day came from, in words. */
export function dayFrom(n: HomeNoteInfo): string {
  return n.day_from === "you" ? "date given" : n.day_from === "document" ? "date from the document" : "uploaded that day";
}
