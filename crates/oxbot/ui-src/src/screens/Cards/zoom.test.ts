import { describe, expect, it } from "vitest";
import { FLAT, panFrom, toggleAt, toPage, ZOOM } from "./zoom";

// A 400×300 box showing a 1280×960 page.
const W = 400;
const H = 300;

describe("the sign-in picture's zoom", () => {
  it("a tap with no zoom lands on the same spot of the page", () => {
    expect(toPage(FLAT, 200, 150, W, H, 1280, 960)).toEqual({ x: 640, y: 480 });
  });

  it("double tap zooms in on that spot, and a tap there still hits it", () => {
    const v = toggleAt(FLAT, 100, 100, W, H);
    expect(v.s).toBe(ZOOM);
    // The spot under the finger didn't move.
    expect(toPage(v, 100, 100, W, H, 1280, 960)).toEqual({ x: 320, y: 320 });
    // Another double tap zooms back out.
    expect(toggleAt(v, 50, 50, W, H)).toEqual(FLAT);
  });

  it("zooming near an edge never shows empty space", () => {
    const v = toggleAt(FLAT, 0, 0, W, H);
    expect(v).toEqual({ s: ZOOM, x: 0, y: 0 });
    const far = toggleAt(FLAT, W, H, W, H);
    expect(far.x).toBe(W - ZOOM * W);
    expect(far.y).toBe(H - ZOOM * H);
  });

  it("dragging moves the zoomed picture, but not past its edges", () => {
    const v = toggleAt(FLAT, 200, 150, W, H);
    const moved = panFrom(v, -40, 0, W, H);
    expect(moved.x).toBe(v.x - 40);
    expect(panFrom(v, 10_000, 10_000, W, H)).toEqual({ s: ZOOM, x: 0, y: 0 });
    // A tap after moving still hits the page where it shows.
    const hit = toPage(moved, 200, 150, W, H, 1280, 960);
    expect(hit.x).toBeGreaterThan(640);
  });
});
