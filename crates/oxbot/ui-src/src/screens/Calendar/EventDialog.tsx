// Adding or changing an event: what, when (or all day), where, who, how
// it repeats, a reminder, notes. Changing a repeating event changes every
// time it happens. Delete puts it in Recently deleted.
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Checkbox, Dialog, DialogForm, Field, Input, Select, TextArea } from "../../components/ui";
import type { EventInput, HomeEventInfo } from "../../net/protocol";
import { addDays } from "../../state/calendar";
import { longDay } from "./CalendarView";

export interface Draft {
  /** The day it's on (a new event) or starts on. */
  day: string;
  /** The event being changed; none for a new one. */
  ev?: HomeEventInfo;
}

const REPEATS = [
  ["", "Doesn't repeat"],
  ["day", "Every day"],
  ["week", "Every week"],
  ["month", "Every month"],
  ["year", "Every year"],
] as const;

const REMINDERS = [
  ["", "No reminder"],
  ["0", "When it starts"],
  ["10", "10 minutes before"],
  ["30", "30 minutes before"],
  ["60", "1 hour before"],
  ["1440", "1 day before"],
] as const;

export function EventDialog({ draft, onClose }: { draft: Draft | null; onClose: () => void }) {
  return (
    <Dialog id="eventDialog" open={!!draft} onClose={onClose} size="md" labelledBy="eventTitle">
      {draft ? <EventForm draft={draft} onClose={onClose} /> : null}
    </Dialog>
  );
}

function EventForm({ draft, onClose }: { draft: Draft; onClose: () => void }) {
  const { send } = useApp();
  const people = useSel((s) => s.home.people);
  const e = draft.ev?.event;
  const [title, setTitle] = useState(e?.title ?? "");
  const [allDay, setAllDay] = useState(draft.ev ? draft.ev.all_day : false);
  const [date, setDate] = useState(e ? e.starts.slice(0, 10) : draft.day);
  const [start, setStart] = useState(e && !draft.ev?.all_day ? e.starts.slice(11, 16) : "09:00");
  const [endDate, setEndDate] = useState(e?.ends ? e.ends.slice(0, 10) : "");
  const [end, setEnd] = useState(e?.ends && !draft.ev?.all_day ? e.ends.slice(11, 16) : "");
  const [place, setPlace] = useState(e?.place ?? "");
  const [who, setWho] = useState<string[]>(e?.people ?? []);
  const [repeat, setRepeat] = useState<string>(e?.repeat?.every ?? "");
  const [until, setUntil] = useState(e?.repeat?.until ?? "");
  const [remind, setRemind] = useState(e?.remind_min != null ? String(e.remind_min) : "");
  const [notes, setNotes] = useState(e?.notes ?? "");
  const [error, setError] = useState("");
  // The dialog focuses its first button (Close); the title is what's typed first.
  const nameRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (draft.ev) return;
    // After the dialog opens (its own effect runs after this one).
    const t = setTimeout(() => nameRef.current?.focus());
    return () => clearTimeout(t);
  }, [draft.ev]);

  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    if (!title.trim()) return setError("Give it a title.");
    if (!date) return setError("Pick a day.");
    let ends: string | null = null;
    if (allDay) {
      if (endDate && endDate > date) ends = endDate;
    } else if (end) {
      // An end earlier than the start is the next day (a late party).
      ends = end > start ? `${date}T${end}` : `${addDays(date, 1)}T${end}`;
    }
    const event: EventInput = {
      title: title.trim(),
      starts: allDay ? date : `${date}T${start || "09:00"}`,
      ends,
      place: place.trim(),
      people: who,
      notes: notes.trim(),
      repeat: repeat ? { every: repeat, until: until || null } : null,
      remind_min: remind === "" ? null : Number(remind),
    };
    send(draft.ev ? { type: "change_event", id: draft.ev.id, event } : { type: "add_event", event });
    onClose();
  };

  const toggle = (name: string) => setWho((w) => (w.includes(name) ? w.filter((n) => n !== name) : [...w, name]));
  const by = draft.ev
    ? draft.ev.made_by === draft.ev.changed_by
      ? `Added by ${draft.ev.made_by || "someone"}`
      : `Added by ${draft.ev.made_by || "someone"} · changed by ${draft.ev.changed_by || "someone"}`
    : "";

  return (
    <DialogForm
      titleId="eventTitle"
      title={draft.ev ? "Event" : `New event · ${longDay(date || draft.day, { weekday: "short", month: "short", day: "numeric" })}`}
      onClose={onClose}
      onSubmit={submit}
      className="event-form"
      footer={
        <>
          {draft.ev ? (
            <Button
              kind="danger"
              className="event-delete"
              onClick={() => {
                if (draft.ev) send({ type: "delete_event", id: draft.ev.id });
                onClose();
              }}
            >
              Delete
            </Button>
          ) : null}
          <Button kind="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button kind="primary" type="submit" id="eventSave">
            Save
          </Button>
        </>
      }
    >
      <Field label="What" error={error || undefined}>
        <Input
          id="eventName"
          ref={nameRef}
          value={title}
          maxLength={200}
          placeholder="Soccer practice"
          onChange={(x) => {
            setTitle(x.target.value);
            setError("");
          }}
        />
      </Field>
      <Checkbox checked={allDay} onChange={(x) => setAllDay(x.target.checked)}>
        All day
      </Checkbox>
      <div className="event-when">
        <Field label={allDay ? "From" : "Day"}>
          <Input type="date" value={date} onChange={(x) => setDate(x.target.value)} required />
        </Field>
        {allDay ? (
          <Field label="Until" hint="Leave empty for one day.">
            <Input type="date" value={endDate} min={date} onChange={(x) => setEndDate(x.target.value)} />
          </Field>
        ) : (
          <>
            <Field label="Starts">
              <Input type="time" value={start} onChange={(x) => setStart(x.target.value)} required />
            </Field>
            <Field label="Ends">
              <Input type="time" value={end} onChange={(x) => setEnd(x.target.value)} />
            </Field>
          </>
        )}
      </div>
      {people.length ? (
        <fieldset className="event-who">
          <legend className="field-name">Who it's for</legend>
          {people.map((p) => (
            <Checkbox key={p.name} checked={who.includes(p.name)} onChange={() => toggle(p.name)}>
              {p.name}
            </Checkbox>
          ))}
        </fieldset>
      ) : null}
      <Field label="Where">
        <Input value={place} maxLength={200} placeholder="Riverside Park" onChange={(x) => setPlace(x.target.value)} />
      </Field>
      <div className="event-when">
        <Field label="Repeats" hint={draft.ev?.event.repeat ? "Changes every time it repeats." : undefined}>
          <Select value={repeat} onChange={(x) => setRepeat(x.target.value)}>
            {REPEATS.map(([v, t]) => (
              <option key={v} value={v}>
                {t}
              </option>
            ))}
          </Select>
        </Field>
        {repeat ? (
          <Field label="Last time" hint="Leave empty to keep repeating.">
            <Input type="date" value={until} min={date} onChange={(x) => setUntil(x.target.value)} />
          </Field>
        ) : null}
        <Field label="Reminder" hint={remind ? "On each person's phone." : undefined}>
          <Select value={remind} onChange={(x) => setRemind(x.target.value)}>
            {REMINDERS.map(([v, t]) => (
              <option key={v} value={v}>
                {t}
              </option>
            ))}
          </Select>
        </Field>
      </div>
      <Field label="Notes" hint={by || undefined}>
        <TextArea value={notes} rows={3} maxLength={4000} onChange={(x) => setNotes(x.target.value)} />
      </Field>
    </DialogForm>
  );
}
