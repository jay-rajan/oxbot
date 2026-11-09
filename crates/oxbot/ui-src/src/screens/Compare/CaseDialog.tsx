// "Keep as a test case": a finished turn kept to replay on other models.
// Opens when the server sends the sheet's draft (after "Test case…").
import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Checkbox, Dialog, DialogForm, Field, Hint, Input, Select, TextArea } from "../../components/ui";
import type { CaseDraft, TrialNetwork } from "../../net/protocol";
import { local } from "../../state/store";

export function CaseDialog() {
  const { dispatch } = useApp();
  const open = useSel((s) => s.ui.dialog === "case");
  const draft = useSel((s) => s.cards.caseDraft);
  const onClose = useCallback(() => dispatch(local("ui/close", { dialog: "case" })), [dispatch]);
  // Each new draft opens the sheet.
  const seen = useRef(draft?.n ?? 0);
  useEffect(() => {
    if (draft && draft.n !== seen.current) {
      seen.current = draft.n;
      dispatch(local("ui/open", { dialog: "case" }));
    }
  }, [draft, dispatch]);
  return (
    <Dialog id="caseDlg" open={open && !!draft} onClose={onClose} labelledBy="caseTitleH">
      {draft ? <CaseForm key={draft.n} draft={draft} onClose={onClose} /> : null}
    </Dialog>
  );
}

function CaseForm({ draft, onClose }: { draft: CaseDraft; onClose: () => void }) {
  const { send, toast } = useApp();
  const [title, setTitle] = useState(draft.title);
  const [rubric, setRubric] = useState(draft.rubric);
  const [signIns, setSignIns] = useState(!!draft.uses_sign_ins);
  const [network, setNetwork] = useState<TrialNetwork>("strict");
  const sites = draft.sites ?? [];
  const hint = `${draft.kind === "acts" ? "Test runs record its live steps instead of doing them." : "It only read."}${sites.length ? ` Sites: ${sites.join(", ")}.` : ""}`;
  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    send({ type: "mark_test_case", bot: draft.bot, seq: draft.seq, title: title.trim(), rubric, network, uses_sign_ins: signIns });
    onClose();
    toast("Kept as a test case. Run it from Compare in the bot's menu.");
  };
  return (
    <DialogForm
      id="caseForm"
      onSubmit={submit}
      title="Keep as a test case"
      titleId="caseTitleH"
      onClose={onClose}
      footer={
        <>
          <Button kind="secondary" id="caseCancel" onClick={onClose}>
            Cancel
          </Button>
          <Button kind="primary" type="submit" id="caseSave">
            Keep
          </Button>
        </>
      }
    >
      <Field label="Title">
        <Input id="caseTitle" required maxLength={120} autoComplete="off" value={title} onChange={(e) => setTitle(e.target.value)} />
      </Field>
      <Field label="What a good result looks like" hint="One check per line.">
        <TextArea id="caseRubric" rows={4} placeholder="Lists at least 3 jobs in Riverside" value={rubric} onChange={(e) => setRubric(e.target.value)} />
      </Field>
      <Checkbox id="caseSignIns" checked={signIns} onChange={(e) => setSignIns(e.target.checked)}>
        Needs websites you're signed in to
      </Checkbox>
      <Field label="Network in test runs">
        <Select id="caseNetwork" value={network} onChange={(e) => setNetwork(e.target.value as TrialNetwork)}>
          <option value="strict">Strict: block every write</option>
          <option value="xhr">Allow reads sent as POST</option>
        </Select>
      </Field>
      <Hint id="caseHint">{hint}</Hint>
    </DialogForm>
  );
}
