// One's own details as the welcome screen and Settings → People edit them:
// a draft made from what the server sent, and the edit saved from it.
import type { ProfileEdit, ProfileView } from "../../net/protocol";

export const ROLES: Record<string, string> = {
  owner: "Owner: you installed oxbot and connect the home's AI account",
  adult: "Adult",
  kid: "Kid (a parent approves what's risky)",
};

export interface ProfileDraft {
  name: string;
  color: string;
  photo: string | null;
  home_area: string;
  home_area_use: boolean;
  food: string;
  food_use: boolean;
  languages: string;
  languages_use: boolean;
  notes: string;
  notes_use: boolean;
  /** The owner's two confirmations (shown only while `owner_confirm`). */
  adult: boolean;
  terms: boolean;
}

export function draftFrom(view: ProfileView): ProfileDraft {
  return {
    name: view.name || "",
    color: view.color,
    photo: view.photo,
    home_area: view.home_area || "",
    home_area_use: view.home_area_use,
    food: view.food || "",
    food_use: view.food_use,
    languages: view.languages || "",
    languages_use: view.languages_use,
    notes: view.notes || "",
    notes_use: view.notes_use,
    adult: false,
    terms: false,
  };
}

export const CONFIRM_ERROR = "To finish, confirm you're 18 or older and accept OpenRouter's terms.";

/** The edit to send, or why it can't go yet. */
export function profileEdit(draft: ProfileDraft, view: ProfileView): { edit: ProfileEdit } | { error: string } {
  const owner_confirmed = view.owner_confirm && draft.adult && draft.terms;
  if (view.owner_confirm && !owner_confirmed) return { error: CONFIRM_ERROR };
  return {
    edit: {
      name: draft.name.trim(),
      color: draft.color || view.color || "",
      photo: draft.photo || null,
      food: draft.food,
      food_use: draft.food_use,
      languages: draft.languages,
      languages_use: draft.languages_use,
      notes: draft.notes,
      notes_use: draft.notes_use,
      home_area: view.can_edit_area ? draft.home_area : null,
      home_area_use: view.can_edit_area ? draft.home_area_use : null,
      owner_confirmed,
    },
  };
}
