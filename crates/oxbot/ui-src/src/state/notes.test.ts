import { describe, expect, it } from "vitest";
import type { HomeNoteInfo } from "../net/protocol";
import { byYearTopic, search, topics } from "./notes";

function note(id: number, title: string, day: string, more: Partial<HomeNoteInfo> = {}): HomeNoteInfo {
  return {
    id, title, day, day_from: "upload", topic: "", tags: [], people: [], summary: "", file: "", name: `${id}.pdf`,
    kind: "pdf", size: 1, private: true, uploaded_ms: 0, uploaded_by: "", ...more,
  };
}

const notes = [
  note(1, "Blood test", "2026-03-02", { topic: "Medical", people: ["Akhila"] }),
  note(2, "Car insurance", "2025-06-10", { topic: "Insurance", tags: ["renewal"] }),
  note(3, "Blood tests again", "2026-10-01", { topic: "Medical" }),
  note(4, "Receipt", "2026-11-17"),
];

describe("records", () => {
  it("finds notes with every word, latest first", () => {
    expect(search(notes, "blood tests").map((n) => n.id)).toEqual([3, 1]);
    expect(search(notes, "akhila's blood").map((n) => n.id)).toEqual([1]);
    expect(search(notes, "insurance 2025").map((n) => n.id)).toEqual([2]);
    expect(search(notes, "march").map((n) => n.id)).toEqual([1]);
    expect(search(notes, "", "Unsorted").map((n) => n.id)).toEqual([4]);
  });

  it("groups by year, then topic with Unsorted last", () => {
    expect(topics(notes)).toEqual(["Insurance", "Medical", "Unsorted"]);
    const g = byYearTopic(search(notes, ""));
    expect(g.map(([y, ts]) => [y, ts.map(([t, l]) => [t, l.map((n) => n.id)])])).toEqual([
      ["2026", [["Medical", [3, 1]], ["Unsorted", [4]]]],
      ["2025", [["Insurance", [2]]]],
    ]);
  });
});
