// The fields a note is filed by: title, day, who it's about, topic and
// tags. Shared by the form shown when files are attached in Family Notes
// and by a note's edit dialog.
import { Checkbox, Field, Input } from "../../components/ui";
import type { NoteInput } from "../../net/protocol";
import { useSel } from "../../app/context";

export const EMPTY_NOTE: NoteInput = { title: "", day: "", topic: "", tags: [], people: [] };

/** "a, b , c" → ["a", "b", "c"]. */
export const splitTags = (t: string) => t.split(",").map((x) => x.trim()).filter(Boolean);

export function NoteFields({
  note,
  onChange,
  placeholder,
  dayHint,
  topics,
  idPrefix,
}: {
  note: NoteInput;
  onChange: (n: NoteInput) => void;
  /** The title used if none is given (the file's name). */
  placeholder: string;
  dayHint: string;
  topics: readonly string[];
  idPrefix: string;
}) {
  const people = useSel((s) => s.home.people);
  const set = (patch: Partial<NoteInput>) => onChange({ ...note, ...patch });
  const toggle = (name: string) =>
    set({ people: note.people.includes(name) ? note.people.filter((n) => n !== name) : [...note.people, name] });
  return (
    <div className="note-fields">
      <Field label="Title">
        <Input id={`${idPrefix}Title`} value={note.title} maxLength={200} placeholder={placeholder} onChange={(x) => set({ title: x.target.value })} />
      </Field>
      <div className="note-row">
        <Field label="Date" hint={dayHint}>
          <Input id={`${idPrefix}Day`} type="date" value={note.day} onChange={(x) => set({ day: x.target.value })} />
        </Field>
        <Field label="Topic" hint="A folder, like Medical or Bills.">
          <Input id={`${idPrefix}Topic`} value={note.topic} maxLength={40} list={`${idPrefix}Topics`} placeholder="Unsorted" onChange={(x) => set({ topic: x.target.value })} />
          <datalist id={`${idPrefix}Topics`}>
            {topics.filter((t) => t !== "Unsorted").map((t) => (
              <option key={t} value={t} />
            ))}
          </datalist>
        </Field>
      </div>
      {people.length ? (
        <fieldset className="note-who">
          <legend className="field-name">Who it's about</legend>
          {people.map((p) => (
            <Checkbox key={p.name} checked={note.people.includes(p.name)} onChange={() => toggle(p.name)}>
              {p.name}
            </Checkbox>
          ))}
        </fieldset>
      ) : null}
      <Field label="Tags" hint="Words to find it by, with commas: insurance, car, renewal.">
        <Input
          id={`${idPrefix}Tags`}
          defaultValue={note.tags.join(", ")}
          maxLength={300}
          onChange={(x) => set({ tags: splitTags(x.target.value) })}
        />
      </Field>
    </div>
  );
}
