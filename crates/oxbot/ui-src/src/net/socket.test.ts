import { describe, expect, it } from "vitest";
import { backoffMs, socketUrl } from "./socket";

describe("socket address", () => {
  it("carries the key", () => {
    expect(socketUrl({ host: "127.0.0.1:7878", secure: false, key: "ab" })).toBe(
      "ws://127.0.0.1:7878/ws?k=ab",
    );
  });
  it("uses the test person instead of the key, and wss on https", () => {
    expect(socketUrl({ host: "h", secure: true, key: "ab", as: "Sam" })).toBe("wss://h/ws?as=Sam");
  });
  it("has no query without either", () => {
    expect(socketUrl({ host: "h", secure: false, key: "" })).toBe("ws://h/ws");
  });
  it("backs off up to 10 s (5 s on a phone)", () => {
    expect([0, 1, 2, 5].map((r) => backoffMs(r, false))).toEqual([500, 1000, 2000, 10000]);
    expect(backoffMs(9, true)).toBe(5000);
  });
});
