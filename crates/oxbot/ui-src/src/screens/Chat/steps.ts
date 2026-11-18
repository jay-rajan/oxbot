// A bot's tool steps, folded so the chat says where it is ("On walmart.ca ·
// browsing") instead of listing each click, fill and key press.
import type { BotEvent } from "../../net/protocol";

/** One run of steps between messages, drawn as a single line. */
export type StepRun = { site: string | null; browsed: boolean };

export type Folded = {
  /** Each run's line, keyed by the seq of its first step. */
  runs: Map<number, StepRun>;
  /** The run the chat ends on, if it ends on steps. */
  tail: StepRun | null;
  tailSeq: number | null;
  /** The site the bot was last on. */
  site: string | null;
};

/** The site in a step's summary, e.g. "walmart.ca" from a page it opened. */
export function siteOf(summary: string): string | null {
  const host = /https?:\/\/([^/\s?#:]+)/.exec(summary)?.[1];
  return host ? host.replace(/^www\./, "") : null;
}

/** Whether a step's label is the bot using a web page. */
const isBrowsing = (name: string) => /^(browser|page|several steps)\b|chrome-devtools|camoufox/.test(name);

export function foldSteps(events: BotEvent[]): Folded {
  const runs = new Map<number, StepRun>();
  // The bot stays on a site until it opens another, even across turns.
  let site: string | null = null;
  let run: StepRun | null = null;
  let runSeq: number | null = null;
  for (const e of events) {
    if (e.kind !== "tool_use") {
      run = null;
      runSeq = null;
      continue;
    }
    site = siteOf(e.summary) ?? site;
    if (!run) {
      run = { site, browsed: false };
      runSeq = e.seq;
      runs.set(e.seq, run);
    }
    if (isBrowsing(e.name)) run.browsed = true;
    if (run.browsed) run.site = site;
  }
  return { runs, tail: run, tailSeq: runSeq, site };
}

/** A finished run's line, or null for steps not worth a line. */
export function doneLine(r: StepRun): string | null {
  if (!r.browsed) return null;
  return r.site ? `Browsed ${r.site}` : "Browsed the web";
}

export const browsingLine = (site: string | null) => (site ? `On ${site} · browsing` : "Browsing");

// Shown as they are; anything else would name a step.
const PLAIN = new Set(["Working…", "Thinking…", "Writing…"]);
const ASIDE = new Set(["Waiting to retry…", "Asking you to sign in…", "Writing a report…"]);

/** The status line under the chat while the bot works, without naming steps. */
export function statusLine(activity: string | null, tail: StepRun | null, site: string | null): string {
  const a = activity || "Working…";
  if (ASIDE.has(a)) return a;
  if (tail?.browsed || /^Using browser|page/i.test(a)) return browsingLine(tail?.site ?? site);
  return PLAIN.has(a) ? a : "Working…";
}
