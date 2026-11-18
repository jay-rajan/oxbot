// Settings → Memory: what bots remember about each person, as a list to add
// to and take from. Bots get the asker's list with each task, never their
// name. On the home computer everyone's list shows; on a phone, your own.
import { type FormEvent, useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, CloseIcon, IconButton, Input, Section, vars } from "../../components/ui";

export function MemoryTab({ hidden }: { hidden: boolean }) {
  const settings = useSel((s) => s.connection.settings);
  const people = useSel((s) => s.home.people);
  const homeComputer = !!settings?.home_computer;
  const you = settings?.you;
  const shown = homeComputer ? people : people.filter((p) => p.name === you);
  return (
    <div id="setMemory" className="settings-pane" role="tabpanel" hidden={hidden}>
      <p className="note-text">Things bots should keep in mind when someone asks them for help, like "Vegetarian" or "Speaks Malayalam". Bots see the list of whoever is asking, never their name.</p>
      {hidden ? null : shown.map((p) => (
        <Section
          key={p.name}
          className="memory-section"
          title={
            <span className="memory-who">
              <span className="person-dot maker-dot" style={vars({ person: p.color })} />
              {p.name === you ? `${p.name} (you)` : p.name}
            </span>
          }
        >
          <MemoryList person={p.name} />
        </Section>
      ))}
      {!hidden && !shown.length ? <p className="field-hint">Add people in People first.</p> : null}
    </div>
  );
}

function MemoryList({ person }: { person: string }) {
  const { send } = useApp();
  const items = useSel((s) => s.home.memory[person]);
  const [text, setText] = useState("");
  useEffect(() => {
    send({ type: "get_memory", person });
  }, [person, send]);
  const add = (ev: FormEvent) => {
    ev.preventDefault();
    const t = text.trim();
    if (!t) return;
    send({ type: "add_memory", person, text: t });
    setText("");
  };
  return (
    <div className="memory" data-person={person}>
      {items?.length ? (
        <ul className="memory-list">
          {items.map((m) => (
            <li key={m.id}>
              <span>{m.text}</span>
              <IconButton label={`Forget: ${m.text}`} className="memory-x" onClick={() => send({ type: "remove_memory", person, id: m.id })}>
                <CloseIcon />
              </IconButton>
            </li>
          ))}
        </ul>
      ) : items ? (
        <p className="field-hint">Nothing yet.</p>
      ) : null}
      <form className="add-site" onSubmit={add}>
        <Input value={text} onChange={(e) => setText(e.target.value)} placeholder={`Something to remember about ${person}`} aria-label={`Something bots should remember about ${person}`} autoComplete="off" maxLength={200} />
        <Button type="submit" disabled={!text.trim()}>
          Add
        </Button>
      </form>
    </div>
  );
}
