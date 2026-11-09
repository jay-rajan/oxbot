// This device's own sidebar settings, in localStorage (it may be blocked:
// then nothing is kept). Same keys as the old page, so nothing is lost.
export const keep = {
  get(k: string): string | null {
    try {
      return localStorage.getItem(k);
    } catch {
      return null;
    }
  },
  set(k: string, v: string): void {
    try {
      localStorage.setItem(k, v);
    } catch {
      /* not kept */
    }
  },
};

/** JSON from storage, or `fallback` if missing or broken. */
export function keptJson<T>(k: string, fallback: T): T {
  try {
    return (JSON.parse(keep.get(k) || "null") as T) ?? fallback;
  } catch {
    return fallback;
  }
}
