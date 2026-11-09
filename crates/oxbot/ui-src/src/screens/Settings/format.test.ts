import { describe, expect, it } from "vitest";
import type { SetupReport } from "../../net/protocol";
import { bytes, doorHere, fixParts, missingChecks, money, setupReady, siteRows, swappedText } from "./format";

describe("format", () => {
  it("money and bytes", () => {
    expect(money(0)).toBe("$0.00");
    expect(money(0.004)).toBe("<$0.01");
    expect(money(1.5)).toBe("$1.50");
    expect(bytes(0)).toBe("0 B");
    expect(bytes(500)).toBe("500 B");
    expect(bytes(2048)).toBe("2.0 KB");
    expect(bytes(20480)).toBe("20 KB");
    expect(bytes(3 * 1024 * 1024)).toBe("3.0 MB");
  });

  it("swapped details in words", () => {
    expect(swappedText({})).toBe("");
    expect(swappedText({ email: 2, person: 1, phone: 0 })).toBe("swapped out: 2 emails, 1 name");
    expect(swappedText({ thing: 3 })).toBe("swapped out: 3 things");
  });

  it("setup: ready, and what to list", () => {
    const setup: SetupReport = {
      checks: [
        { id: "chrome", name: "Chrome", ok: true, detail: "found", fix: null, optional: false },
        { id: "mic", name: "Dictation", ok: false, detail: "No microphone", fix: null, optional: true },
      ],
    };
    expect(setupReady(null)).toBe(true);
    expect(setupReady(setup)).toBe(true);
    expect(missingChecks(setup, false)).toEqual([]);
    expect(missingChecks(setup, true).map((c) => c.id)).toEqual(["mic"]);
    const broken = { checks: [...setup.checks, { id: "node", name: "Node", ok: false, detail: "missing", fix: null, optional: false }] };
    expect(setupReady(broken)).toBe(false);
  });

  it("commands in a fix are code", () => {
    expect(fixParts("Run  brew install node  then retry")).toEqual([
      { text: "Run", code: false },
      { text: "brew install node", code: true },
      { text: "then retry", code: false },
    ]);
  });

  it("site rows: home first, fine-grained-only sites, then private ones not at home", () => {
    const s = (site: string) => ({ site, signed_in_ms: 1, icon: null });
    const rows = siteRows([s("a.com")], [s("a.com"), s("b.com"), s("c.com")], ["c.com", "a.com"]);
    expect(rows.map((r) => `${r.site.site}:${r.mine ? "mine" : r.stepOnly ? "step" : "home"}`)).toEqual([
      "a.com:home",
      "c.com:step",
      "b.com:mine",
      "c.com:mine",
    ]);
  });

  it("the door: here and listed", () => {
    const net = { id: "x", label: "Home", here: true };
    expect(doorHere(null)).toEqual({ here: null, listed: false });
    expect(doorHere({ open: true, why: "", networks: [], current: net, name: "" }).listed).toBe(false);
    expect(doorHere({ open: true, why: "", networks: [net], current: net, name: "" }).listed).toBe(true);
  });
});
