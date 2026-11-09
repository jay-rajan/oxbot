// The welcome screen: shown while the server says this person hasn't filled
// in their details yet (profile.welcome), and it stays until they're saved.
import { type FormEvent, useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Dialog, DialogForm, ErrorText } from "../../components/ui";
import { ProfileFields, useProfileDraft } from "./ProfileFields";
import { profileEdit } from "./profile";
import { Board } from "../../components/Board";
import "./welcome.css";

const noop = () => {};

export function WelcomeDialog() {
  const profile = useSel((s) => s.home.profile);
  const open = !!profile?.welcome;

  // Escape doesn't close it.
  useEffect(() => {
    const d = document.getElementById("welcomeDlg");
    const stop = (ev: Event) => ev.preventDefault();
    d?.addEventListener("cancel", stop);
    return () => d?.removeEventListener("cancel", stop);
  }, []);

  return (
    <Dialog id="welcomeDlg" size="lg" labelledBy="welcomeTitle" open={open} onClose={noop}>
      <WelcomeForm />
    </Dialog>
  );
}

function WelcomeForm() {
  const { send } = useApp();
  const profile = useSel((s) => s.home.profile);
  const { formRef, draft, setDraft } = useProfileDraft(profile);
  const [err, setErr] = useState<string | null>(null);
  // A fresh profile from the server clears the last complaint.
  useEffect(() => setErr(null), [profile]);

  const save = (ev: FormEvent) => {
    ev.preventDefault();
    if (!profile || !draft) return;
    const r = profileEdit(draft, profile);
    if ("error" in r) {
      setErr(r.error);
      return;
    }
    setErr(null);
    send({ type: "set_profile", profile: r.edit });
  };

  return (
    <DialogForm
      id="welcomeForm"
      ref={formRef}
      className="profile-form"
      title="Welcome to oxbot"
      titleId="welcomeTitle"
      onSubmit={save}
      footer={
        <Button kind="primary" type="submit">
          Save and start
        </Button>
      }
    >
      <div className="welcome-lead">
        <span className="ttt-hero" id="welcomeArt" aria-hidden="true">
          <Board once />
        </span>
        <p className="muted">Tell oxbot who you are. It stays on the home computer.</p>
      </div>
      {profile && draft ? <ProfileFields boxId="welcomeFields" view={profile} draft={draft} onChange={setDraft} /> : <div id="welcomeFields" />}
      <ErrorText id="welcomeErr" hidden={!err}>
        {err}
      </ErrorText>
    </DialogForm>
  );
}
