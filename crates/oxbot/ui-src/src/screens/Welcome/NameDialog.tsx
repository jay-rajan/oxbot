// A name, asked in a small dialog: for a new or renamed section or bot (not
// typed in the list, which is redrawn on every update). Open it with
// `askName(dispatch, {title, value, max, done})`; `done(name)` runs on Save
// with the trimmed name. Cancel or Escape just closes it.
import { type FormEvent, useCallback, useEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { Dialog } from "../../components/Dialog";
import { type Action, local } from "../../state/store";

export interface NameAsk {
  title: string;
  /** What's there now; empty for a new one (the button says Create). */
  value: string;
  max: number;
  done: (name: string) => void;
}

// The open question: a callback can't live in the store.
let pending: NameAsk | null = null;

export function askName(dispatch: (a: Action) => void, ask: NameAsk): void {
  pending = ask;
  dispatch(local("ui/open", { dialog: "name" }));
}

export function NameDialog() {
  const { dispatch } = useApp();
  const open = useSel((s) => s.ui.dialog === "name");
  const onClose = useCallback(() => {
    pending = null;
    dispatch(local("ui/close", { dialog: "name" }));
  }, [dispatch]);
  return (
    <Dialog id="nameDlg" labelledBy="ndTitle" open={open} onClose={onClose}>
      <NameForm onClose={onClose} />
    </Dialog>
  );
}

function NameForm({ onClose }: { onClose: () => void }) {
  const ask = pending;
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const save = (ev: FormEvent) => {
    ev.preventDefault();
    const name = ref.current?.value.trim() ?? "";
    if (!name) return;
    const done = ask?.done;
    onClose();
    done?.(name);
  };
  return (
    <form id="nameForm" onSubmit={save}>
      <h2 id="ndTitle">{ask?.title ?? "Name"}</h2>
      <label>
        Name
        <input id="ndName" ref={ref} required autoComplete="off" spellCheck={false} defaultValue={ask?.value ?? ""} maxLength={ask?.max} />
      </label>
      <div className="row">
        <button className="ghost" type="button" id="ndCancel" onClick={onClose}>
          Cancel
        </button>
        <button className="primary" type="submit" id="ndSave">
          {ask?.value ? "Save" : "Create"}
        </button>
      </div>
    </form>
  );
}
