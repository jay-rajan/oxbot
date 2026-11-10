// Settings → Schedules: every schedule this person may see, in every bot
// (the owner: all of them), with its next run, how the last one went, and
// Pause/Resume and Delete where they may change it. The server decides who
// sees and changes what, as in a bot's own ··· settings. New schedules come
// only from asking a bot.
import { useEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { Section } from "../../components/ui";
import { ScheduleList } from "../BotSettings/ScheduleList";

export const NO_SCHEDULES = 'No schedules yet. Ask a bot, e.g. "every Saturday at 8 AM, plan the week".';

export function SchedulesTab({ hidden }: { hidden: boolean }) {
  const { send } = useApp();
  const list = useSel((s) => s.cards.allSchedules);
  const changed = useSel((s) => s.cards.schedulesChanged);

  // Asked for when the tab shows, and again when schedules change meanwhile.
  const asked = useRef(-1);
  useEffect(() => {
    if (hidden || asked.current === changed) return;
    asked.current = changed;
    send({ type: "list_schedules", bot: null });
  }, [hidden, changed, send]);
  useEffect(() => {
    if (hidden) asked.current = -1;
  }, [hidden]);

  return (
    <div id="setSchedules" className="settings-pane" role="tabpanel" hidden={hidden}>
      <Section>
        <ScheduleList id="allScheds" list={list} withBot empty={{ id: "noScheds", text: NO_SCHEDULES }} />
      </Section>
    </div>
  );
}
