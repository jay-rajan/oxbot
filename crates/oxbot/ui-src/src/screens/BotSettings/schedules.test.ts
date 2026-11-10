import { describe, expect, it } from "vitest";
import type { ScheduleInfo } from "../../net/protocol";
import { lastResult, scheduleMeta } from "./schedules";

const base: ScheduleInfo = {
  id: 1,
  bot: "recipes",
  bot_name: "Recipes",
  person: "Jay",
  task: "plan the week",
  rule: { kind: "weekly", days: ["sat"], time: "08:00" },
  text: "Every Saturday at 8 AM: plan the week",
  tz: "UTC",
  next_run_ms: null,
  last_run_ms: null,
  last_outcome: null,
  last_late: false,
  paused: false,
  can_change: true,
  mine: true,
  created_ms: 0,
};

describe("schedule rows", () => {
  it("say how the last run went in a word", () => {
    expect(lastResult(base)).toBeNull();
    expect(lastResult({ ...base, last_outcome: "ok" })).toEqual({ word: "ran", tone: "ok" });
    expect(lastResult({ ...base, last_outcome: "ok", last_late: true })?.word).toBe("late");
    expect(lastResult({ ...base, last_outcome: "missed" })?.word).toBe("missed");
    expect(lastResult({ ...base, last_outcome: "failed" })?.tone).toBe("danger");
    expect(lastResult({ ...base, last_outcome: "busy" })).toEqual({ word: "skipped", tone: "warn", why: "the bot was busy" });
  });

  it("name the bot when listing every schedule", () => {
    expect(scheduleMeta({ ...base, paused: true }, true)).toBe("Recipes · paused");
    expect(scheduleMeta({ ...base, mine: false, next_run_ms: Date.UTC(2026, 10, 14, 8) }, false)).toMatch(/^as Jay · next Sat, Nov 14/);
  });
});
