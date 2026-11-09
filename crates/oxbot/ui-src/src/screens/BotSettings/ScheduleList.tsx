// A list of schedules (a bot's, in its ··· settings): what, when, how the
// last run went, with Pause/Resume and Delete for those who may change it.
import { useState } from "react";
import { useApp } from "../../app/context";
import { Button, cx, List, ListRow } from "../../components/ui";
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
    <List id={id}>
      {!list ? (
        <li className="note-text">Loading…</li>
      ) : !list.length ? (
        <li className="note-text">None yet. Ask the bot, e.g. "every Saturday at 6 PM, check…"</li>
      ) : (
        list.map((sc) => (
          <ListRow
            className={cx("sched-row", sc.paused && "paused")}
            key={sc.id}
            title={sc.text}
            sub={scheduleMeta(sc, withBot)}
            trail={
              sc.can_change ? (
                <>
                  <Button kind="quiet" size="sm" onClick={() => send({ type: "pause_schedule", id: sc.id, paused: !sc.paused })}>
                    {sc.paused ? "Resume" : "Pause"}
                  </Button>
                  <Button
                    kind="quiet"
                    size="sm"
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
                  </Button>
                </>
              ) : null
            }
          />
        ))
      )}
    </List>
  );
}
