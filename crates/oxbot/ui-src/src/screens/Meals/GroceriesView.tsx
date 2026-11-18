// Groceries: the family's shared shopping list (food and household things),
// by aisle, beside the Groceries bot's chat. The Meal Plan bot adds what its
// meals need to the same list.
import { useEffect } from "react";
import { useApp, useSel } from "../../app/context";
import { IconButton, MenuIcon } from "../../components/ui";
import { local } from "../../state/store";
import { HomeMode } from "../Calendar/HomeMode";
import { HomeIcon } from "../Sidebar/HomeIcon";
import { Groceries } from "./Groceries";
import "../Calendar/calendar.css";
import "./meals.css";

export function GroceriesView() {
  const { send, dispatch } = useApp();
  const changed = useSel((s) => s.meals.changed);
  const status = useSel((s) => s.connection.status);
  const left = useSel((s) => s.meals.groceries.filter((g) => !g.done).length);
  useEffect(() => {
    if (status === "open") send({ type: "groceries" });
  }, [changed, status, send]);
  return (
    <main className="cal groceries-view" id="groceries" aria-label="Groceries">
      <header className="cal-head">
        <IconButton className="menu cal-menu" label="Bots" onClick={() => dispatch(local("ui/side", { open: true }))}>
          <MenuIcon />
        </IconButton>
        <HomeIcon id="home-groceries" />
        <h1>
          Groceries {left ? <span className="groceries-left">{left} to get</span> : null}
        </h1>
        <HomeMode where="view" view="List" />
      </header>
      <div className="cal-body">
        <Groceries />
      </div>
    </main>
  );
}
