// Settings: one fixed-size window, five tabs down the side (General,
// Websites, People, Schedules, Activity); only the right side scrolls. On a
// phone it opens to a menu of the five, and each opens as its own page
// with a back arrow, like a phone's own settings. Opened with
// `ui/open {dialog:"settings", arg?}` (arg: a tab id, or "addDevice", which
// opens that page straight away). The tab it was left on is kept for next time.
import { type ReactNode, useCallback, useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { BackIcon, Dialog, DialogFrame, IconButton, Tabs } from "../../components/ui";
import type { SettingsTab } from "../../state/activity";
import { local } from "../../state/store";
import { useMedia } from "../Calendar/HomeMode";
import { expose } from "../../web/testBridge";
import { ActivityTab, askNet } from "./ActivityTab";
import { GeneralTab } from "./GeneralTab";
import { PeopleTab } from "./PeopleTab";
import { SchedulesTab } from "./SchedulesTab";
import { WebsitesTab } from "./WebsitesTab";
import "./settings.css";

const TABS: (readonly [SettingsTab, string, string?])[] = [
  ["setGeneral", "General"],
  ["setSignins", "Websites"],
  ["setPeople", "People", "peopleTab"],
  ["setSchedules", "Schedules", "schedulesTab"],
  ["setActivity", "Activity", "activityTab"],
];

const PHONE = "(max-width: 600px)";

const line = { viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: 1.8, strokeLinecap: "round", strokeLinejoin: "round", "aria-hidden": true } as const;
const ICONS: Record<SettingsTab, ReactNode> = {
  setGeneral: (
    <svg {...line}>
      <path d="M4 7h10M18 7h2M4 17h4M12 17h8" />
      <circle cx="16" cy="7" r="2" />
      <circle cx="10" cy="17" r="2" />
    </svg>
  ),
  setSignins: (
    <svg {...line}>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17M12 3.5c2.4 2.4 3.6 5.2 3.6 8.5s-1.2 6.1-3.6 8.5c-2.4-2.4-3.6-5.2-3.6-8.5s1.2-6.1 3.6-8.5z" />
    </svg>
  ),
  setPeople: (
    <svg {...line}>
      <circle cx="9" cy="8" r="3.2" />
      <path d="M3 19c.6-3.2 3-5 6-5s5.4 1.8 6 5M16 5.2a3 3 0 0 1 0 5.6M18 14.4c1.6.7 2.6 2.3 3 4.6" />
    </svg>
  ),
  setSchedules: (
    <svg {...line}>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </svg>
  ),
  setActivity: (
    <svg {...line}>
      <path d="M3 12h4l2.5-6 5 12 2.5-6h4" />
    </svg>
  ),
};

/** The phone's first page: each part of Settings, with a word on what's in it. */
function SettingsMenu({ go }: { go: (tab: SettingsTab) => void }) {
  const owner = useSel((s) => !!s.connection.settings?.owner);
  const sites = useSel((s) => s.home.homeSites.length + (owner ? s.home.mySites.length : 0));
  const people = useSel((s) => s.home.people.length);
  const schedules = useSel((s) => s.cards.allSchedules?.length ?? null);
  const mac = useSel((s) => s.connection.serverName);
  const version = useSel((s) => s.connection.settings?.version);
  const about: Record<SettingsTab, string> = {
    setGeneral: "AI, notifications, about",
    setSignins: sites ? `${sites} signed in` : "Sites your bots sign in to",
    setPeople: people ? `${people} of 6 people · join by QR code` : "Add the family; they join by QR code",
    setSchedules: schedules ? `${schedules} repeating` : "What bots do on their own",
    setActivity: "What went in and out today",
  };
  return (
    <div className="settings-menu">
      <nav className="set-card" aria-label="Settings">
        {TABS.map(([id, name]) => (
          <button key={id} type="button" className="set-row nav" data-tab={id} onClick={() => go(id)}>
            <span className="set-icon">{ICONS[id]}</span>
            <span className="set-text">
              <span className="set-name">{name}</span>
              <small>{about[id]}</small>
            </span>
            <span className="set-go" aria-hidden="true">›</span>
          </button>
        ))}
      </nav>
      <p className="set-foot">{[mac, version ? `version ${version}` : ""].filter(Boolean).join(" · ")}</p>
    </div>
  );
}

export function SettingsDialog() {
  const app = useApp();
  const { dispatch, send, store } = app;
  const open = useSel((s) => s.ui.dialog === "settings");
  const tab = useSel((s) => s.activity.tab);
  const profilesChanged = useSel((s) => s.home.profilesChanged);
  const arg = useSel((s) => (s.ui.dialog === "settings" ? s.ui.dialogArg : null));
  const phone = useMedia(PHONE);
  // On a phone: the menu, or one page. Opening to a named page skips the menu.
  const [paged, setPaged] = useState(false);
  useEffect(() => {
    if (open) setPaged(!!arg);
  }, [open, arg]);
  const onMenu = phone && !paged;
  const onClose = useCallback(() => {
    dispatch(local("ui/close", { dialog: "settings" }));
    // The browser hands focus back to the sidebar button that opened it,
    // ringed as if tabbed to: let it go.
    requestAnimationFrame(() => {
      const el = document.activeElement;
      if (el instanceof HTMLElement && el.closest(".side-foot")) el.blur();
    });
  }, [dispatch]);

  // Tests read the latest door state (door.networks, door.current).
  useEffect(() => expose("door", () => store.getState().home.door), [store]);

  // A tab coming into view asks for what it shows.
  useEffect(() => {
    if (!open || onMenu) return;
    if (tab === "setPeople") send({ type: "get_profile" });
    if (tab === "setActivity") askNet(app);
  }, [open, onMenu, tab, send, app]);

  // Someone's profile changed while People is open: ask again.
  useEffect(() => {
    const s = store.getState();
    if (profilesChanged && s.ui.dialog === "settings" && s.activity.tab === "setPeople") send({ type: "get_profile" });
  }, [profilesChanged, store, send]);

  return (
    <Dialog id="appDlg" size="xl" className="settings-dlg" labelledBy="appTitle" open={open} onClose={onClose}>
      <DialogFrame
        title={
          phone && paged ? (
            <>
              <IconButton label="Back to Settings" id="appBack" className="set-back" onClick={() => setPaged(false)}>
                <BackIcon />
              </IconButton>
              {TABS.find(([id]) => id === tab)?.[1] ?? "Settings"}
            </>
          ) : (
            "Settings"
          )
        }
        titleId="appTitle"
        onClose={onClose}
        closeId="appClose"
        className={phone ? "settings-frame phone" : "settings-frame"}
        bodyClassName="settings-body"
      >
        {onMenu ? (
          <SettingsMenu
            go={(id) => {
              dispatch(local("activity/tab", { tab: id }));
              setPaged(true);
            }}
          />
        ) : null}
        <Tabs
          hidden={phone}
          label="Settings"
          vertical
          className="settings-tabs"
          tabs={TABS}
          value={tab}
          onChange={(id) => {
            if (tab === id) {
              // Clicking the open tab asks again, as before.
              if (id === "setPeople") send({ type: "get_profile" });
              if (id === "setActivity") askNet(app);
            }
            dispatch(local("activity/tab", { tab: id }));
          }}
        />
        <div className="settings-panes" hidden={onMenu}>
          <GeneralTab hidden={tab !== "setGeneral"} />
          <WebsitesTab hidden={tab !== "setSignins"} />
          <PeopleTab hidden={tab !== "setPeople"} />
          <SchedulesTab hidden={!open || tab !== "setSchedules"} />
          <ActivityTab hidden={tab !== "setActivity"} />
        </div>
      </DialogFrame>
    </Dialog>
  );
}
