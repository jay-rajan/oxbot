// Recently deleted: events deleted in the last 30 days, each with Bring
// back. Older ones are gone for good.
import { useApp, useSel } from "../../app/context";
import { Button, Dialog, DialogFrame, List, ListRow, Note } from "../../components/ui";
import { clock, longDay } from "./CalendarView";

export function DeletedDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { send } = useApp();
  const deleted = useSel((s) => s.calendar.deleted);
  const changed = useSel((s) => s.calendar.changed);
  return (
    <Dialog id="deletedDialog" open={open} onClose={onClose} size="md" labelledBy="deletedTitle">
      <DialogFrame titleId="deletedTitle" title="Recently deleted" onClose={onClose} key={changed}>
        {deleted.length ? (
          <List>
            {deleted.map((e) => (
              <ListRow
                key={e.id}
                title={e.event.title}
                sub={`${longDay(e.event.starts.slice(0, 10), { weekday: "short", month: "short", day: "numeric" })}${
                  e.all_day ? "" : ` · ${clock(e.event.starts.slice(11, 16))}`
                } · deleted by ${e.changed_by || "someone"}`}
                trail={
                  <Button
                    kind="secondary"
                    size="sm"
                    onClick={() => {
                      send({ type: "restore_event", id: e.id });
                      send({ type: "deleted_events" });
                    }}
                  >
                    Bring back
                  </Button>
                }
              />
            ))}
          </List>
        ) : (
          <Note>Nothing deleted in the last 30 days.</Note>
        )}
      </DialogFrame>
    </Dialog>
  );
}
