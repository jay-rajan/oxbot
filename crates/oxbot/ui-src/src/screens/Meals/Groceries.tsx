// The grocery list by aisle: a tap ticks an item off (it stays, crossed
// out, until "Clear got"); × takes it off. Add an item at the top.
import { type FormEvent, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Input, Select } from "../../components/ui";
import type { GroceryInfo } from "../../net/protocol";
import { byAisle } from "../../state/meals";

const AISLES = ["Produce", "Dairy", "Meat & fish", "Bakery", "Pantry", "Frozen", "Drinks", "Household", "Other"];

export function Groceries() {
  const { send } = useApp();
  const items = useSel((s) => s.meals.groceries);
  const loaded = useSel((s) => s.meals.loaded);
  const [name, setName] = useState("");
  const [qty, setQty] = useState("");
  const [aisle, setAisle] = useState("");
  const { aisles, got } = byAisle(items);

  const add = (ev: FormEvent) => {
    ev.preventDefault();
    if (!name.trim()) return;
    send({ type: "add_grocery", item: { name: name.trim(), qty: qty.trim(), aisle: aisle === "Other" ? "" : aisle } });
    setName("");
    setQty("");
  };

  return (
    <div className="groceries">
      <form className="grocery-add" onSubmit={add}>
        <Input id="groceryName" value={name} maxLength={120} placeholder="Add an item: milk" aria-label="Item" onChange={(x) => setName(x.target.value)} />
        <Input id="groceryQty" className="grocery-qty" value={qty} maxLength={30} placeholder="How much" aria-label="How much" onChange={(x) => setQty(x.target.value)} />
        <Select aria-label="Aisle" value={aisle} onChange={(x) => setAisle(x.target.value)}>
          <option value="">Aisle</option>
          {AISLES.map((a) => (
            <option key={a} value={a}>
              {a}
            </option>
          ))}
        </Select>
        <Button kind="primary" type="submit" id="groceryAdd" disabled={!name.trim()}>
          Add
        </Button>
      </form>
      {!loaded ? null : !items.length ? (
        <p className="cal-empty">Nothing on the list. Add items here or tell the Groceries bot; the Meal Plan bot adds what its meals need.</p>
      ) : (
        <>
          {got.length ? (
            <div className="grocery-got">
              <span>{got.length} got</span>
              <Button kind="quiet" size="sm" id="groceryClear" onClick={() => send({ type: "clear_groceries" })}>
                Clear got
              </Button>
            </div>
          ) : null}
          {aisles.map(([a, list]) => (
            <section key={a} className="grocery-aisle" aria-label={a}>
              <h3>{a}</h3>
              <ul>{list.map((g) => <Item key={g.id} g={g} />)}</ul>
            </section>
          ))}
        </>
      )}
    </div>
  );
}

function Item({ g }: { g: GroceryInfo }) {
  const { send } = useApp();
  return (
    <li className={`grocery${g.done ? " got" : ""}`}>
      <label>
        <input type="checkbox" checked={g.done} onChange={() => send({ type: "set_grocery", id: g.id, done: !g.done })} />
        <span className="grocery-name">{g.item.name}</span>
        {g.item.qty ? <span className="grocery-meta">{g.item.qty}</span> : null}
        {g.for_meal ? <span className="grocery-meta">for {g.for_meal}</span> : null}
      </label>
      <button type="button" className="grocery-x" aria-label={`Remove ${g.item.name}`} title="Take it off the list" onClick={() => send({ type: "remove_grocery", id: g.id })}>
        ×
      </button>
    </li>
  );
}
