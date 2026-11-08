// New bot: a name, what it should do, and whether it's private. oxbot picks
// its models from the instructions.
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Dialog } from "../../components/Dialog";
import { local } from "../../state/store";

export function NewBotDialog() {
  const { dispatch } = useApp();
  const open = useSel((s) => s.ui.dialog === "newBot");
  const close = () => dispatch(local("ui/close", { dialog: "newBot" }));
  return (
    <Dialog id="newDlg" open={open} onClose={close}>
      <NewBotForm close={close} />
    </Dialog>
  );
}

function NewBotForm({ close }: { close: () => void }) {
  const { send } = useApp();
  // Only the owner of the home computer can have private bots.
  const ownsHome = useSel((s) => !!s.connection.settings?.owner);
  const [priv, setPriv] = useState(false);
  const name = useRef<HTMLInputElement>(null);
  const instr = useRef<HTMLTextAreaElement>(null);
  useEffect(() => name.current?.focus(), []);
  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    send({
      type: "create_bot",
      name: (name.current?.value ?? "").trim(),
      instructions: (instr.current?.value ?? "").trim(),
      private: ownsHome && priv,
      engine: null,
      models: [],
    });
    close();
  };
  return (
    <form id="newForm" onSubmit={submit}>
      <h2>New bot</h2>
      <label>
        Name
        <input id="nbName" ref={name} required placeholder="Daily News" autoComplete="off" />
      </label>
      <label>
        What should it do?
        <textarea
          id="nbInstr"
          ref={instr}
          placeholder="Every morning when I ask, read the top stories on news.ycombinator.com and give me five lines."
        />
      </label>
      <label className="check" id="nbPrivateRow" hidden={!ownsHome}>
        <input type="checkbox" id="nbPrivate" checked={priv} onChange={(e) => setPriv(e.target.checked)} /> Private: only you can
        see and use it
      </label>
      <div className="row">
        <button className="ghost" type="button" id="nbCancel" onClick={close}>
          Cancel
        </button>
        <button className="primary" type="submit">
          Create
        </button>
      </div>
    </form>
  );
}
