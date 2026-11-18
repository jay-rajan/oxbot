// A dish on the plan: its day, meal, name, notes and who it's for; or a new
// one, its day and meal picked where it was added. Remove takes it off.
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Checkbox, Dialog, DialogForm, Field, Input, Select, TextArea } from "../../components/ui";
import type { MealInfo, MealInput } from "../../net/protocol";
import { SLOTS } from "../../state/meals";
import { longDay } from "../Calendar/CalendarView";

export interface MealDraft {
  day: string;
  slot: string;
  /** The dish being changed; none for a new one. */
  meal?: MealInfo;
}

export const SLOT_NAME: Record<string, string> = { breakfast: "Breakfast", lunch: "Lunch", dinner: "Dinner" };

export function MealDialog({ draft, onClose }: { draft: MealDraft | null; onClose: () => void }) {
  return (
    <Dialog id="mealDialog" open={!!draft} onClose={onClose} size="md" labelledBy="mealTitle">
      {draft ? <MealForm draft={draft} onClose={onClose} /> : null}
    </Dialog>
  );
}

function MealForm({ draft, onClose }: { draft: MealDraft; onClose: () => void }) {
  const { send } = useApp();
  const people = useSel((s) => s.home.people);
  const m = draft.meal?.meal;
  const [title, setTitle] = useState(m?.title ?? "");
  const [day, setDay] = useState(m?.day ?? draft.day);
  const [slot, setSlot] = useState(m?.slot ?? draft.slot);
  const [notes, setNotes] = useState(m?.notes ?? "");
  const [who, setWho] = useState<string[]>(m?.people ?? []);
  const [error, setError] = useState("");
  const nameRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (draft.meal) return;
    const t = setTimeout(() => nameRef.current?.focus());
    return () => clearTimeout(t);
  }, [draft.meal]);

  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    if (!title.trim()) return setError("What's the dish?");
    const meal: MealInput = { day, slot, title: title.trim(), notes: notes.trim(), people: who };
    send(draft.meal ? { type: "change_meal", id: draft.meal.id, meal } : { type: "add_meal", meal });
    onClose();
  };
  const toggle = (name: string) => setWho((w) => (w.includes(name) ? w.filter((n) => n !== name) : [...w, name]));

  return (
    <DialogForm
      titleId="mealTitle"
      title={draft.meal ? "Meal" : `${SLOT_NAME[slot] ?? "Meal"} · ${longDay(day || draft.day, { weekday: "short", month: "short", day: "numeric" })}`}
      onClose={onClose}
      onSubmit={submit}
      className="event-form"
      footer={
        <>
          {draft.meal ? (
            <Button
              kind="danger"
              className="event-delete"
              id="mealRemove"
              onClick={() => {
                if (draft.meal) send({ type: "remove_meal", id: draft.meal.id });
                onClose();
              }}
            >
              Remove
            </Button>
          ) : null}
          <Button kind="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button kind="primary" type="submit" id="mealSave">
            Save
          </Button>
        </>
      }
    >
      <Field label="Dish" error={error || undefined}>
        <Input
          id="mealName"
          ref={nameRef}
          value={title}
          maxLength={200}
          placeholder="Chicken tacos"
          onChange={(x) => {
            setTitle(x.target.value);
            setError("");
          }}
        />
      </Field>
      <div className="event-when">
        <Field label="Day">
          <Input type="date" value={day} onChange={(x) => setDay(x.target.value)} required />
        </Field>
        <Field label="Meal">
          <Select value={slot} onChange={(x) => setSlot(x.target.value)}>
            {SLOTS.map((s) => (
              <option key={s} value={s}>
                {SLOT_NAME[s]}
              </option>
            ))}
          </Select>
        </Field>
      </div>
      {people.length ? (
        <fieldset className="event-who">
          <legend className="field-name">Who it's for (none: everyone)</legend>
          {people.map((p) => (
            <Checkbox key={p.name} checked={who.includes(p.name)} onChange={() => toggle(p.name)}>
              {p.name}
            </Checkbox>
          ))}
        </fieldset>
      ) : null}
      <Field label="Notes" hint={draft.meal?.made_by ? `Planned by ${draft.meal.made_by}` : "A recipe link, what to prep, who's cooking."}>
        <TextArea value={notes} rows={3} maxLength={4000} onChange={(x) => setNotes(x.target.value)} />
      </Field>
    </DialogForm>
  );
}
