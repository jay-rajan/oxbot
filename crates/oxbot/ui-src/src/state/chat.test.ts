import { describe, expect, it } from "vitest";
import type { BotEvent, BotInfo, ServerMsg } from "../net/protocol";
import { type ChatState, KEEP_PAGES, PAGE, chatReducer, initialChat, uploading } from "./chat";
import { type Action, local } from "./store";

const server = (msg: ServerMsg): Action => ({ kind: "server", msg });
const run = (actions: Action[], state: ChatState = initialChat) => actions.reduce(chatReducer, state);

function ev(seq: number, kind: "user_message" | "assistant_message" | "turn_done" | "tool_use" = "tool_use", extra: Record<string, unknown> = {}): BotEvent {
  const base = { bot: "b", seq, ts_ms: seq };
  if (kind === "user_message") return { ...base, kind, text: `u${seq}`, from: null, files: [], ...extra } as BotEvent;
  if (kind === "assistant_message") return { ...base, kind, text: `a${seq}` } as BotEvent;
  if (kind === "turn_done") return { ...base, kind, ok: true, duration_ms: 1000 } as BotEvent;
  return { ...base, kind, name: "Read", summary: String(seq) } as BotEvent;
}
const event = (e: BotEvent): Action => server({ type: "event", ...e } as ServerMsg);
const history = (events: BotEvent[], first_seq: number, has_older: boolean): Action =>
  server({ type: "history", bot: "b", events, first_seq, has_older });
const range = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, i) => ev(from + i));

function info(id: string, extra: Partial<BotInfo> = {}): BotInfo {
  return { id, name: id, busy: false, last_seq: 0, last_reply_seq: 0, central: false, locked: false, ...extra } as BotInfo;
}

describe("chat", () => {
  it("takes live events once, in order, and tracks busy and the stream", () => {
    let s = run([event(ev(5, "user_message")), event(ev(5)), server({ type: "delta", bot: "b", text: "Hi" }), server({ type: "delta", bot: "b", text: " there" })]);
    const b = s.bots.b!;
    expect(b.events.map((e) => e.seq)).toEqual([5]);
    expect(b.firstSeq).toBe(5);
    expect(b.hasOlder).toBe(true);
    expect(b.busy).toBe(true);
    expect(b.activity).toBe("Working…");
    expect(b.stream).toBe("Hi there");
    s = run([event(ev(6, "assistant_message")), event(ev(7, "turn_done"))], s);
    expect(s.bots.b!.stream).toBeNull();
    expect(s.bots.b!.busy).toBe(false);
  });

  it("a new request drops text streamed before it", () => {
    const s = run([server({ type: "delta", bot: "b", text: "x" }), server({ type: "activity", bot: "b", text: "Working…" })]);
    expect(s.bots.b!.stream).toBeNull();
    expect(s.bots.b!.activity).toBe("Working…");
  });

  it("opens with the latest page, then pages back", () => {
    let s = run([history(range(301, 500), 301, true)]);
    expect(s.bots.b!.events.length).toBe(200);
    expect(s.bots.b!.firstSeq).toBe(301);
    s = run([local("chat/olderAsked", { bot: "b" }), history(range(101, 300), 101, true)], s);
    expect(s.bots.b!.events[0]!.seq).toBe(101);
    expect(s.bots.b!.events.length).toBe(400);
    expect(s.bots.b!.loadingOlder).toBe(false);
    expect(s.bots.b!.olderAsks).toEqual([]);
  });

  it("an older page nobody asked for is ignored", () => {
    let s = run([history(range(301, 500), 301, true)]);
    s = run([history(range(101, 300), 101, true)], s);
    expect(s.bots.b!.events[0]!.seq).toBe(301);
  });

  it("an older page asked before the chat was trimmed is dropped", () => {
    let s = run([local("ui/select", { bot: "b" })]);
    // Pages back to 1000 events, then asks once more.
    s = run([history(range(1001, 1200), 1001, true)], s);
    for (let first = 801; first >= 201; first -= 200) s = run([local("chat/olderAsked", { bot: "b" }), history(range(first, first + 199), first, true)], s);
    expect(s.bots.b!.events.length).toBe(1000);
    s = run([local("chat/olderAsked", { bot: "b" })], s);
    // Leaving the chat trims it; the late page is then dropped.
    s = run([local("ui/select", { bot: "other" })], s);
    expect(s.bots.b!.events.length).toBe(KEEP_PAGES * PAGE);
    expect(s.bots.b!.hasOlder).toBe(true);
    s = run([history(range(1, 200), 1, false)], s);
    expect(s.bots.b!.events.length).toBe(KEEP_PAGES * PAGE);
    expect(s.bots.b!.events[0]!.seq).toBe(601);
  });

  it("an empty older page still moves back", () => {
    let s = run([history(range(301, 500), 301, true), local("chat/olderAsked", { bot: "b" })]);
    s = run([history([], 150, true)], s);
    expect(s.bots.b!.firstSeq).toBe(150);
    expect(s.bots.b!.hasOlder).toBe(true);
  });

  it("catching up after a gap starts a fresh window", () => {
    let s = run([history(range(1, 10), 1, false)]);
    s = run([history(range(500, 520), 500, true)], s);
    expect(s.bots.b!.events[0]!.seq).toBe(500);
    expect(s.bots.b!.lastSeq).toBe(520);
  });

  it("a reconnect voids pending older asks of followed chats", () => {
    let s = run([local("chat/subscribed", { bot: "b" }), local("chat/olderAsked", { bot: "b" })]);
    s = run([{ kind: "status", status: "open" }], s);
    expect(s.bots.b!.loadingOlder).toBe(false);
    expect(s.bots.b!.olderAsks).toEqual([]);
  });

  it("files: chips go sending, come back on an error with the words, and leave once sent", () => {
    let s = run([
      local("ui/select", { bot: "b" }),
      local("chat/addChip", { chip: { bot: "b", name: "a.csv", size: 10, state: "uploading", progress: 0, att: null, error: "" } }),
    ]);
    expect(uploading(s, "b")).toBe(true);
    const att = { id: "f1", name: "a.csv", size: 10, kind: "text", path: "", readable: null, frames: 0, note: null };
    s = run([local("chat/chip", { bot: "b", key: 1, patch: { state: "ready", att } }), local("chat/input", { text: "hi" }), local("chat/sent", { bot: "b", text: "hi" })], s);
    expect(s.drafts.b![0]!.state).toBe("sending");
    expect(s.input).toBe("");
    s = run([server({ type: "error", message: "busy" })], s);
    expect(s.drafts.b![0]!.state).toBe("ready");
    expect(s.input).toBe("hi");
    s = run([event(ev(1, "user_message", { files: [att] }))], s);
    expect(s.drafts.b).toEqual([]);
  });

  it("dictation fills the box around the cursor", () => {
    let s = run([local("chat/dictating", { at: { before: "Say ", after: "!" } }), server({ type: "dictation", text: "hi", done: false, error: null })]);
    expect(s.input).toBe("Say hi!");
    expect(s.caret?.at).toBe(6);
    s = run([server({ type: "dictation", text: "hello", done: true, error: "Mic off" })], s);
    expect(s.input).toBe("Say hello!");
    expect(s.dictating).toBeNull();
    expect(s.notice?.text).toBe("Mic off");
  });

  it("reports: kept, opened when asked, forgotten with their bot", () => {
    const report = (id: string): Action => server({ type: "report", bot: "b", id, title: "T", created_ms: 1, html: "<p>x</p>" });
    let s = run([local("chat/wantReport", { key: "b:r1" }), report("r1")]);
    expect(s.reportReady).toBe("b:r1");
    expect(s.wantReport).toBeNull();
    s = run([server({ type: "bots", bots: [info("b", { locked: true })] })], s);
    expect(s.reports["b:r1"]).toBeUndefined();
    s = initialChat;
    for (let i = 0; i < 25; i++) s = run([report(`r${i}`)], s);
    expect(Object.keys(s.reports).length).toBe(20);
    expect(s.reports["b:r0"]).toBeUndefined();
  });

  it("meters show only in the open chat", () => {
    let s = run([local("ui/select", { bot: "b" }), event(ev(1, "turn_done")), server({ type: "meter", bot: "b", model: "x/m", requests: 2, cost_usd: 0 })]);
    expect(s.bots.b!.meters).toEqual([{ after: 1, meter: { model: "x/m", requests: 2, cost_usd: 0 } }]);
    s = run([server({ type: "meter", bot: "c", model: "x/m", requests: 2, cost_usd: 0 })], s);
    expect(s.bots.c).toBeUndefined();
  });

  it("a bot that's gone leaves", () => {
    let s = run([local("ui/select", { bot: "b" }), event(ev(1))]);
    s = run([server({ type: "bots", bots: [info("c", { busy: true })] })], s);
    expect(s.bots.b).toBeUndefined();
    expect(s.current).toBeNull();
    expect(s.bots.c!.busy).toBe(true);
  });

  it("a lost connection marks the offline bar and stops dictating", () => {
    const s = run([local("chat/dictating", { at: { before: "", after: "" } }), { kind: "status", status: "closed" }]);
    expect(s.failedOnce).toBe(true);
    expect(s.dictating).toBeNull();
  });
});
