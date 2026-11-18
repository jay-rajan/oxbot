// Schedules in words for a list: where, next run, and how the last run
// went (ran, late, missed, failed, skipped).
import type { ScheduleInfo } from "../../net/protocol";

/** A run that didn't happen: why, after its pill. */
const SKIPPED: Record<string, string> = {
  busy: "the bot was busy",
  limit: "an AI limit was reached",
  too_soon: "too soon after the last run",
  no_access: "no longer allowed to use this bot",
};

type Tone = "accent" | "ok" | "warn" | "danger";

/** How the last run went, in a word, with its pill's tone. */
export function lastResult(sc: ScheduleInfo): { word: string; tone: Tone; why?: string } | null {
  const outcome = sc.last_outcome;
  if (!outcome) return null;
  switch (outcome) {
    case "ok":
      return sc.last_late ? { word: "late", tone: "warn", why: "ran when the home computer was back" } : { word: "ran", tone: "ok" };
    case "running":
      return { word: sc.last_late ? "running, late" : "running", tone: "accent" };
    case "failed":
      return { word: "failed", tone: "danger" };
    case "missed":
      return { word: "missed", tone: "danger", why: "the home computer was off or asleep" };
    default:
      return { word: "skipped", tone: "warn", why: SKIPPED[outcome] ?? outcome };
  }
}

export function schedWhen(ms: number, tz: string): string {
  const opts: Intl.DateTimeFormatOptions = { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" };
  try {
    return new Date(ms).toLocaleString([], { ...opts, timeZone: tz });
  } catch {
    return new Date(ms).toLocaleString([], opts);
  }
}

/** When a schedule runs, in words, without its task: "Every day at 8 AM". */
export function schedRule(sc: ScheduleInfo): string {
  const tail = `: ${sc.task}`;
  if (sc.text.endsWith(tail)) return sc.text.slice(0, -tail.length);
  const cut = sc.text.indexOf(": ");
  return cut > 0 ? sc.text.slice(0, cut) : sc.text;
}

/** Settings → Schedules' short line: when it runs, and its next run. */
export function schedShort(sc: ScheduleInfo): string {
  const next = sc.paused ? "paused" : sc.next_run_ms ? `next ${schedWhen(sc.next_run_ms, sc.tz)}` : "done: it ran once";
  return [schedRule(sc), !sc.mine && sc.person ? `as ${sc.person}` : "", next].filter(Boolean).join(" · ");
}

/** The line under a schedule: where (`withBot`), as whom, and its next run. */
export function scheduleMeta(sc: ScheduleInfo, withBot: boolean): string {
  const parts: string[] = [];
  if (withBot) parts.push(sc.bot_name);
  if (!sc.mine && sc.person) parts.push(`as ${sc.person}`);
  parts.push(sc.paused ? "paused" : sc.next_run_ms ? `next ${schedWhen(sc.next_run_ms, sc.tz)}` : "done: it ran once");
  return parts.join(" · ");
}
