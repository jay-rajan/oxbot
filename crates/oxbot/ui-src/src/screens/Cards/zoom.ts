// The live sign-in picture's zoom: a scale and where the picture sits in
// its box (translate, in box pixels). Double-tap zooms in on a spot, drag
// moves around, and a tap anywhere is turned back into the page's own
// pixels.

export type View = { s: number; x: number; y: number };

export const FLAT: View = { s: 1, x: 0, y: 0 };
/** How far a double tap zooms in. */
export const ZOOM = 2.5;

/** The picture can't be dragged off its box: no empty edges. */
function clamp(v: View, w: number, h: number): View {
  const x = Math.min(0, Math.max(w - v.s * w, v.x));
  const y = Math.min(0, Math.max(h - v.s * h, v.y));
  return { s: v.s, x, y };
}

/** Double tap at (px, py) in a w×h box: zoom in there, or back out. */
export function toggleAt(v: View, px: number, py: number, w: number, h: number): View {
  if (v.s > 1) return FLAT;
  // The spot under the finger stays under it.
  return clamp({ s: ZOOM, x: px - ZOOM * px, y: py - ZOOM * py }, w, h);
}

/** Dragged by (dx, dy) from where a drag started. */
export function panFrom(start: View, dx: number, dy: number, w: number, h: number): View {
  return clamp({ s: start.s, x: start.x + dx, y: start.y + dy }, w, h);
}

/** A tap at (px, py) in the w×h box, as the page's pixels (pageW×pageH). */
export function toPage(v: View, px: number, py: number, w: number, h: number, pageW: number, pageH: number) {
  const ix = (px - v.x) / v.s;
  const iy = (py - v.y) / v.s;
  return { x: Math.round((ix / w) * pageW), y: Math.round((iy / h) * pageH) };
}
