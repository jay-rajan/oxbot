// Settings → Websites: one list. The home's sites (shared bots) first, then
// the owner's private ones (their private bots ask for those themselves;
// nobody else has any). A home
// row's actions are on the row: Fine-grained control, Open, Sign out (Open
// and Sign out only on the home computer); a private row's only action is
// Share with the home.
import { type FormEvent, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, Checkbox, cx, Hint, Input, List, ListRow, Note, Section } from "../../components/ui";
import type { SiteInfo } from "../../net/protocol";
import { siteRows } from "./format";
import { useArm } from "./useArm";

const FINE_TITLE =
  "Bots do one step at a time here. Either way they ask before Add to cart, Submit, Post or Sign in.";

/** The site's own icon (from that browser's saved icons), else its first letter. */
function SiteMark({ s }: { s: SiteInfo }) {
  if (s.icon?.startsWith("data:image/"))
    return (
      <span className="site-mark has-icon">
        <img src={s.icon} alt="" />
      </span>
    );
  return <span className="site-mark">{s.site.replace(/^www\./, "")[0] || "?"}</span>;
}

const NO_SITES: SiteInfo[] = [];

export function WebsitesTab({ hidden }: { hidden: boolean }) {
  const { send, toast } = useApp();
  const homeComputer = useSel((s) => !!s.connection.settings?.home_computer);
  const homeSites = useSel((s) => s.home.homeSites);
  // Only the owner of the home computer has private sign-ins.
  const ownsHome = useSel((s) => !!s.connection.settings?.owner);
  const allMine = useSel((s) => s.home.mySites);
  const mySites = ownsHome ? allMine : NO_SITES;
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

  const homeAll = homeSites.length > 0 && homeComputer;
  return (
    <div id="setSignins" className="settings-pane" role="tabpanel" hidden={hidden}>
      <Section>
        <form className="add-site" id="addSiteForm" onSubmit={addSite}>
          <Input
            id="addSiteUrl"
            ref={urlRef}
            type="text"
            inputMode="url"
            autoComplete="off"
            spellCheck={false}
            placeholder="e.g. gmail.com"
            aria-label="Add a website"
          />
          <Button kind="primary" type="submit">
            Sign in
          </Button>
        </form>
        <Hint id="siteHint">
          {ownsHome
            ? "Sign in once here and shared bots use it. Private bots ask for their own."
            : "Bots use the home browser and the websites the home is signed in to."}
        </Hint>
      </Section>
      <Section>
        <List className="site-rows" id="siteRows">
          {rows.length === 0 ? (
            <li>
              <Note>No websites yet.</Note>
            </li>
          ) : null}
          {rows.map(({ site: s, mine, stepOnly }) => {
            const key = (mine ? "me:" : "home:") + s.site;
            const since = s.signed_in_ms
              ? ` · since ${new Date(s.signed_in_ms).toLocaleDateString([], { month: "short", day: "numeric" })}`
              : "";
            return (
              <ListRow
                key={key}
                className={cx("site-row", mine && "private")}
                lead={<SiteMark s={s} />}
                title={s.site}
                sub={mine ? `Private: your bots${since}` : stepOnly ? "Not signed in" : `Shared bots${since}`}
                trail={
                  mine ? (
                    // The home computer says when it's shared (or why it couldn't be).
                    <Button
                      kind="quiet"
                      size="sm"
                      title="Shared bots can then use this sign-in too. Your private bots keep theirs."
                      onClick={() => send({ type: "share_site", site: s.site })}
                    >
                      Share with the home
                    </Button>
                  ) : (
                    <>
                      <Checkbox
                        className="steps"
                        title={FINE_TITLE}
                        checked={fineSites.includes(s.site)}
                        disabled={!homeComputer}
                        onChange={(e) => send({ type: "set_fine_grained_site", site: s.site, on: e.target.checked })}
                      >
                        Fine-grained control
                      </Checkbox>
                      {/* Only the home computer may open or sign the home browser out. */}
                      {!stepOnly && homeComputer ? (
                        <>
                          <Button kind="quiet" size="sm" onClick={() => send({ type: "open_site", site: s.site, mine: false })}>
                            Open
                          </Button>
                          <Button
                            kind="quiet"
                            size="sm"
                            onClick={() => {
                              if (row.fire(key)) send({ type: "sign_out_site", site: s.site, mine: false });
                            }}
                          >
                            {row.armed === key ? "Yes, sign out" : "Sign out"}
                          </Button>
                        </>
                      ) : null}
                    </>
                  )
                }
              />
            );
          })}
        </List>
        <div className="btn-row" hidden={!homeAll && !mySites.length}>
          <Button
            size="sm"
            id="signOutAll"
            hidden={!homeAll}
            onClick={() => {
              if (all.fire("all")) send({ type: "sign_out_all", mine: false });
            }}
          >
            {all.armed === "all" ? "Yes, sign out everywhere" : "Sign out of all sites"}
          </Button>
          <Button
            size="sm"
            id="signOutMine"
            hidden={!mySites.length}
            onClick={() => {
              if (mineAll.fire("mine")) send({ type: "sign_out_all", mine: true });
            }}
          >
            {mineAll.armed === "mine" ? "Yes, sign out of all your private sites" : "Sign out of all your private sites"}
          </Button>
        </div>
      </Section>
    </div>
  );
}
