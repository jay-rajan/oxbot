// How a Home view (Calendar, Records) and its bot's chat share the
// screen: the view alone (named for it: "Calendar", "Records"), the chat
// alone, or on a wide screen both side by side (Split). A phone has no
// Split: it shows the view instead. Shown in both headers, so either way
// back; the device remembers the choice.
import { useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Segmented } from "../../components/ui";
import { local } from "../../state/store";
import type { HomeMode as Mode } from "../../state/ui";

const KEY = "oxbot.homeMode";
const PHONE = "(max-width: 720px)";

/** Whether this is a phone-sized screen (no Split there). */
function useNarrow(): boolean {
  const [narrow, setNarrow] = useState(() => matchMedia(PHONE).matches);
  useEffect(() => {
    const m = matchMedia(PHONE);
    const on = () => setNarrow(m.matches);
    m.addEventListener("change", on);
    return () => m.removeEventListener("change", on);
  }, []);
  return narrow;
}

/** The mode in effect: Split is the view alone on a phone. */
export function useHomeMode(): Mode {
  const mode = useSel((s) => s.ui.homeMode);
  return useNarrow() && mode === "split" ? "full" : mode;
}

/** This device's last choice, once (none, or storage off: Split). */
export function useSavedHomeMode() {
  const { dispatch } = useApp();
  useEffect(() => {
    try {
      const m = localStorage.getItem(KEY);
      if (m === "full" || m === "chat") dispatch(local("ui/homeMode", { mode: m }));
    } catch {
      // Storage off: Split.
    }
  }, [dispatch]);
}

export function HomeMode({ where, view }: { where: string; view: string }) {
  const { dispatch } = useApp();
  const narrow = useNarrow();
  const mode = useHomeMode();
  const choose = (m: Mode) => {
    dispatch(local("ui/homeMode", { mode: m }));
    try {
      localStorage.setItem(KEY, m);
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
