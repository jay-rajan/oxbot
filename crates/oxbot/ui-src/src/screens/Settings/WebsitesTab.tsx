// Settings → Websites: one list. The home's sites (shared bots) first, then
// your private ones (your private bots ask for those themselves). A home
// row's actions are on the row: Fine-grained control, Open, Sign out (Open
// and Sign out only on the home computer); a private row's only action is
// Share with the home.
import { type FormEvent, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import type { SiteInfo } from "../../net/protocol";
import { siteRows } from "./format";
import { useArm } from "./useArm";

const FINE_TITLE =
  "Bots do one step at a time here, so each fill and click shows on its own. Off: they may fill in, pick and click several things in a row. Either way they ask before Add to cart, Submit, Post or Sign in.";

/** The site's own icon (from that browser's saved icons), else its first letter. */
function SiteMark({ s }: { s: SiteInfo }) {
  if (s.icon?.startsWith("data:image/"))
    return (
      <span className="mono has-icon">
        <img src={s.icon} alt="" />
      </span>
    );
  return <span className="mono">{s.site.replace(/^www\./, "")[0] || "?"}</span>;
}

export function WebsitesTab({ hidden }: { hidden: boolean }) {
  const { send, toast } = useApp();
  const farm = useSel((s) => !!s.connection.settings?.farm);
  const homeSites = useSel((s) => s.home.homeSites);
  const mySites = useSel((s) => s.home.mySites);
  const fineSites = useSel((s) => s.home.fineSites);
  const urlRef = useRef<HTMLInputElement>(null);
  const row = useArm();
  const all = useArm();
  const mineAll = useArm();
  const rows = siteRows(homeSites, mySites, fineSites);

  const addSite = (ev: FormEvent) => {
    ev.preventDefault();
    const input = urlRef.current;
    const url = input?.value.trim() ?? "";
    if (!url || !input) return;
    send({ type: "add_sign_in", url });
    input.value = "";
    toast(`Opening ${url} in the home's Chrome window. Sign in there, then close the window.`);
  };

  return (
    <div id="setSignins" role="tabpanel" hidden={hidden}>
      <section>
        <form className="add-site" id="addSiteForm" onSubmit={addSite}>
          <input
            id="addSiteUrl"
            ref={urlRef}
            type="text"
            inputMode="url"
            autoComplete="off"
            spellCheck={false}
            placeholder="e.g. gmail.com"
            aria-label="Add a website"
          />
          <button className="primary" type="submit">
            Sign in
          </button>
        </form>
        <p className="hint">You sign in once, in the home browser, and shared bots use it. A private bot asks for its own sign-in when it needs one.</p>
        <ul className="site-rows" id="siteRows">
          {rows.length === 0 ? <li className="hint">No websites yet.</li> : null}
          {rows.map(({ site: s, mine, stepOnly }) => {
            const key = (mine ? "me:" : "home:") + s.site;
            const since = s.signed_in_ms
              ? ` · since ${new Date(s.signed_in_ms).toLocaleDateString([], { month: "short", day: "numeric" })}`
              : "";
            return (
              <li key={key} className={`site-row${mine ? " private" : ""}`}>
                <SiteMark s={s} />
                <span className="name">
                  {s.site}
                  <small>{mine ? `Private: your bots${since}` : stepOnly ? "Not signed in" : `Shared bots${since}`}</small>
                </span>
                <div className="site-acts">
                  {mine ? (
                    // The home computer says when it's shared (or why it couldn't be).
                    <button
                      type="button"
                      className="ghost"
                      title="Shared bots can then use this sign-in too. Your private bots keep theirs."
                      onClick={() => send({ type: "share_site", site: s.site })}
                    >
                      Share with the home
                    </button>
                  ) : (
                    <>
                      <label className="check steps" title={FINE_TITLE}>
                        <input
                          type="checkbox"
                          checked={fineSites.includes(s.site)}
                          disabled={!farm}
                          onChange={(e) => send({ type: "set_fine_grained_site", site: s.site, on: e.target.checked })}
                        />
                        {" Fine-grained control"}
                      </label>
                      {/* Only the home computer may open or sign the home browser out. */}
                      {!stepOnly && farm ? (
                        <>
                          <button type="button" className="ghost" onClick={() => send({ type: "open_site", site: s.site, mine: false })}>
                            Open
                          </button>
                          <button
                            type="button"
                            className="ghost"
                            onClick={() => {
                              if (row.fire(key)) send({ type: "sign_out_site", site: s.site, mine: false });
                            }}
                          >
                            {row.armed === key ? "Yes, sign out" : "Sign out"}
                          </button>
                        </>
                      ) : null}
                    </>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
        <div className="btns">
          <button
            className="ghost"
            type="button"
            id="signOutAll"
            hidden={!homeSites.length || !farm}
            onClick={() => {
              if (all.fire("all")) send({ type: "sign_out_all", mine: false });
            }}
          >
            {all.armed === "all" ? "Yes, sign out everywhere" : "Sign out of all sites"}
          </button>
          <button
            className="ghost"
            type="button"
            id="signOutMine"
            hidden={!mySites.length}
            onClick={() => {
              if (mineAll.fire("mine")) send({ type: "sign_out_all", mine: true });
            }}
          >
            {mineAll.armed === "mine" ? "Yes, sign out of all your private sites" : "Sign out of all your private sites"}
          </button>
        </div>
      </section>
    </div>
  );
}
