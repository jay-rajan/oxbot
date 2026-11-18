import { describe, expect, it } from "vitest";
import type { BotEvent } from "../../net/protocol";
import { doneLine, foldSteps, statusLine } from "./steps";

let seq = 0;
const step = (name: string, summary = "") => ({ seq: ++seq, ts_ms: 0, kind: "tool_use", name, summary }) as unknown as BotEvent;
const said = (text: string) => ({ seq: ++seq, ts_ms: 0, kind: "assistant_message", text }) as unknown as BotEvent;

describe("a bot's steps", () => {
  it("fold into one line naming the site, not the actions", () => {
    const events = [step("browser navigate_page", "https://www.walmart.ca/en/search?q=lime"), step("page search"), step("browser click"), step("browser fill"), said("Added."), step("grocery list")];
    const f = foldSteps(events);
    expect(f.runs.size).toBe(2);
    expect(doneLine(f.runs.get(events[0]!.seq)!)).toBe("Browsed walmart.ca");
    expect(doneLine(f.runs.get(events[5]!.seq)!)).toBeNull();
  });

  it("keep the site the bot is on into later turns", () => {
    const f = foldSteps([step("browser navigate_page", "https://www.walmart.ca/"), said("ok"), step("browser click"), step("browser press_key")]);
    expect(statusLine("Using browser: click…", f.tail, f.site)).toBe("On walmart.ca · browsing");
    expect(statusLine("Thinking…", f.tail, f.site)).toBe("On walmart.ca · browsing");
  });

  it("never name a step in the status line", () => {
    expect(statusLine("Using browser: navigate_page…", null, null)).toBe("Browsing");
    expect(statusLine("Updating its to-do list…", null, null)).toBe("Working…");
    expect(statusLine("Asking you to sign in…", null, "walmart.ca")).toBe("Asking you to sign in…");
    expect(statusLine("Thinking…", null, null)).toBe("Thinking…");
  });
});
