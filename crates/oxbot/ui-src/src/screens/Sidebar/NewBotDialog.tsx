// New bot: a name, what it should do, and whether it's private. oxbot picks
// its models from the instructions.
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Checkbox, Dialog, DialogForm, Field, Input, TextArea } from "../../components/ui";
import { local } from "../../state/store";

export function NewBotDialog() {
  const { dispatch } = useApp();
  const open = useSel((s) => s.ui.dialog === "newBot");
  const close = () => dispatch(local("ui/close", { dialog: "newBot" }));
  return (
    <Dialog id="newDlg" open={open} onClose={close} labelledBy="nbTitle">
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
    <DialogForm
      id="newForm"
      title="New bot"
      titleId="nbTitle"
      onClose={close}
      onSubmit={submit}
      footer={
        <>
          <Button kind="secondary" id="nbCancel" onClick={close}>
            Cancel
          </Button>
          <Button kind="primary" type="submit">
            Create
          </Button>
        </>
      }
    >
      <Field label="Name">
        <Input id="nbName" ref={name} required placeholder="Daily News" autoComplete="off" />
      </Field>
      <Field label="What should it do?">
        <TextArea
          id="nbInstr"
          ref={instr}
          placeholder="Every morning when I ask, read the top stories on news.ycombinator.com and give me five lines."
        />
      </Field>
      <Checkbox rowId="nbPrivateRow" rowHidden={!ownsHome} id="nbPrivate" checked={priv} onChange={(e) => setPriv(e.target.checked)}>
        Private: only you can see and use it
      </Checkbox>
    </DialogForm>
  );
}
