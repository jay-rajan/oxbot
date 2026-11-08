// The sidebar: brand and New bot, the server line, search, group by, the bot
// list, and the bottom row (Settings, Connect a device, Websites). On a
// phone it's a drawer over the chat, with a scrim behind it.
import { type KeyboardEvent, useCallback, useEffect, useLayoutEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { botList } from "../../state/bots";
import { groupBots, initialSidebar } from "../../state/sidebar";
import { local } from "../../state/store";
import { type NameAsk as AskName, askName as askNameDialog } from "../Welcome/NameDialog";
import { BotList } from "./BotList";
import { BotMenu, type MenuAt, type MenuFor, refocus } from "./BotMenu";
import { BrandMark } from "./BrandMark";
import { keep, keptJson } from "./storage";

export function Sidebar() {
  const { store, dispatch } = useApp();
  const sideOpen = useSel((s) => s.ui.sideOpen);
  const current = useSel((s) => s.ui.current);
  const status = useSel((s) => s.connection.status);
  const everConnected = useSel((s) => s.connection.everConnected);
  const serverName = useSel((s) => s.connection.serverName);
  const farm = useSel((s) => !!s.connection.settings?.farm);
  const signedIn = useSel((s) => s.home.homeSites.length + s.home.mySites.length);
  const warnings = useSel((s) => s.sidebar.warnings);
  const query = useSel((s) => s.sidebar.query);
  const groupBy = useSel((s) => s.sidebar.groupBy);
  const seen = useSel((s) => s.sidebar.seen);
  const collapsed = useSel((s) => s.sidebar.collapsed);
  const botsN = useSel((s) => s.sidebar.botsN);
  const created = useSel((s) => s.sidebar.created);

  // What this device kept: seen marks (green dots), grouping, folded groups.
  useLayoutEffect(() => {
    dispatch(
      local("sidebar/load", {
        seen: keptJson<Record<string, number>>("oxbot.seen", {}),
        groupBy: keep.get("oxbot.groupBy"),
        collapsed: keptJson<string[]>("oxbot.collapsed", []),
      }),
    );
    const vis = () => dispatch(local("sidebar/visible", { visible: document.visibilityState === "visible" }));
    vis();
    document.addEventListener("visibilitychange", vis);
    return () => document.removeEventListener("visibilitychange", vis);
  }, [dispatch]);
  useEffect(() => {
    if (seen !== initialSidebar.seen) keep.set("oxbot.seen", JSON.stringify(seen));
  }, [seen]);
  useEffect(() => {
    if (collapsed !== initialSidebar.collapsed) keep.set("oxbot.collapsed", JSON.stringify(collapsed));
  }, [collapsed]);
  // The open bot is remembered for next time.
  useEffect(() => {
    if (current) keep.set("oxbot.bot", current);
  }, [current]);

  // Which bot to open when the list comes: one just made, the open one, the
  // one open last time, else the first.
  useEffect(() => {
    if (!botsN) return;
    const s = store.getState();
    const ids = s.bots.order;
    const known = (id: string | null | undefined): id is string => !!id && ids.includes(id);
    const pending = s.ui.pendingSelect;
    const want = [pending, s.ui.current, keep.get("oxbot.bot"), ids[0]].find(known);
    if (want && want !== s.ui.current && (!pending || want === pending)) dispatch(local("ui/select", { bot: want }));
  }, [botsN, store, dispatch]);
  // A bot made from this page opens now if it's listed, else once it is.
  useEffect(() => {
    if (!created) return;
    if (created.id in store.getState().sidebar.busy) dispatch(local("ui/select", { bot: created.id }));
    else dispatch(local("ui/pendingSelect", { bot: created.id }));
  }, [created, store, dispatch]);

  // On a phone the sidebar covers the chat: Escape closes it (after the
  // menu, the search and any dialog).
  useEffect(() => {
    if (!sideOpen) return;
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape" && !document.querySelector("dialog[open]")) dispatch(local("ui/side", { open: false }));
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, [sideOpen, dispatch]);

  // The right-click menu, and the name dialog it (and + New section) uses.
  const [menu, setMenu] = useState<MenuAt | null>(null);
  const openMenu = useCallback(
    (f: MenuFor, x: number, y: number) => {
      // Assistant can't be renamed, deleted or moved: no menu at all.
      const s = store.getState();
      if (f.bot && (!s.bots.byId[f.bot] || s.bots.byId[f.bot]?.central)) {
        setMenu(null);
        return;
      }
      if (f.section && !s.home.sections.some((x) => x.id === f.section)) return;
      setMenu({ for: f, x, y });
    },
    [store],
  );
  const closeMenu = useCallback((refocusAfter?: boolean) => {
    setMenu((m) => {
      if (m && refocusAfter) {
        const f = m.for;
        setTimeout(() => refocus(f));
      }
      return null;
    });
  }, []);
  const askName = useCallback(
    (a: AskName) => askNameDialog(dispatch, a),
    [dispatch],
  );

  const toSettings = (tab?: string) => {
    dispatch(local("ui/side", { open: false }));
    dispatch(local("ui/open", tab ? { dialog: "settings", arg: tab } : { dialog: "settings" }));
  };

  // Search: as you type; Enter opens the first match, Escape clears it.
  const searchKey = (ev: KeyboardEvent<HTMLInputElement>) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      const s = store.getState();
      const first = groupBots(botList(s.bots), s.home.sections, s.home.placed, s.sidebar.groupBy, s.sidebar.query, s.connection.settings?.you).flatMap((g) => g.ids)[0];
      if (first) dispatch(local("ui/select", { bot: first }));
    } else if (ev.key === "Escape" && ev.currentTarget.value) {
      ev.stopPropagation();
      dispatch(local("sidebar/query", { query: "" }));
    }
  };

  // Group by sections or by people: this device remembers which.
  const setGroupBy = (mode: "sections" | "people") => {
    keep.set("oxbot.groupBy", mode);
    dispatch(local("sidebar/groupBy", { groupBy: mode }));
  };
  const groupKey = (ev: KeyboardEvent<HTMLButtonElement>) => {
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(ev.key)) return;
    ev.preventDefault();
    const next = groupBy === "people" ? "sections" : "people";
    setGroupBy(next);
    document.querySelector<HTMLElement>(`#groupBy [data-group="${next}"]`)?.focus();
  };

  // The server's name; "Reconnecting…" once a connection is lost (or never came).
  const server = status !== "open" && (everConnected || status === "closed") ? "Reconnecting…" : serverName || "Connecting…";

  return (
    <>
      <aside className={`side${sideOpen ? " open" : ""}`} id="side">
        <div className="brand">
          <BrandMark />
          <button className="new-bot" id="newBot" type="button" aria-label="New bot" title="New bot" onClick={() => dispatch(local("ui/open", { dialog: "newBot" }))}>
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M8 2.5v11M2.5 8h11" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" fill="none" />
            </svg>
          </button>
        </div>
        <div className="server" id="server">
          {server}
        </div>
        <input
          type="search"
          className="bot-search"
          id="botSearch"
          placeholder="Search bots"
          aria-label="Search bots"
          autoComplete="off"
          spellCheck={false}
          value={query}
          onChange={(e) => dispatch(local("sidebar/query", { query: e.target.value }))}
          onKeyDown={searchKey}
        />
        <div className="group-by" id="groupBy" role="radiogroup" aria-label="Group bots by">
          <button type="button" role="radio" data-group="sections" aria-checked={String(groupBy === "sections") as "true" | "false"} onClick={() => setGroupBy("sections")} onKeyDown={groupKey}>
            Sections
          </button>
          <button type="button" role="radio" data-group="people" aria-checked={String(groupBy === "people") as "true" | "false"} onClick={() => setGroupBy("people")} onKeyDown={groupKey}>
            People
          </button>
        </div>
        <BotList openMenu={openMenu} closeMenu={closeMenu} askName={askName} />
        <div className="side-foot">
          <button className="gear" id="settingsBtn" type="button" aria-label="Settings" title="Settings" onClick={() => toSettings()}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="12" cy="12" r="7.2" fill="none" stroke="currentColor" strokeWidth="3.6" strokeDasharray="2.83 2.83" />
              <circle cx="12" cy="12" r="5.6" fill="none" stroke="currentColor" strokeWidth="2" />
              <circle cx="12" cy="12" r="2" fill="currentColor" />
            </svg>
            <span className="badge" id="warnBadge" hidden={warnings === 0}>
              {String(warnings)}
            </span>
          </button>
          <button className="foot-btn" id="gateBtn" type="button" aria-label="Connect a device" title="Connect a device: pair a phone, tablet or laptop" hidden={!farm} onClick={() => toSettings("setPeople")}>
            <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
              <rect x="3.5" y="3.5" width="6.5" height="6.5" rx="1" />
              <rect x="14" y="3.5" width="6.5" height="6.5" rx="1" />
              <rect x="3.5" y="14" width="6.5" height="6.5" rx="1" />
              <path d="M14 14h2.5v2.5H14zM18 18h2.5v2.5H18zM18 14h2.5M14 18v2.5" strokeLinecap="round" />
              <rect x="5.8" y="5.8" width="1.9" height="1.9" fill="currentColor" stroke="none" />
              <rect x="16.3" y="5.8" width="1.9" height="1.9" fill="currentColor" stroke="none" />
              <rect x="5.8" y="16.3" width="1.9" height="1.9" fill="currentColor" stroke="none" />
            </svg>
            <span className="label">Connect a device</span>
          </button>
          <button className="foot-btn end" id="signedInBtn" type="button" aria-label="Websites" title="Websites: the ones your bots are signed in to (Settings → Websites)" onClick={() => toSettings("setSignins")}>
            <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
              <circle cx="12" cy="12" r="8.5" />
              <path d="M3.5 12h17M12 3.5c2.4 2.4 3.6 5.2 3.6 8.5s-1.2 6.1-3.6 8.5c-2.4-2.4-3.6-5.2-3.6-8.5s1.2-6.1 3.6-8.5z" strokeLinejoin="round" />
            </svg>
            <span className="count" id="signedInCount" hidden={signedIn === 0}>
              {String(signedIn)}
            </span>
          </button>
        </div>
      </aside>
      <div className="scrim" id="scrim" aria-hidden="true" onClick={() => dispatch(local("ui/side", { open: false }))} />
      <BotMenu at={menu} close={closeMenu} askName={askName} />
    </>
  );
}
