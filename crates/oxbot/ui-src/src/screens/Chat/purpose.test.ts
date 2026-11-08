import { describe, expect, it } from "vitest";
import { purposeLine } from "./purpose";

describe("the line under a bot's name says what it's for", () => {
  it("is the first sentence", () => {
    expect(purposeLine("Find dinner recipes when asked. Prefer vegetarian.")).toBe("Find dinner recipes when asked.");
  });
  it("skips headings, bullets and blank lines", () => {
    expect(purposeLine("\n# Groceries\n- Keep our weekly list.")).toBe("Groceries");
    expect(purposeLine("\n\n- Keep our weekly list.")).toBe("Keep our weekly list.");
  });
  it("cuts a long sentence at a word", () => {
    const line = purposeLine(
      "Every morning when I ask, read the top stories on news.ycombinator.com and give me five lines.",
    );
    expect(line.endsWith("…")).toBe(true);
    expect(line.length).toBeLessThanOrEqual(61);
    expect(line).toBe("Every morning when I ask, read the top stories on…");
  });
  it("is empty without instructions", () => {
    expect(purposeLine("")).toBe("");
    expect(purposeLine(null)).toBe("");
  });
});
