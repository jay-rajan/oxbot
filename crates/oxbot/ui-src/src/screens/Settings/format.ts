// Words and numbers the Settings window shows, as the hand-written page
// wrote them. Pure, so they're tested on their own.
import type { DoorState, GateInfo, SetupCheck, SetupReport, SiteInfo } from "../../net/protocol";

/** Dollars: "<$0.01" for a sliver, else two decimals. */
export const money = (c: number): string => (c > 0 && c < 0.01 ? "<$0.01" : `$${c.toFixed(2)}`);

export function bytes(n: number): string {
  if (!n) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

const SWAP_WORDS: Record<string, [string, string]> = {
  email: ["email", "emails"],
  phone: ["phone number", "phone numbers"],
  person: ["name", "names"],
  place: ["place", "places"],
  address: ["address", "addresses"],
  postal: ["postal code", "postal codes"],
  card: ["card number", "card numbers"],
  account: ["account number", "account numbers"],
  id: ["ID number", "ID numbers"],
};

/** "swapped out: 2 emails, 1 name", or "" when nothing was. */
export function swappedText(sw: { [k in string]?: number } | null | undefined): string {
  const parts = Object.entries(sw ?? {})
    .filter((e): e is [string, number] => (e[1] ?? 0) > 0)
    .map(([k, n]) => {
      const w = SWAP_WORDS[k] ?? [k, `${k}s`];
      return `${n} ${n === 1 ? w[0] : w[1]}`;
    });
  return parts.length ? `swapped out: ${parts.join(", ")}` : "";
}

export const NET_KINDS: Record<string, string> = { ai: "AI", website: "Website", device: "Device", oxbot: "oxbot" };

export const plural = (n: number, word: string): string => `${n} ${word}${n === 1 ? "" : "s"}`;

/** A device: "connected now", or when it was last seen. */
export function seenText(g: GateInfo): string {
  return g.online
    ? "connected now"
    : `seen ${new Date(g.last_seen_ms).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}`;
}

/** The network the home computer is on, and whether phones may use it. */
export function doorHere(door: DoorState | null): { here: DoorState["current"]; listed: boolean } {
  const here = door?.current ?? null;
  return { here, listed: !!(here && door?.networks.some((n) => n.id === here.id)) };
}

// ---------- setup

/** Whether nothing needed is missing (optional parts may be off). */
export function setupReady(setup: SetupReport | null): boolean {
  return !setup || setup.checks.every((c) => c.ok || c.optional);
}

/** The checks to list: what's missing, and optional ones too when asked. */
export function missingChecks(setup: SetupReport | null, withOptional: boolean): SetupCheck[] {
  if (!setup) return [];
  return setup.checks.filter((c) => !c.ok && (!c.optional || withOptional));
}

/** A fix's words, with commands (between two spaces) marked as code. */
export function fixParts(fix: string): { text: string; code: boolean }[] {
  return fix.split(/ {2}/).map((text, i) => ({ text, code: i % 2 === 1 }));
}

// ---------- websites

export interface SiteRow {
  site: SiteInfo;
  /** One of your private sites (your private bots'). */
  mine: boolean;
  /** Set to fine-grained control, and nobody's signed in to it yet. */
  stepOnly: boolean;
}

/** One list: the home's sites (shared bots) first, with sites set to
 *  fine-grained control that nobody's signed in to, then your private ones. */
export function siteRows(homeSites: SiteInfo[], mySites: SiteInfo[], fineSites: string[]): SiteRow[] {
  const home = new Map<string, SiteRow>(homeSites.map((s) => [s.site, { site: s, mine: false, stepOnly: false }]));
  for (const site of fineSites)
    if (!home.has(site)) home.set(site, { site: { site, signed_in_ms: 0, icon: null }, mine: false, stepOnly: true });
  const mine = mySites
    .filter((s) => !home.has(s.site) || home.get(s.site)?.stepOnly)
    .map((s) => ({ site: s, mine: true, stepOnly: false }));
  return [...home.values(), ...mine];
}
