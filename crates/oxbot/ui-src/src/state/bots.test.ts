import { describe, expect, it } from "vitest";
import type { BotInfo, ServerMsg } from "../net/protocol";
import { botList, botsReducer, initialBots } from "./bots";

const server = (msg: ServerMsg) => ({ kind: "server", msg }) as const;

function bot(id: string, name = id): BotInfo {
  return {
    id,
    name,
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
  };
}

describe("bots reducer", () => {
  it("lists the bots in the server's order", () => {
    const next = botsReducer(
      initialBots,
      server({ type: "bots", bots: [bot("assistant", "Assistant"), bot("b2", "Mail")] }),
    );
    expect(botList(next).map((b) => b.name)).toEqual(["Assistant", "Mail"]);
  });

  it("drops a bot that's no longer listed", () => {
    const two = botsReducer(initialBots, server({ type: "bots", bots: [bot("a"), bot("b")] }));
    const one = botsReducer(two, server({ type: "bots", bots: [bot("b")] }));
    expect(one.order).toEqual(["b"]);
    expect(one.byId).not.toHaveProperty("a");
  });

  it("adds a bot just made, and remembers it to open", () => {
    const one = botsReducer(initialBots, server({ type: "bots", bots: [bot("a")] }));
    const next = botsReducer(one, server({ type: "bot_created", ...bot("b", "New") }));
    expect(next.order).toEqual(["a", "b"]);
    expect(next.byId["b"]?.name).toBe("New");
    expect(next.byId["b"]).not.toHaveProperty("type");
    expect(next.justCreated).toBe("b");
  });

  it("doesn't list a bot twice when the full list follows", () => {
    const created = botsReducer(initialBots, server({ type: "bot_created", ...bot("a") }));
    const listed = botsReducer(created, server({ type: "bots", bots: [bot("a")] }));
    expect(listed.order).toEqual(["a"]);
  });

  it("ignores other messages and the socket", () => {
    expect(botsReducer(initialBots, { kind: "status", status: "open" })).toBe(initialBots);
  });
});
