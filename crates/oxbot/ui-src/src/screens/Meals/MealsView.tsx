// The Meal Plan: the week (each day's breakfast, lunch and dinner; tap a
// dish to change it, + to add one), next to the Meal Plan bot's chat. What
// the meals need goes on Groceries. Under 900px the week is a list of days.
import { useEffect, useMemo, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, IconButton, MenuIcon } from "../../components/ui";
import { addDays, weekday, weekStart } from "../../state/calendar";
import { byDaySlot, SLOTS } from "../../state/meals";
import { local } from "../../state/store";
import { longDay, today } from "../Calendar/CalendarView";
import { HomeMode } from "../Calendar/HomeMode";
import { HomeIcon } from "../Sidebar/HomeIcon";
import { type MealDraft, MealDialog, SLOT_NAME } from "./MealDialog";
import "../Calendar/calendar.css";
import "./meals.css";

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export function MealsView() {
  const { send, dispatch } = useApp();
  const stored = useSel((s) => s.meals.day);
  const got = useSel((s) => s.meals);
  const changed = useSel((s) => s.meals.changed);
  const status = useSel((s) => s.connection.status);
  const now = today();
  const from = weekStart(stored || now);
  const to = addDays(from, 6);
  const days = useMemo(() => Array.from({ length: 7 }, (_, i) => addDays(from, i)), [from]);
  const [draft, setDraft] = useState<MealDraft | null>(null);

  useEffect(() => {
    if (status === "open") send({ type: "home_meals", from, to });
  }, [from, to, changed, status, send]);

  const plan = byDaySlot(got.from === from ? got.meals : []);
  const go = (d: string) => dispatch(local("meals/day", { day: d }));
  const title = `${longDay(from, { month: "short", day: "numeric" })} – ${from.slice(0, 7) === to.slice(0, 7) ? Number(to.slice(8)) : longDay(to, { month: "short", day: "numeric" })}`;

  const cell = (day: string, slot: string) => {
    const dishes = plan.get(day)?.[slot as (typeof SLOTS)[number]] ?? [];
    return (
    <div className="meal-cell">
      {dishes.map((m) => (
        <button key={m.id} type="button" className="meal" title={m.meal.notes || m.meal.title} onClick={() => setDraft({ day, slot, meal: m })}>
          <span className="meal-title">{m.meal.title}</span>
          {m.meal.people.length ? <span className="meal-who">{m.meal.people.join(", ")}</span> : null}
        </button>
      ))}
      {/* Another dish for a meal that has one: a small line, not a box. */}
      <button type="button" className={`meal-add${dishes.length ? " more" : ""}`} aria-label={`Add ${SLOT_NAME[slot]?.toLowerCase()} on ${longDay(day)}`} onClick={() => setDraft({ day, slot })}>
        {dishes.length ? "+ Add" : "+"}
      </button>
    </div>
    );
  };

  return (
    <main className="cal meals" id="meals" aria-label="Meal Plan">
      <header className="cal-head">
        <IconButton className="menu cal-menu" label="Bots" onClick={() => dispatch(local("ui/side", { open: true }))}>
          <MenuIcon />
        </IconButton>
        <HomeIcon id="home-meals" />
        <h1>Meal Plan</h1>
        <HomeMode where="view" view="Plan" />
        <span className="cal-break" aria-hidden="true" />
        <div className="cal-range">
            <IconButton label="Earlier" size="sm" onClick={() => go(addDays(from, -7))}>
              ‹
            </IconButton>
            <span className="cal-title">{title}</span>
            <IconButton label="Later" size="sm" onClick={() => go(addDays(from, 7))}>
              ›
            </IconButton>
            <Button kind="quiet" size="sm" onClick={() => go(now)} disabled={weekStart(now) === from}>
              This week
            </Button>
          </div>
      </header>
      <div className="cal-body">
        <div className="meals-week">
            {/* Wide: a table of days by meal. */}
            <div className="meal-grid" role="table" aria-label="This week's meals">
              <div className="meal-row head" role="row">
                <span className="meal-slot" />
                {days.map((d) => (
                  <span key={d} className={`meal-day${d === now ? " today" : ""}`} role="columnheader">
                    <span className="cal-wd">{WEEKDAYS[weekday(d)]}</span> <span className="cal-dn">{Number(d.slice(8))}</span>
                  </span>
                ))}
              </div>
              {SLOTS.map((slot) => (
                <div key={slot} className="meal-row" role="row">
                  <span className="meal-slot" role="rowheader">
                    {SLOT_NAME[slot]}
                  </span>
                  {days.map((d) => (
                    <div key={d} className={`meal-col${d === now ? " today" : ""}`} role="cell">
                      {cell(d, slot)}
                    </div>
                  ))}
                </div>
              ))}
            </div>
            {/* Narrow: a list of days. */}
            <div className="meal-days">
              {days.map((d) => (
                <section key={d} className={`meal-daycard${d === now ? " today" : ""}`} aria-label={longDay(d)}>
                  <h3>{longDay(d, { weekday: "long", month: "short", day: "numeric" })}</h3>
                  {SLOTS.map((slot) => (
                    <div key={slot} className="meal-line">
                      <span className="meal-slot">{SLOT_NAME[slot]}</span>
                      {cell(d, slot)}
                    </div>
                  ))}
                </section>
              ))}
            </div>
        </div>
      </div>
      <MealDialog draft={draft} onClose={() => setDraft(null)} />
    </main>
  );
}
