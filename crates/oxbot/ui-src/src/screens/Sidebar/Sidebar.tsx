// The sidebar: brand and New bot, the server line, search, group by, the bot
// list, and the bottom row (Settings, People, Signed-in websites). On a
// phone it's a drawer over the chat, with a scrim behind it.
import { type KeyboardEvent, type MouseEvent, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, IconButton, Input, Pill, PlusIcon, vars } from "../../components/ui";
import { botList } from "../../state/bots";
import { groupBots, HOME_LEADS, initialSidebar } from "../../state/sidebar";
import { local } from "../../state/store";
import { type NameAsk as AskName, askName as askNameDialog } from "../Welcome/NameDialog";
import { BotList } from "./BotList";
import { BotMenu, type MenuAt, type MenuFor, menuKey, refocus } from "./BotMenu";
import { avatarColor, avatarSvg } from "../../components/brand";
import { BrandMark } from "./BrandMark";
import { HomeIcon } from "./HomeIcon";
import { keep, keptJson } from "./storage";
import "./sidebar.css";

export function Sidebar() {
  const { store, dispatch } = useApp();
  const sideOpen = useSel((s) => s.ui.sideOpen);
  const page = useSel((s) => s.ui.page);
  const current = useSel((s) => s.ui.current);
  const status = useSel((s) => s.connection.status);
  const everConnected = useSel((s) => s.connection.everConnected);
  const serverName = useSel((s) => s.connection.serverName);
  const homeComputer = useSel((s) => !!s.connection.settings?.home_computer);
  const signedIn = useSel((s) => s.home.homeSites.length + (s.connection.settings?.owner ? s.home.mySites.length : 0));
  const warnings = useSel((s) => s.sidebar.warnings);
  const query = useSel((s) => s.sidebar.query);
  const seen = useSel((s) => s.sidebar.seen);
  const collapsed = useSel((s) => s.sidebar.collapsed);
  const pinned = useSel((s) => s.sidebar.pinned);
  const botsN = useSel((s) => s.sidebar.botsN);
  const created = useSel((s) => s.sidebar.created);
  const busy = useSel((s) => s.sidebar.busy);
  const hasBot = useSel((s) => s.bots.byId);

  // What this device kept: seen marks (green dots), grouping, folded groups.
  useLayoutEffect(() => {
    dispatch(
      local("sidebar/load", {
        seen: keptJson<Record<string, number>>("oxbot.seen", {}),
        // Always by section (People grouping is gone).
        groupBy: "sections",
        collapsed: keptJson<string[]>("oxbot.collapsed", []),
        pinned: keptJson<string[]>("oxbot.pinned", []),
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
    if (pinned !== initialSidebar.pinned) keep.set("oxbot.pinned", JSON.stringify(pinned));
  }, [pinned]);
  useEffect(() => {
    if (collapsed !== initialSidebar.collapsed) keep.set("oxbot.collapsed", JSON.stringify(collapsed));
  }, [collapsed]);
  // The open bot, and the Home view if one is open, are remembered for
  // next time (a refresh comes back to them).
  useEffect(() => {
    if (current) keep.set("oxbot.bot", current);
  }, [current]);
  // Read before anything is written: the first render's page is always the
  // chat, and saving it first would forget the view to come back to.
  const lastPage = useRef(keep.get("oxbot.page"));
  useEffect(() => {
    if (lastPage.current === null) keep.set("oxbot.page", page);
  }, [page]);

  // Which bot to open when the list comes: one just made, the open one, the
  // one open last time, else the Calendar's bot, else the first. Never the
  // Assistant: it's out of view now.
  useEffect(() => {
    if (!botsN) return;
    const s = store.getState();
    const ids = s.bots.order.filter((id) => !s.bots.byId[id]?.central);
    const known = (id: string | null | undefined): id is string => !!id && ids.includes(id);
    const pending = s.ui.pendingSelect;
    const want = [pending, s.ui.current, keep.get("oxbot.bot"), HOME_LEADS[0].id, ids[0]].find(known);
    if (want && want !== s.ui.current && (!pending || want === pending)) {
      dispatch(local("ui/select", { bot: want }));
      // A Home view open last time, with its bot, opens again.
      const home = HOME_LEADS.find((h) => h.page === lastPage.current && h.id === want);
      if (home && !pending) dispatch(local("ui/page", { page: home.page }));
    }
    if (want) {
      // From now on, each view is remembered as it's opened.
      lastPage.current = null;
    }
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

  // A Home item: its bot (if the home still has it) and its page: the
  // Calendar beside its bot's chat; Meal Plan and Records their chat
  // for now.
  const openHome = (h: (typeof HOME_LEADS)[number]) => {
    const s = store.getState();
    if (s.bots.byId[h.id] && h.id !== s.ui.current) dispatch(local("ui/select", { bot: h.id, keepPage: true }));
    dispatch(local("ui/page", { page: h.page }));
  };

  const toSettings = (tab?: string) => {
    dispatch(local("ui/side", { open: false }));
    dispatch(local("ui/open", tab ? { dialog: "settings", arg: tab } : { dialog: "settings" }));
  };

  // Search: as you type; Enter opens the first match, Escape clears it.
  const searchKey = (ev: KeyboardEvent<HTMLInputElement>) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      const s = store.getState();
      const first = groupBots(botList(s.bots), s.home.sections, s.home.placed, s.sidebar.groupBy, s.sidebar.query, s.connection.settings?.you, s.sidebar.pinned).flatMap((g) => g.ids)[0];
      if (first) dispatch(local("ui/select", { bot: first }));
    } else if (ev.key === "Escape" && ev.currentTarget.value) {
      ev.stopPropagation();
      dispatch(local("sidebar/query", { query: "" }));
    }
  };


  // The server's name; "Reconnecting…" once a connection is lost (or never came).
  const server = status !== "open" && (everConnected || status === "closed") ? "Reconnecting…" : serverName || "Connecting…";

  return (
    <>
      <aside className={`side${sideOpen ? " open" : ""}`} id="side">
        <div className="side-head">
          <div className="brand">
            <BrandMark />
            <Button
              kind="secondary"
              size="sm"
              className="new-bot"
              id="newBot"
              aria-label="New bot"
              title="Make a new bot: say what it should do, in your own words"
              onClick={() => dispatch(local("ui/open", { dialog: "newBot" }))}
            >
              <PlusIcon />
              New bot
            </Button>
          </div>
          <div className="server" id="server">
            {server}
          </div>
        </div>
        <nav className="side-home" aria-label="Home">
          {HOME_LEADS.map((h) => {
            const here = page === h.page;
            if (h.page !== "calendar" && !hasBot[h.id]) return null;
            return (
              <button
                key={h.id}
                type="button"
                className="home-item"
                id={h.btn}
                aria-current={String(here) as "true" | "false"}
                onClick={() => openHome(h)}
              >
                <HomeIcon id={h.id} working={!!(busy[h.id] ?? hasBot[h.id]?.busy)} />
                <span className="meta">
                  <span className="name">
                    <span>{h.name}</span>
                  </span>
                </span>
              </button>
            );
          })}
          {pinned.map((id) => {
            const b = hasBot[id];
            if (!b || b.central) return null;
            return (
              <button
                key={id}
                type="button"
                className="bot-item pinned"
                data-bot={id}
                aria-current={String(id === current && page === "chat") as "true" | "false"}
                title="Pinned (right-click to unpin)"
                onClick={() => dispatch(local("ui/select", { bot: id }))}
                onContextMenu={(ev: MouseEvent) => {
                  ev.preventDefault();
                  openMenu({ bot: id }, ev.clientX, ev.clientY);
                }}
                onKeyDown={(ev: KeyboardEvent<HTMLButtonElement>) => {
                  if (!menuKey(ev)) return;
                  ev.preventDefault();
                  const r = ev.currentTarget.getBoundingClientRect();
                  openMenu({ bot: id }, r.left + 24, r.bottom);
                }}
              >
                {/* biome-ignore lint/security/noDangerouslySetInnerHtml: oxbot's own SVG */}
                <span className={`avatar${busy[id] ?? b.busy ? " working" : ""}`} style={vars({ tint: avatarColor(id) })} dangerouslySetInnerHTML={{ __html: avatarSvg(id, false) }} />
                <span className="meta">
                  <span className="name">
                    <span>{b.name || id}</span>
                  </span>
                </span>
              </button>
            );
          })}
        </nav>
        <div className="side-find">
          <Input
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
        </div>
        <BotList openMenu={openMenu} closeMenu={closeMenu} askName={askName} />
        <div className="side-foot">
          <IconButton className="gear" id="settingsBtn" label="Settings" onClick={() => toSettings()}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="12" cy="12" r="7.2" fill="none" stroke="currentColor" strokeWidth="3.6" strokeDasharray="2.83 2.83" />
              <circle cx="12" cy="12" r="5.6" fill="none" stroke="currentColor" strokeWidth="2" />
              <circle cx="12" cy="12" r="2" fill="currentColor" />
            </svg>
            <span className="badge" id="warnBadge" hidden={warnings === 0}>
              <Pill tone="danger">{String(warnings)}</Pill>
            </span>
          </IconButton>
          <Button
            kind="quiet"
            className="foot-btn reveal"
            id="deviceBtn"
            aria-label="People"
            title="People: add a person by scanning a QR code from their phone"
            hidden={!homeComputer}
            onClick={() => toSettings("setPeople")}
            icon={
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                <rect x="3.5" y="3.5" width="6.5" height="6.5" rx="1" />
                <rect x="14" y="3.5" width="6.5" height="6.5" rx="1" />
                <rect x="3.5" y="14" width="6.5" height="6.5" rx="1" />
                <path d="M14 14h2.5v2.5H14zM18 18h2.5v2.5H18zM18 14h2.5M14 18v2.5" strokeLinecap="round" />
                <rect x="5.8" y="5.8" width="1.9" height="1.9" fill="currentColor" stroke="none" />
                <rect x="16.3" y="5.8" width="1.9" height="1.9" fill="currentColor" stroke="none" />
                <rect x="5.8" y="16.3" width="1.9" height="1.9" fill="currentColor" stroke="none" />
              </svg>
            }
          >
            <span className="label">People</span>
          </Button>
          <Button
            kind="quiet"
            className="foot-btn end reveal"
            id="signedInBtn"
            aria-label="Signed-in websites"
            title="Signed-in websites: the ones your bots are signed in to (Settings → Websites)"
            onClick={() => toSettings("setSignins")}
            icon={
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                <circle cx="12" cy="12" r="8.5" />
                <path d="M3.5 12h17M12 3.5c2.4 2.4 3.6 5.2 3.6 8.5s-1.2 6.1-3.6 8.5c-2.4-2.4-3.6-5.2-3.6-8.5s1.2-6.1 3.6-8.5z" strokeLinejoin="round" />
              </svg>
            }
          >
            <span className="label">Signed-in websites</span>
            <span className="count" id="signedInCount" hidden={signedIn === 0}>
              <Pill>{String(signedIn)}</Pill>
            </span>
          </Button>
        </div>
      </aside>
      <div className="scrim" id="scrim" aria-hidden="true" onClick={() => dispatch(local("ui/side", { open: false }))} />
      <BotMenu at={menu} close={closeMenu} askName={askName} />
    </>
  );
}
