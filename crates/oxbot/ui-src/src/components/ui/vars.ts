// The one way a screen sets a style from data: CSS custom properties only
// (a person's color, a bar's fill), read by the screen's CSS. Plain inline
// styles are refused by scripts/check-ui.sh so every look stays in CSS.
import type { CSSProperties } from "react";

export function vars(values: Record<string, string | number | null | undefined>): CSSProperties {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(values)) {
    if (v !== null && v !== undefined && v !== "") out[`--${k}`] = typeof v === "number" ? String(v) : v;
  }
  return out as CSSProperties;
}

/** A 0–100 fill for a bar, as a percentage. */
export const pct = (part: number, whole: number) => `${whole > 0 ? Math.min(100, Math.max(0, Math.round((part / whole) * 100))) : 0}%`;
