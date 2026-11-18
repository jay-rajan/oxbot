import { describe, expect, it } from "vitest";
import type { GroceryInfo, MealInfo } from "../net/protocol";
import { byAisle, byDaySlot } from "./meals";

const meal = (id: number, day: string, slot: string, title: string): MealInfo => ({
  id,
  meal: { day, slot, title, notes: "", people: [] },
  made_by: "",
});
const item = (id: number, name: string, aisle: string, done = false): GroceryInfo => ({
  id,
  item: { name, qty: "", aisle },
  done,
  for_meal: "",
  added_by: "",
});

describe("meal plan", () => {
  it("lays the week out by day and meal", () => {
    const m = byDaySlot([meal(1, "2026-11-18", "dinner", "Tacos"), meal(2, "2026-11-18", "breakfast", "Oatmeal"), meal(3, "2026-11-18", "dinner", "Salad")]);
    expect(m.get("2026-11-18")?.dinner.map((x) => x.meal.title)).toEqual(["Tacos", "Salad"]);
    expect(m.get("2026-11-18")?.breakfast.map((x) => x.id)).toEqual([2]);
    expect(m.get("2026-11-19")).toBeUndefined();
  });

  it("groups by aisle, Other last; a ticked item stays in its aisle, after the rest", () => {
    const g = byAisle([item(4, "Eggs", "Dairy", true), item(1, "Milk", "Dairy"), item(2, "Foil", ""), item(3, "Apples", "Produce")]);
    expect(g.aisles.map(([a, l]) => [a, l.map((x) => x.item.name)])).toEqual([
      ["Dairy", ["Milk", "Eggs"]],
      ["Produce", ["Apples"]],
      ["Other", ["Foil"]],
    ]);
    expect(g.got.map((x) => x.id)).toEqual([4]);
  });
});
