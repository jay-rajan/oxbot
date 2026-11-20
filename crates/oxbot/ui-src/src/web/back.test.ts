import { beforeEach, describe, expect, it } from "vitest";

// A stand-in for the browser's history: Back fires popstate later, as a
// browser does.
const win = new EventTarget();
let entries = 1;
const back = () => {
  entries -= 1;
  setTimeout(() => win.dispatchEvent(new Event("popstate")), 0);
};
Object.assign(globalThis, {
  window: win,
  history: { state: null, pushState: () => (entries += 1), back },
});
const { addLayer, resetLayers } = await import("./back");
const settle = () => new Promise((r) => setTimeout(r, 5));

describe("Back closes the top window", () => {
  beforeEach(() => {
    resetLayers();
    entries = 1;
  });

  it("closes the newest window first, then the one under it", async () => {
    const closed: string[] = [];
    addLayer(() => closed.push("drawer"));
    addLayer(() => closed.push("settings"));
    expect(entries).toBe(3);
    back();
    await settle();
    expect(closed).toEqual(["settings"]);
    back();
    await settle();
    expect(closed).toEqual(["settings", "drawer"]);
    expect(entries).toBe(1);
  });

  it("a window closed by its own button takes its entry out without closing another", async () => {
    const closed: string[] = [];
    addLayer(() => closed.push("drawer"));
    const closeSettings = addLayer(() => closed.push("settings"));
    closeSettings();
    await settle();
    expect(closed).toEqual([]);
    expect(entries).toBe(2);
    // Back now closes the drawer, and calling close again does nothing.
    back();
    await settle();
    expect(closed).toEqual(["drawer"]);
    closeSettings();
    await settle();
    expect(entries).toBe(1);
  });

  it("a window Back closed doesn't step back again when the page tidies up", async () => {
    const closed: string[] = [];
    const tidy = addLayer(() => closed.push("settings"));
    back();
    await settle();
    tidy();
    await settle();
    expect(closed).toEqual(["settings"]);
    expect(entries).toBe(1);
  });
});
