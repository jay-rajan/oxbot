import { describe, expect, it } from "vitest";
import type { NetRow, NetTotals, ServerMsg } from "../net/protocol";
import { activityReducer, initialActivity } from "./activity";
import { local } from "./store";

const server = (msg: ServerMsg) => ({ kind: "server", msg }) as const;
const totals: NetTotals = { today_rows: 1, today_up: 10, today_down: 20, today_cost_usd: 0 };
function row(id: number): NetRow {
  return { id, ts_ms: id, kind: "ai", who: "Recipes", bot: null, person: null, host: "openrouter.ai", label: "", bytes_up: 1, bytes_down: 2, cost_usd: null, swapped: {}, ms: 100, outcome: "ok", other_hosts: 0 };
}

describe("activity", () => {
  it("opens on a named tab, else where it was left", () => {
    let s = activityReducer(initialActivity, local("ui/open", { dialog: "settings", arg: "setSignins" }));
    expect(s.tab).toBe("setSignins");
    s = activityReducer(s, local("ui/open", { dialog: "settings" }));
    expect(s.tab).toBe("setSignins");
    s = activityReducer(s, local("ui/open", { dialog: "report", arg: "setPeople" }));
    expect(s.tab).toBe("setSignins");
    s = activityReducer(s, local("activity/tab", { tab: "nope" }));
    expect(s.tab).toBe("setSignins");
  });

  it("addDevice opens People with the form, and a code replaces the form", () => {
    let s = activityReducer({ ...initialActivity, pairWhy: "x" }, local("ui/open", { dialog: "settings", arg: "addDevice" }));
    expect(s).toMatchObject({ tab: "setPeople", pairFormOpen: true, pairWhy: null });
    s = activityReducer(s, server({ type: "pairing", person: "Jay", code: "AAAA-BBBB", link: "", qr_svg: "", expires_ms: 0 }));
    expect(s.pairFormOpen).toBe(false);
  });

  it("toggles a person's row", () => {
    let s = activityReducer(initialActivity, local("activity/person", { name: "Jay" }));
    expect(s.personOpen).toBe("Jay");
    s = activityReducer(s, local("activity/person", { name: "Jay" }));
    expect(s.personOpen).toBeNull();
  });

  it("keeps the net log: a fresh page replaces, an older one appends", () => {
    let s = activityReducer(initialActivity, server({ type: "net_log", rows: [row(3), row(2)], totals, more: true }));
    expect(s.net.rows.map((r) => r.id)).toEqual([3, 2]);
    s = activityReducer(s, local("activity/netOlder"));
    s = activityReducer(s, server({ type: "net_log", rows: [row(1)], totals, more: false }));
    expect(s.net.rows.map((r) => r.id)).toEqual([3, 2, 1]);
    expect(s.net.more).toBe(false);
    expect(s.net.askingOlder).toBe(false);
    s = activityReducer(s, server({ type: "net_log", rows: [row(4)], totals, more: false }));
    expect(s.net.rows.map((r) => r.id)).toEqual([4]);
  });

  it("a chip starts over; a row opens and closes", () => {
    let s = activityReducer(initialActivity, server({ type: "net_log", rows: [row(1)], totals, more: false }));
    s = activityReducer(s, local("activity/netToggle", { id: 1 }));
    expect(s.net.open).toBe(1);
    s = activityReducer(s, local("activity/netKind", { netKind: "ai" }));
    expect(s.net).toMatchObject({ kind: "ai", rows: [], totals: null, open: null });
  });

  it("ignores what isn't for it", () => {
    expect(activityReducer(initialActivity, local("ui/select", { bot: "x" }))).toBe(initialActivity);
    expect(activityReducer(initialActivity, server({ type: "profiles_changed" }))).toBe(initialActivity);
  });
});
