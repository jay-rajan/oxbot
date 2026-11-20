// How a Home view (Calendar, Meal Plan, Groceries, Records) and its bot's chat share
// the screen: the view alone (named for it: "Calendar", "Records",
// "Plan", "List"), the chat alone, or on a wide screen both side by side (Split).
// A phone has no Split: it shows the view instead. Shown in both headers,
// so either way back; the device remembers each view's choice.
import { useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Segmented } from "../../components/ui";
import { local } from "../../state/store";
import type { HomeMode as Mode } from "../../state/ui";

const KEY = "oxbot.homeMode.";
const PHONE = "(max-width: 720px)";

/** Whether the screen matches a media query, kept up to date. */
export function useMedia(query: string): boolean {
  const [on, setOn] = useState(() => matchMedia(query).matches);
  useEffect(() => {
    const m = matchMedia(query);
    const change = () => setOn(m.matches);
    change();
    m.addEventListener("change", change);
    return () => m.removeEventListener("change", change);
  }, [query]);
  return on;
}

/** Whether this is a phone-sized screen (no Split there). */
const useNarrow = () => useMedia(PHONE);

/** The open view's mode in effect: each view keeps its own (none yet:
 *  Split), and Split is the view alone on a phone. */
export function useHomeMode(): Mode {
  const mode = useSel((s) => s.ui.homeMode[s.ui.page] ?? "split");
  return useNarrow() && mode === "split" ? "full" : mode;
}

/** Each view's last choice on this device, once (none, or storage off: Split). */
export function useSavedHomeMode() {
  const { dispatch } = useApp();
  useEffect(() => {
    for (const page of ["calendar", "notes", "meals", "groceries"]) {
      try {
        const m = localStorage.getItem(KEY + page);
        if (m === "split" || m === "full" || m === "chat") dispatch(local("ui/homeMode", { page, mode: m }));
      } catch {
        // Storage off: Split.
      }
    }
  }, [dispatch]);
}

export function HomeMode({ where, view }: { where: string; view: string }) {
  const { dispatch } = useApp();
  const narrow = useNarrow();
  const mode = useHomeMode();
  const page = useSel((s) => s.ui.page);
  const choose = (m: Mode) => {
    dispatch(local("ui/homeMode", { page, mode: m }));
    try {
      localStorage.setItem(KEY + page, m);
    } catch {
      // Not kept on this device: fine.
    }
  };
  const options: (readonly [Mode, string])[] = [
    ...(narrow ? [] : [["split", "Split"] as const]),
    ["full", view],
    ["chat", "Chat"],
  ];
  return <Segmented className="home-mode" name={`homeMode-${where}`} label="Show" options={options} value={mode} onChange={choose} />;
}
