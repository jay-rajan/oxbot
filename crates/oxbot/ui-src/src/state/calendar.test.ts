import { describe, expect, it } from "vitest";
import type { HomeEventInfo } from "../net/protocol";
import { addDays, addMonths, occurrences, rangeOf, step, weekday, weekStart } from "./calendar";

function ev(id: number, starts: string, more: Partial<HomeEventInfo["event"]> = {}): HomeEventInfo {
  return {
    id,
    event: { title: `e${id}`, starts, ends: null, place: "", people: [], notes: "", repeat: null, remind_min: null, ...more },
    all_day: !starts.includes("T"),
    source: "",
    made_by: "Maya",
    changed_by: "Maya",
    updated_ms: 0,
    deleted_ms: null,
  };
}

const days = (m: Map<string, unknown[]>) => [...m.keys()].sort();

describe("days", () => {
  it("counts weeks from Monday and months by their last day", () => {
    expect(weekday("2026-11-16")).toBe(0);
    expect(weekday("2026-11-22")).toBe(6);
    expect(weekStart("2026-11-20")).toBe("2026-11-16");
    expect(addDays("2026-12-31", 1)).toBe("2027-01-01");
    expect(addMonths("2027-01-31", 1)).toBe("2027-02-28");
    expect(addMonths("2026-11-16", -11)).toBe("2025-12-16");
  });

  it("gives each view its days, and steps by them", () => {
    expect(rangeOf("week", "2026-11-19")).toEqual(["2026-11-16", "2026-11-22"]);
    // November 2026 starts on a Sunday and ends on a Monday: whole weeks.
    expect(rangeOf("month", "2026-11-19")).toEqual(["2026-10-26", "2026-12-06"]);
    expect(step("month", "2026-11-19", 1)).toBe("2026-12-01");
    expect(step("week", "2026-11-19", -1)).toBe("2026-11-12");
  });
});

describe("occurrences", () => {
  it("puts timed events on their day, all-day ones first", () => {
    const m = occurrences(
      [ev(1, "2026-11-17T17:30", { ends: "2026-11-17T18:30" }), ev(2, "2026-11-17"), ev(3, "2026-12-01")],
      "2026-11-16",
      "2026-11-22",
    );
    expect(days(m)).toEqual(["2026-11-17"]);
    const tue = m.get("2026-11-17") ?? [];
    expect(tue.map((o) => o.ev.id)).toEqual([2, 1]);
    expect(tue[1]?.time).toBe("17:30");
    expect(tue[1]?.endTime).toBe("18:30");
  });

  it("repeats weekly from long ago, and stops on its last day", () => {
    const m = occurrences(
      [ev(1, "2025-01-07T17:30", { repeat: { every: "week", until: "2026-11-24" } })],
      "2026-11-16",
      "2026-11-30",
    );
    expect(days(m)).toEqual(["2026-11-17", "2026-11-24"]);
  });

  it("repeats monthly on its date, skipping months without it", () => {
    const m = occurrences([ev(1, "2026-10-31", { repeat: { every: "month", until: null } })], "2026-10-01", "2027-01-31");
    expect(days(m)).toEqual(["2026-10-31", "2026-12-31", "2027-01-31"]);
    const y = occurrences([ev(2, "2020-02-14", { repeat: { every: "year", until: null } })], "2026-01-01", "2026-12-31");
    expect(days(y)).toEqual(["2026-02-14"]);
  });

  it("shows a long event on each day it covers, its time on the first", () => {
    const m = occurrences([ev(1, "2026-11-20T09:00", { ends: "2026-11-23T12:00" })], "2026-11-16", "2026-11-22");
    expect(days(m)).toEqual(["2026-11-20", "2026-11-21", "2026-11-22"]);
    expect(m.get("2026-11-20")?.[0]?.time).toBe("09:00");
    expect(m.get("2026-11-21")?.[0]?.time).toBeNull();
    // One that started before the range still shows.
    const later = occurrences([ev(1, "2026-11-20", { ends: "2026-11-24" })], "2026-11-23", "2026-11-29");
    expect(days(later)).toEqual(["2026-11-23", "2026-11-24"]);
  });
});
