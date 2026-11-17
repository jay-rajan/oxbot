// Attaching files in Family Notes: a form for each file, asked right away:
// its title, date, who it's about, topic, tags, and whether the AI may read
// it. Save files them (and sends anything typed in the box). A private file
// is filed with only what's typed here; one the AI may read, the bot reads
// and fills in what's left (its date from the document, a summary).
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Dialog, DialogForm } from "../../components/ui";
import type { NoteInput } from "../../net/protocol";
import type { Chip } from "../../state/chat";
import { topics as topicsOf } from "../../state/notes";
import { local } from "../../state/store";
import { removeChip, sendMessage, sizeText } from "../Chat/actions";
import { EMPTY_NOTE, NoteFields } from "./NoteFields";

const NO_CHIPS: Chip[] = [];

export function FileDialog({ bot }: { bot: string }) {
  const app = useApp();
  const chips = useSel((s) => s.chat.drafts[bot]) ?? NO_CHIPS;
  const notes = useSel((s) => s.notes.notes);
  const status = useSel((s) => s.connection.status);
  const [open, setOpen] = useState(false);
  const count = useRef(chips.length);
  const live = chips.filter((c) => c.state !== "error");

  // New files: ask about them (and get the topics in use).
  useEffect(() => {
    if (chips.length > count.current) {
      setOpen(true);
      if (status === "open") app.send({ type: "home_notes" });
    }
    if (!live.length) setOpen(false);
    count.current = chips.length;
  }, [chips.length, live.length, status, app]);

  const cancel = () => {
    for (const c of chips) removeChip(app, bot, c.key);
    setOpen(false);
  };
  const save = (ev: FormEvent) => {
    ev.preventDefault();
    if (live.some((c) => c.state === "uploading")) return;
    sendMessage(app);
    setOpen(false);
  };
  const waiting = live.some((c) => c.state === "uploading");
  const topics = topicsOf(notes);

  return (
    <Dialog id="noteFileDialog" open={open && !!live.length} onClose={() => setOpen(false)} size="md" labelledBy="noteFileTitle">
      <DialogForm
        titleId="noteFileTitle"
        title={live.length === 1 ? "Keep in Family Notes" : `Keep ${live.length} files in Family Notes`}
        onClose={() => setOpen(false)}
        onSubmit={save}
        className="note-file-form"
        footer={
          <>
            <Button kind="secondary" onClick={cancel}>
              Cancel
            </Button>
            <Button kind="primary" type="submit" id="noteFileSave" disabled={waiting || status !== "open"}>
              {waiting ? "Uploading…" : "Save"}
            </Button>
          </>
        }
      >
        {live.map((c) => (
          <FileForm key={c.key} c={c} topics={topics} />
        ))}
      </DialogForm>
    </Dialog>
  );
}

function FileForm({ c, topics }: { c: Chip; topics: readonly string[] }) {
  const app = useApp();
  const note = c.note ?? EMPTY_NOTE;
  const patch = (p: Partial<Chip>) => app.dispatch(local("chat/chip", { bot: c.bot, key: c.key, patch: p }));
  const stem = c.name.replace(/\.[^.]+$/, "").replace(/[_-]+/g, " ").trim();
  const progress = c.state === "uploading" ? (c.progress >= 1 ? "checking…" : `${Math.round(c.progress * 100)}%`) : sizeText(c.att?.size ?? c.size);
  return (
    <section className="note-file" aria-label={c.name}>
      <p className="note-file-name">
        {c.state === "uploading" ? <span className="spin" aria-hidden="true" /> : null}
        <span>{c.name}</span>
        <span className="note-file-size">{progress}</span>
      </p>
      <fieldset className="note-ai">
        <legend className="field-name">Who reads it</legend>
        <label className={`note-ai-pick${c.ai ? "" : " on"}`}>
          <input type="radio" name={`ai${c.key}`} checked={!c.ai} onChange={() => patch({ ai: false })} />
          <span>
            <b>🔒 Private</b>
            <small>Kept on the home computer. Filed only by what you type here; the AI never sees it.</small>
          </span>
        </label>
        <label className={`note-ai-pick${c.ai ? " on" : ""}`}>
          <input type="radio" name={`ai${c.key}`} checked={!!c.ai} onChange={() => patch({ ai: true })} />
          <span>
            <b>✦ AI may read it</b>
            <small>The whole file is sent to the AI provider. The bot fills in what you leave empty: its date, topic, tags.</small>
          </span>
        </label>
      </fieldset>
      <NoteFields
        note={note}
        onChange={(n: NoteInput) => patch({ note: n })}
        placeholder={stem || "Title"}
        dayHint={c.ai ? "Empty: the date in the document, or today." : "Empty: today, the day it was uploaded."}
        topics={topics}
        idPrefix={`note${c.key}`}
      />
    </section>
  );
}
