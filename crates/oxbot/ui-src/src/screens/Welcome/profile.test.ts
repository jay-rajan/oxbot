import { describe, expect, it } from "vitest";
import type { ProfileView } from "../../net/protocol";
import { CONFIRM_ERROR, draftFrom, profileEdit } from "./profile";

const view: ProfileView = {
  name: "Jay", color: "#111", colors: ["#111", "#222"], photo: null, role: "owner",
  food: "", food_use: true, languages: "", languages_use: true, notes: "", notes_use: false,
  home_area: "", home_area_use: true, can_edit_area: true, welcome: true, owner_confirm: true,
};

describe("profile", () => {
  it("the owner confirms before it saves", () => {
    const d = { ...draftFrom(view), food: "vegetarian" };
    expect(profileEdit(d, view)).toEqual({ error: CONFIRM_ERROR });
    const r = profileEdit({ ...d, adult: true, terms: true, name: " Jay " }, view);
    expect(r).toMatchObject({ edit: { name: "Jay", food: "vegetarian", owner_confirmed: true, home_area: "" } });
  });

  it("someone else: no confirmation, no home area", () => {
    const v = { ...view, role: "adult", owner_confirm: false, can_edit_area: false, home_area: "Toronto" };
    const r = profileEdit({ ...draftFrom(v), color: "" }, v);
    expect(r).toMatchObject({ edit: { home_area: null, home_area_use: null, owner_confirmed: false, color: "#111" } });
  });
});
