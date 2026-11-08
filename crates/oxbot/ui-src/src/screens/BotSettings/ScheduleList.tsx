// A list of schedules (a bot's, in its ··· settings): what, when, how the
// last run went, with Pause/Resume and Delete for those who may change it.
import { useState } from "react";
import { useApp } from "../../app/context";
import type { ScheduleInfo } from "../../net/protocol";

const OUTCOMES: Record<string, string> = {
  ok: "done",
  failed: "didn't finish",
  running: "running now",
  busy: "skipped: the bot was busy",
  limit: "skipped: an AI limit was reached",
  missed: "missed: the home computer was off or asleep",
  too_soon: "skipped: too soon after the last run",
  no_access: "paused: no longer allowed to use this bot",
};

function schedWhen(ms: number, tz: string): string {
  const opts: Intl.DateTimeFormatOptions = { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" };
  try {
    return new Date(ms).toLocaleString([], { ...opts, timeZone: tz });
  } catch {
    return new Date(ms).toLocaleString([], opts);
  }
}

/** The line under a schedule: where, as whom, next and last run. */
export function scheduleMeta(sc: ScheduleInfo, withBot: boolean): string {
  const parts: string[] = [];
  if (withBot) parts.push(`in ${sc.bot_name}`);
  if (!sc.mine && sc.person) parts.push(`as ${sc.person}`);
  parts.push(sc.paused ? "paused" : sc.next_run_ms ? `next ${schedWhen(sc.next_run_ms, sc.tz)}` : "done: it ran once");
  if (sc.last_run_ms)
    parts.push(
      `last ${schedWhen(sc.last_run_ms, sc.tz)}${sc.last_outcome ? `, ${OUTCOMES[sc.last_outcome] || sc.last_outcome}` : ""}`,
    );
  return parts.join(" · ");
}

export function ScheduleList({ id, list, withBot }: { id: string; list: ScheduleInfo[] | null; withBot: boolean }) {
  const { send } = useApp();
  const [armed, setArmed] = useState<number | null>(null);
  return (
    <ul className="rules" id={id}>
      {!list ? (
        <li className="hint">Loading…</li>
      ) : !list.length ? (
        <li className="hint">None. Ask the bot for one ("every Saturday at 6 PM, check…") and press Create on its schedule card.</li>
      ) : (
        list.map((sc) => (
          <li className={`sched-row${sc.paused ? " paused" : ""}`} key={sc.id}>
            <span className="name">
              <span>{sc.text}</span>
              <small>{scheduleMeta(sc, withBot)}</small>
            </span>
            {sc.can_change ? (
              <div className="btns">
                <button type="button" className="ghost" onClick={() => send({ type: "pause_schedule", id: sc.id, paused: !sc.paused })}>
                  {sc.paused ? "Resume" : "Pause"}
                </button>
                <button
                  type="button"
                  className="ghost"
                  onClick={() => {
                    if (armed !== sc.id) {
                      setArmed(sc.id);
                      return;
                    }
                    setArmed(null);
                    send({ type: "delete_schedule", id: sc.id });
                  }}
                >
                  {armed === sc.id ? "Yes, delete" : "Delete"}
                </button>
              </div>
            ) : null}
          </li>
        ))
      )}
    </ul>
  );
}
