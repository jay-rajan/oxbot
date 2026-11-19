// A phone's tab bar, at the bottom within the thumb's reach: the family
// Home's views. The bot list stays behind the menu button at the top. Wider
// screens have the sidebar instead, so it's hidden there (shell.css).
import { useApp, useSel } from "../../app/context";
import { HOME_LEADS } from "../../state/sidebar";
import { local } from "../../state/store";
import { HomeIcon } from "./HomeIcon";

/** Shorter names than the sidebar's, to fit four across a phone. */
const SHORT: Record<string, string> = { "home-meals": "Meals" };

export function PhoneTabs() {
  const { store, dispatch } = useApp();
  const page = useSel((s) => s.ui.page);
  const sideOpen = useSel((s) => s.ui.sideOpen);
  const hasBot = useSel((s) => s.bots.byId);
  const open = (h: (typeof HOME_LEADS)[number]) => {
    const s = store.getState();
    if (s.bots.byId[h.id] && h.id !== s.ui.current) dispatch(local("ui/select", { bot: h.id, keepPage: true }));
    dispatch(local("ui/page", { page: h.page }));
  };
  return (
    <nav className="phone-tabs" aria-label="Views">
      {HOME_LEADS.map((h) =>
        h.page !== "calendar" && !hasBot[h.id] ? null : (
          <button
            key={h.id}
            type="button"
            className="phone-tab"
            id={`tab-${h.page}`}
            aria-current={String(page === h.page && !sideOpen) as "true" | "false"}
            onClick={() => open(h)}
          >
            <HomeIcon id={h.id} />
            <span>{SHORT[h.id] ?? h.name}</span>
          </button>
        ),
      )}
    </nav>
  );
}
