import { describe, expect, it } from "vitest";
import { stamp } from "./Log";

describe("a message's time", () => {
  const now = new Date(2026, 10, 17, 16, 30);
  it("is the time today, the day too before, and the year in another year", () => {
    expect(stamp(new Date(2026, 10, 17, 9, 5).getTime(), now)).not.toMatch(/Nov/);
    expect(stamp(new Date(2026, 10, 16, 21, 0).getTime(), now)).toMatch(/^Nov 16, /);
    expect(stamp(new Date(2025, 0, 2, 8, 0).getTime(), now)).toMatch(/2025, /);
  });
});
