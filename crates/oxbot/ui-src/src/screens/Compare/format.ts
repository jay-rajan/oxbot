// Numbers on the Compare dialog, as the hand-written page wrote them.
import type { RunVerdict } from "../../net/protocol";

export const cmpMoney = (x: number | null | undefined) =>
  x == null ? "–" : x === 0 ? "$0" : x < 0.01 ? "<$0.01" : `$${x.toFixed(2)}`;
export const cmpNum = (x: number | null | undefined, digits = 0) =>
  x == null ? "–" : Number(x).toLocaleString(undefined, { maximumFractionDigits: digits });
export const cmpPct = (x: number | null | undefined) => (x == null ? "–" : `${Math.round(x * 100)}%`);
export const cmpSecs = (ms: number | null | undefined) => (ms == null ? "–" : `${(ms / 1000).toFixed(1)}s`);
export const VERDICT: Record<RunVerdict, string> = { pass: "✓ pass", partial: "½ partial", fail: "✗ fail" };
/** "3 actions held back": what a practice run would have done but didn't; nothing when none. */
export const heldBack = (n: number) => (n > 0 ? `${n} ${n === 1 ? "action" : "actions"} held back` : "");
/** A test target in words: its model. */
export const targetName = (t: { model: string }) => t.model;
