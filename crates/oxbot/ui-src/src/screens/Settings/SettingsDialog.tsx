// Settings: one fixed-size window, four tabs down the side (General,
// Websites, People, Activity); only the right side scrolls. Opened with
// `ui/open {dialog:"settings", arg?}` (arg: a tab id, or "addDevice").
// The tab it was left on is kept for next time.
import { useCallback, useEffect } from "react";
import { useApp, useSel } from "../../app/context";
import { Dialog } from "../../components/Dialog";
import type { SettingsTab } from "../../state/activity";
import { local } from "../../state/store";
import { expose } from "../../web/testBridge";
import { ActivityTab, askNet } from "./ActivityTab";
import { GeneralTab } from "./GeneralTab";
import { PeopleTab } from "./PeopleTab";
import { WebsitesTab } from "./WebsitesTab";

const TABS: [SettingsTab, string, string | undefined][] = [
  ["setGeneral", "General", undefined],
  ["setSignins", "Websites", undefined],
  ["setPeople", "People", "peopleTab"],
  ["setActivity", "Activity", "activityTab"],
];

export function SettingsDialog() {
  const app = useApp();
  const { dispatch, send, store } = app;
  const open = useSel((s) => s.ui.dialog === "settings");
  const tab = useSel((s) => s.activity.tab);
  const profilesChanged = useSel((s) => s.home.profilesChanged);
  const onClose = useCallback(() => dispatch(local("ui/close", { dialog: "settings" })), [dispatch]);

  // Tests read the latest door state (door.networks, door.current).
  useEffect(() => expose("door", () => store.getState().home.door), [store]);

  // A tab coming into view asks for what it shows.
  useEffect(() => {
    if (!open) return;
    if (tab === "setPeople") send({ type: "get_profile" });
    if (tab === "setActivity") askNet(app);
  }, [open, tab, send, app]);

  // Someone's profile changed while People is open: ask again.
  useEffect(() => {
    const s = store.getState();
    if (profilesChanged && s.ui.dialog === "settings" && s.activity.tab === "setPeople") send({ type: "get_profile" });
  }, [profilesChanged, store, send]);

  return (
    <Dialog id="appDlg" className="settings-dlg" labelledBy="appTitle" open={open} onClose={onClose}>
      <div className="settings-frame">
        <header className="settings-head">
          <h2 id="appTitle">Settings</h2>
          <button className="ghost" id="appClose" type="button" onClick={onClose}>
            Close
          </button>
        </header>
        <div className="settings-body">
          <div className="tabs vtabs" role="tablist" aria-label="Settings" aria-orientation="vertical">
            {TABS.map(([id, label, btnId]) => (
              <button
                key={id}
                type="button"
                role="tab"
                data-tab={id}
                aria-selected={tab === id}
                id={btnId}
                onClick={() => {
                  if (tab === id) {
                    // Clicking the open tab asks again, as before.
                    if (id === "setPeople") send({ type: "get_profile" });
                    if (id === "setActivity") askNet(app);
                  }
                  dispatch(local("activity/tab", { tab: id }));
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <div className="settings-panes">
            <GeneralTab hidden={tab !== "setGeneral"} />
            <WebsitesTab hidden={tab !== "setSignins"} />
            <PeopleTab hidden={tab !== "setPeople"} />
            <ActivityTab hidden={tab !== "setActivity"} />
          </div>
        </div>
      </div>
    </Dialog>
  );
}
