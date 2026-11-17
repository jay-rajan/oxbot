// A note: what it is, where it's kept, and changing its title, day, topic,
// tags or people. Its file moves to match.
import { type FormEvent, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Dialog, DialogForm } from "../../components/ui";
import type { HomeNoteInfo, NoteInput } from "../../net/protocol";
import { dayFrom, topics as topicsOf } from "../../state/notes";
import { NoteFields } from "./NoteFields";

export function NoteDialog({
  note,
  onClose,
  onDeleted,
}: {
  note: HomeNoteInfo | null;
  onClose: () => void;
  /** It was deleted (for an Undo line). */
  onDeleted: (n: HomeNoteInfo) => void;
}) {
  return (
    <Dialog id="noteDialog" open={!!note} onClose={onClose} size="md" labelledBy="noteTitle">
      {note ? <NoteForm note={note} onClose={onClose} onDeleted={onDeleted} /> : null}
    </Dialog>
  );
}

function NoteForm({ note, onClose, onDeleted }: { note: HomeNoteInfo; onClose: () => void; onDeleted: (n: HomeNoteInfo) => void }) {
  const { send } = useApp();
  const notes = useSel((s) => s.notes.notes);
  const [input, setInput] = useState<NoteInput>({
    title: note.title,
    day: note.day,
    topic: note.topic,
    tags: note.tags,
    people: note.people,
  });
  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    // An unchanged day keeps where it came from.
    send({ type: "change_note", id: note.id, note: { ...input, day: input.day === note.day ? "" : input.day } });
    onClose();
  };
  return (
    <DialogForm
      titleId="noteTitle"
      title="Record"
      onClose={onClose}
      onSubmit={submit}
      className="note-form"
      footer={
        <>
          <Button
            kind="danger"
            className="event-delete"
            id="noteDelete"
            onClick={() => {
              send({ type: "delete_note", id: note.id });
              onDeleted(note);
              onClose();
            }}
          >
            Delete
          </Button>
          <Button kind="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button kind="primary" type="submit" id="noteSave">
            Save
          </Button>
        </>
      }
    >
      <p className="note-where">
        {note.private ? "🔒 Private · " : "✦ The AI read it · "}
        Records/{note.file}
        <br />
        {note.name} · uploaded {new Date(note.uploaded_ms).toLocaleDateString()}
        {note.uploaded_by ? ` by ${note.uploaded_by}` : ""} · {dayFrom(note)}
      </p>
      {note.summary ? <p className="note-summary">{note.summary}</p> : null}
      <NoteFields
        note={input}
        onChange={setInput}
        placeholder={note.title}
        dayHint="The day it's about: a bill's, a visit's, a statement's."
        topics={topicsOf(notes)}
        idPrefix="noteEdit"
      />
    </DialogForm>
  );
}
