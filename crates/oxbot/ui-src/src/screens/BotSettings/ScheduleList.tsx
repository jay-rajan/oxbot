// A list of schedules (a bot's, in its ··· settings, or every one the
// viewer may see, in Settings → Schedules): what, when, how the last run
// went, with Pause/Resume and Delete for those who may change it.
import { useState } from "react";
import { useApp } from "../../app/context";
import { Button, cx, List, ListRow, Note, Pill } from "../../components/ui";
import type { ScheduleInfo } from "../../net/protocol";
import { lastResult, scheduleMeta, schedWhen } from "./schedules";

/** The last run: a pill (ran, late, missed, failed…), when, and why if it didn't run. */
function LastRun({ sc }: { sc: ScheduleInfo }) {
  const last = lastResult(sc);
  if (!last || !sc.last_run_ms) return null;
  return (
    <>
      {" · last "}
      <Pill tone={last.tone} className="sched-last">
        {last.word}
      </Pill>{" "}
      {schedWhen(sc.last_run_ms, sc.tz)}
      {last.why ? `: ${last.why}` : ""}
    </>
  );
}

export function ScheduleList({
  id,
  list,
  withBot,
  empty,
}: {
  id: string;
  list: ScheduleInfo[] | null;
  withBot: boolean;
  /** With none: this line instead of the list. */
  empty?: { id: string; text: string };
}) {
  const { send } = useApp();
  const [armed, setArmed] = useState<number | null>(null);
  if (empty && list && !list.length) return <Note id={empty.id}>{empty.text}</Note>;
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
            data-id={sc.id}
            sub={
              <>
                {scheduleMeta(sc, withBot)}
                <LastRun sc={sc} />
              </>
            }
            trail={
              sc.can_change ? (
                <>
                  <Button kind="quiet" size="sm" className="sched-pause" onClick={() => send({ type: "pause_schedule", id: sc.id, paused: !sc.paused })}>
                    {sc.paused ? "Resume" : "Pause"}
                  </Button>
                  <Button
                    kind="quiet"
                    size="sm"
                    className="sched-delete"
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
