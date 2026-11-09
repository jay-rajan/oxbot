import { describe, expect, it } from "vitest";
import { takeKey } from "./address";

const store = (items: Record<string, string>) => ({
  getItem: (k: string) => items[k] ?? null,
  setItem: (k: string, v: string) => {
    items[k] = v;
  },
});
const key = "a".repeat(64);

describe("takeKey", () => {
  it("takes the key from the address and keeps it", () => {
    const items: Record<string, string> = {};
    let url = "";
    expect(takeKey({ hash: `#k=${key}`, pathname: "/", search: "" } as Location, store(items), (u) => (url = u))).toBe(key);
    expect(items.oxbotHomeComputerKey).toBe(key);
    expect(url).toBe("/");
  });

  it("a tab opened before the rename still finds its key", () => {
    expect(takeKey({ hash: "", pathname: "/", search: "" } as Location, store({ oxbotFarmKey: key }), () => {})).toBe(key);
  });
});
