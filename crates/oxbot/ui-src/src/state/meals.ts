// The Meal Plan: the week's dishes and the grocery list as the server last
// sent them, the week being looked at, and a count bumped when the
// Home changed (the screen then asks again). Also the grouping the screen
// and its tests share.
import type { GroceryInfo, MealInfo } from "../net/protocol";
import type { Action } from "./store";

export const SLOTS = ["breakfast", "lunch", "dinner"] as const;
export type MealSlot = (typeof SLOTS)[number];

export interface MealsState {
  /** A day in the week being looked at ("2026-11-16"); empty: this week. */
  day: string;
  /** The last answer: its range and dishes. */
  from: string;
  to: string;
  meals: MealInfo[];
  groceries: GroceryInfo[];
  /** Whether a grocery list came yet. */
  loaded: boolean;
  /** Bumped when the server says the Home changed (ask again). */
  changed: number;
}

export const initialMeals: MealsState = { day: "", from: "", to: "", meals: [], groceries: [], loaded: false, changed: 0 };

export function mealsReducer(state: MealsState, action: Action): MealsState {
  if (action.kind === "server") {
    const msg = action.msg;
    switch (msg.type) {
      case "home_meals":
        return { ...state, from: msg.from, to: msg.to, meals: msg.meals };
      case "groceries":
        return { ...state, groceries: msg.items, loaded: true };
      case "home_changed":
        return { ...state, changed: state.changed + 1 };
      default:
        return state;
    }
  }
  if (action.kind !== "local") return state;
  switch (action.type) {
    case "meals/day":
      return { ...state, day: action.day as string };
    default:
      return state;
  }
}

/** The week's dishes by day, then meal: `byDay.get(day)?.[slot]`. */
export function byDaySlot(meals: readonly MealInfo[]): Map<string, Record<MealSlot, MealInfo[]>> {
  const out = new Map<string, Record<MealSlot, MealInfo[]>>();
  for (const m of meals) {
    const day = out.get(m.meal.day) ?? { breakfast: [], lunch: [], dinner: [] };
    const slot = (SLOTS as readonly string[]).includes(m.meal.slot) ? (m.meal.slot as MealSlot) : "dinner";
    day[slot].push(m);
    out.set(m.meal.day, day);
  }
  return out;
}

/** The list by aisle (A-Z, "Other" last); a ticked item stays in its
 *  aisle, after what's still to get. Also what's got, for Clear got. */
export function byAisle(items: readonly GroceryInfo[]): { aisles: [string, GroceryInfo[]][]; got: GroceryInfo[] } {
  const groups = new Map<string, GroceryInfo[]>();
  for (const g of [...items.filter((g) => !g.done), ...items.filter((g) => g.done)]) {
    const aisle = g.item.aisle || "Other";
    groups.set(aisle, [...(groups.get(aisle) ?? []), g]);
  }
  const aisles = [...groups.entries()].sort(([a], [b]) => (a === "Other" ? 1 : b === "Other" ? -1 : a.localeCompare(b)));
  return { aisles, got: items.filter((g) => g.done) };
}
