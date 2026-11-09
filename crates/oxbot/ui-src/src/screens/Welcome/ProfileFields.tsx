// The profile fields, shared by the welcome screen and Settings → People.
// Each detail says whether bots may use it (never the name, color or photo).
import { useEffect, useRef, useState } from "react";
import { useApp } from "../../app/context";
import { Button, Checkbox, Field, Hint, Input, TextArea, vars } from "../../components/ui";
import type { ProfileView } from "../../net/protocol";
import { type ProfileDraft, ROLES, draftFrom } from "./profile";
import "./welcome.css";

/** A draft that follows the server's view, except while the form is in use. */
export function useProfileDraft(view: ProfileView | null) {
  const formRef = useRef<HTMLFormElement>(null);
  const [draft, setDraft] = useState<ProfileDraft | null>(view ? draftFrom(view) : null);
  useEffect(() => {
    if (!view) return;
    if (formRef.current?.contains(document.activeElement)) return;
    setDraft(draftFrom(view));
  }, [view]);
  return { formRef, draft, setDraft };
}

type TextKey = "name" | "home_area" | "food" | "languages" | "notes";
type UseKey = "home_area_use" | "food_use" | "languages_use" | "notes_use";

/** A photo, made small (96 px, JPEG) in the page before it's sent. */
function shrinkPhoto(file: File, done: (url: string) => void, fail: () => void) {
  const url = URL.createObjectURL(file);
  const img = new Image();
  img.onload = () => {
    const n = 96;
    const c = document.createElement("canvas");
    c.width = n;
    c.height = n;
    const side = Math.min(img.width, img.height);
    c.getContext("2d")?.drawImage(img, (img.width - side) / 2, (img.height - side) / 2, side, side, 0, 0, n, n);
    URL.revokeObjectURL(url);
    done(c.toDataURL("image/jpeg", 0.8));
  };
  img.onerror = () => {
    URL.revokeObjectURL(url);
    fail();
  };
  img.src = url;
}

/** `boxId` is the fields' box id ("welcomeFields", "meFields"): the colour
 *  radios and the photo picker are named after it. */
export function ProfileFields({
  boxId,
  view,
  draft,
  onChange,
}: {
  boxId: string;
  view: ProfileView;
  draft: ProfileDraft;
  onChange: (d: ProfileDraft) => void;
}) {
  const { toast } = useApp();
  const pickRef = useRef<HTMLInputElement>(null);
  const set = (patch: Partial<ProfileDraft>) => onChange({ ...draft, ...patch });

  const field = (label: string, key: TextKey, max: number, use: UseKey | null, area = false, placeholder?: string) => {
    const common = {
      "data-key": key,
      maxLength: max,
      autoComplete: "off",
      placeholder,
      value: draft[key],
    };
    const input = area ? (
      <TextArea {...common} onChange={(e) => set({ [key]: e.target.value })} />
    ) : (
      <Input {...common} onChange={(e) => set({ [key]: e.target.value })} />
    );
    if (use === null)
      return (
        <Field label={label} hint="Bots may use this: never">
          {input}
        </Field>
      );
    return (
      <div className="pf">
        <Field label={label}>{input}</Field>
        <Checkbox className="use" data-use={key} checked={draft[use]} onChange={(e) => set({ [use]: e.target.checked })}>
          Bots may use this
        </Checkbox>
      </div>
    );
  };

  return (
    <div id={boxId} className="profile-fields">
      {field("Name", "name", 40, null)}
      {/* Color and photo: shown with your messages, never sent to bots. */}
      <div className="pf">
        <span className="pf-label">Color and photo</span>
        <div className="swatches">
          {view.colors.map((c) => (
            <label key={c} style={vars({ person: c })} title={c}>
              <input type="radio" name={`${boxId}Color`} value={c} checked={c === draft.color} aria-label={c} onChange={() => set({ color: c })} />
            </label>
          ))}
        </div>
        <div className="photo-row">
          <img className="photo" alt="Your photo" hidden={!draft.photo} src={draft.photo ?? undefined} />
          <Button size="sm" onClick={() => pickRef.current?.click()}>
            {draft.photo ? "Change photo" : "Add a photo"}
          </Button>
          <Button kind="quiet" size="sm" hidden={!draft.photo} onClick={() => set({ photo: null })}>
            Remove
          </Button>
          <input
            type="file"
            accept="image/*"
            className="file-input"
            id={`${boxId}Photo`}
            ref={pickRef}
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) shrinkPhoto(f, (url) => set({ photo: url }), () => toast("That picture can't be read; pick another.", true));
            }}
          />
        </div>
        <p className="use">Bots may use this: never</p>
      </div>
      <p className="role">{`In the home: ${ROLES[view.role] || view.role}`}</p>
      {view.can_edit_area ? (
        field("Home area", "home_area", 100, "home_area_use", false, "City, region")
      ) : view.home_area ? (
        <Hint>{`Home area: ${view.home_area} (the owner sets it)`}</Hint>
      ) : null}
      {field("Food needs", "food", 200, "food_use", false, "Optional: vegetarian, no nuts")}
      {field("Languages", "languages", 200, "languages_use", false, "Optional")}
      {field("Notes for bots", "notes", 500, "notes_use", true, "Optional: I prefer metric; no paid apps")}
      {view.owner_confirm ? (
        <div className="confirms">
          <Checkbox data-confirm="adult" checked={draft.adult} onChange={(e) => set({ adult: e.target.checked })}>
            I'm 18 or older.
          </Checkbox>
          <Checkbox data-confirm="terms" checked={draft.terms} onChange={(e) => set({ terms: e.target.checked })}>
            I accept OpenRouter's terms of service (openrouter.ai/terms), for the home's AI account.
          </Checkbox>
        </div>
      ) : null}
    </div>
  );
}
