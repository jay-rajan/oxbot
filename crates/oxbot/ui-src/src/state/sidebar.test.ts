import { describe, expect, it } from "vitest";
import type { BotInfo, ServerMsg } from "../net/protocol";
import { groupBots, initialSidebar, isUnread, type SidebarState, sidebarReducer } from "./sidebar";
import { type Action, local } from "./store";

const server = (msg: ServerMsg) => ({ kind: "server", msg }) as const;

function bot(id: string, more: Partial<BotInfo> = {}): BotInfo {
  return {
    id,
    name: id,
    busy: false,
    last_seq: 0,
    last_reply_seq: 0,
    browser_state: "off",
    central: false,
    created_by: null,
    private: false,
    can_manage: true,
    locked: false,
    engine: "codex",
    model: null,
    models: [],
    can_task: true,
    instructions: "",
    daily_requests: null,
    sharing: "everyone",
    members: [],
    ...more,
  };
}

const run = (...actions: Action[]): SidebarState => actions.reduce(sidebarReducer, initialSidebar);
const ev = (bot: string, seq: number, kind: "user_message" | "assistant_message" | "turn_done"): ServerMsg =>
  (kind === "user_message"
    ? { type: "event", bot, seq, ts_ms: 0, kind, text: "hi", from: null, files: [] }
    : kind === "assistant_message"
      ? { type: "event", bot, seq, ts_ms: 0, kind, text: "hello" }
      : { type: "event", bot, seq, ts_ms: 0, kind }) as ServerMsg;

describe("groupBots", () => {
  const list = [
    bot("assistant", { name: "Assistant", central: true }),
    bot("recipes", { name: "Recipes", created_by: "Jay" }),
    bot("bank", { name: "Bank", created_by: "Sam" }),
    bot("cafe", { name: "Café", created_by: "Ann" }),
    bot("home", { name: "Home" }),
  ];
  const secs = [{ id: "s1", name: "Kitchen" }];

  it("puts Assistant first, then bots not in a section, then each section", () => {
    const g = groupBots(list, secs, { recipes: "s1" }, "sections", "", "Jay");
    expect(g.map((x) => [x.key, x.ids])).toEqual([
      ["assistant", ["assistant"]],
      ["none", ["bank", "cafe", "home"]],
      ["s:s1", ["recipes"]],
    ]);
  });

  it("ignores a placement in a section that's gone, and keeps empty sections", () => {
    const g = groupBots(list, secs, { recipes: "gone" }, "sections", "", "Jay");
    expect(g.find((x) => x.key === "s:s1")?.ids).toEqual([]);
    expect(g.find((x) => x.key === "none")?.ids).toContain("recipes");
  });

  it("groups by maker: you first, then A-Z, then the home computer", () => {
    const g = groupBots(list, secs, {}, "people", "", "Sam");
    expect(g.map((x) => x.key)).toEqual(["assistant", "p:Sam", "p:Ann", "p:Jay", "p:"]);
    expect(g.at(-1)?.title).toBe("Home computer");
  });

  it("searches ignoring case and accents, and drops empty groups", () => {
    const g = groupBots(list, secs, { recipes: "s1" }, "sections", "CAFE", "Jay");
    expect(g.map((x) => [x.key, x.ids])).toEqual([["none", ["cafe"]]]);
  });
});

describe("sidebar reducer", () => {
  it("keeps who's working from the list and from events", () => {
    let s = run(server({ type: "bots", bots: [bot("a", { busy: true }), bot("b")] }));
    expect(s.busy).toEqual({ a: true, b: false });
    s = sidebarReducer(s, server(ev("b", 1, "user_message")));
    expect(s.busy.b).toBe(true);
    s = sidebarReducer(s, server(ev("b", 2, "turn_done")));
    expect(s.busy.b).toBe(false);
    // An event already seen changes nothing.
    expect(sidebarReducer(s, server(ev("b", 1, "user_message")))).toBe(s);
  });

  it("shows a new reply as unread until the bot is opened", () => {
    let s = run(server({ type: "bots", bots: [bot("a", { last_reply_seq: 3 }), bot("b")] }));
    // Old replies on first sight aren't new.
    expect(isUnread(s, "a")).toBe(false);
    s = sidebarReducer(s, server(ev("a", 4, "assistant_message")));
    expect(isUnread(s, "a")).toBe(true);
    s = sidebarReducer(s, local("ui/select", { bot: "a" }));
    expect(isUnread(s, "a")).toBe(false);
    expect(s.seen.a).toBe(4);
  });

  it("marks the open bot's replies seen only while the page is in view", () => {
    let s = run(server({ type: "bots", bots: [bot("a")] }), local("ui/select", { bot: "a" }), local("sidebar/visible", { visible: false }));
    s = sidebarReducer(s, server(ev("a", 2, "assistant_message")));
    expect(s.seen.a ?? 0).toBe(0);
    s = sidebarReducer(s, local("sidebar/visible", { visible: true }));
    expect(s.seen.a).toBe(2);
  });

  it("forgets a bot that's gone, and its seen mark", () => {
    let s = run(server({ type: "bots", bots: [bot("a"), bot("b")] }), local("ui/select", { bot: "a" }));
    s = sidebarReducer(s, server({ type: "bots", bots: [bot("b")] }));
    expect(s.seen).toEqual({ b: 0 });
    expect(s.current).toBeNull();
    expect(s.botsN).toBe(2);
  });

  it("uses the kept seen marks over first-sight ones", () => {
    const s = run(server({ type: "bots", bots: [bot("a", { last_reply_seq: 5 })] }), local("sidebar/load", { seen: { a: 2 } }));
    expect(isUnread(s, "a")).toBe(true);
  });

  it("counts warnings while Settings is closed, and clears them when it opens", () => {
    const log = (level: string): ServerMsg => ({ type: "log", ts_ms: 0, level, target: "x", message: "m" }) as ServerMsg;
    let s = run(server(log("warn")), server(log("info")), server(log("error")));
    expect(s.warnings).toBe(2);
    s = sidebarReducer(s, local("ui/open", { dialog: "settings" }));
    expect(s.warnings).toBe(0);
    s = sidebarReducer(s, server(log("warn")));
    expect(s.warnings).toBe(0);
    s = sidebarReducer(s, local("ui/close", {}));
    s = sidebarReducer(s, server(log("warn")));
    expect(s.warnings).toBe(1);
  });

  it("folds groups and forgets deleted sections' folds", () => {
    let s = run(local("sidebar/toggle", { key: "s:x" }), local("sidebar/toggle", { key: "p:Jay" }));
    expect(s.collapsed).toEqual(["s:x", "p:Jay"]);
    s = sidebarReducer(s, server({ type: "sections", sections: [], placed: {} }));
    expect(s.collapsed).toEqual(["p:Jay"]);
  });

  it("notes each bot made from this page", () => {
    const s = run(server({ type: "bot_created", ...bot("n") }), server({ type: "bot_created", ...bot("n") }));
    expect(s.created).toEqual({ id: "n", n: 2 });
  });
});
