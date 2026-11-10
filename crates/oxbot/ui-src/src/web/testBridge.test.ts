import { describe, expect, it } from "vitest";
import { createTestBridge } from "./testBridge";

describe("the tests' names on the page", () => {
  it("stay hidden until the server says it's in test mode", () => {
    const page: Record<string, unknown> = {};
    const bridge = createTestBridge(page);
    bridge.expose("send", () => "sent");
    expect("send" in page).toBe(false);
    bridge.enable();
    expect(page.send).toBe("sent");
    // Names a screen adds later appear at once, the newest winning.
    bridge.expose("bots", () => 1);
    bridge.expose("bots", () => 2);
    expect(page.bots).toBe(2);
  });

  it("never appear without test mode", () => {
    const page: Record<string, unknown> = {};
    const bridge = createTestBridge(page);
    bridge.expose("approvals", () => new Map());
    bridge.expose("door", () => null);
    expect(Object.keys(page)).toEqual([]);
  });
});
