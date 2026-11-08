// The open bot's chat: header, setup checks, the offline bar, the usage
// alert banner, the log, the note about the AI account, the cards, and the
// message box. Same ids, classes and words as the hand-written page.
import { type DragEvent, useEffect, useRef, useState } from "react";
import { type App, useApp, useSel } from "../../app/context";
import { avatarSvg } from "../../components/brand";
import type { BotInfo, UsageAlert } from "../../net/protocol";
import { uploading } from "../../state/chat";
import { local } from "../../state/store";
import { expose, exposeFn } from "../../web/testBridge";
import { Cards } from "../Cards/Cards";
import { addFiles, follow, isLocked, openEngines, openReport, stopDictation } from "./actions";
import { Composer } from "./Composer";
import { Log } from "./Log";
import { purposeLine } from "./purpose";

const KEYCHAIN_WAITING = "Waiting for the Mac's Keychain: allow oxbot in the dialog.";

// ---------- usage alert banner
const OR_ALERT_NAMES: Record<string, string> = {
  openrouter_free_daily: "Free AI questions today",
  openrouter_spend_monthly: "Paid AI spending this month",
  openrouter_credits: "AI account credit",
};
const DISMISSED = "oxbotAlertsDismissed";
const alertKey = (a: UsageAlert) => `${a.limit}:${a.resets_at_ms}:${a.level}`;
function readDismissed(): string[] {
  try { return JSON.parse(localStorage.getItem(DISMISSED) || "[]") || []; } catch { return []; }
}
function resetsIn(ms: number): string {
  if (!ms) return "";
  const mins = Math.round((ms - Date.now()) / 60000);
  if (mins <= 0) return "resets now";
  const when = new Date(ms).toLocaleString([], { weekday: mins > 24 * 60 ? "short" : undefined, hour: "numeric", minute: "2-digit" });
  return mins < 60 ? `resets in ${mins} min` : mins < 24 * 60 ? `resets in ${Math.floor(mins / 60)} h ${mins % 60} min (${when})` : `resets ${when}`;
}

function UsageAlertBanner() {
  const app = useApp();
  const alerts = useSel((s) => s.home.usageAlerts);
  const helloAt = useSel((s) => s.home.helloAt);
  const on = useSel((s) => s.connection.settings?.usage_alerts !== false);
  const byId = useSel((s) => s.bots.byId);
  // A window that resets takes its alert away, and "resets in" keeps up.
  const [, tick] = useState(0);
  useEffect(() => { const t = setInterval(() => tick((n) => n + 1), 60000); return () => clearInterval(t); }, []);

  // A live alert (not the banner coming back on connect) also toasts.
  const known = useRef<Record<string, UsageAlert>>({});
  useEffect(() => {
    for (const a of Object.values(alerts)) {
      const k = known.current[a.limit];
      const fresh = !k || k.level !== a.level || k.resets_at_ms !== a.resets_at_ms;
      if (fresh && k !== a && Date.now() - helloAt > 3000) app.toast(a.message);
    }
    known.current = { ...alerts };
  }, [alerts, helloAt, app]);

  const now = Date.now();
  const dismissed = readDismissed();
  const live = on
    ? Object.values(alerts).filter((a) => (!a.resets_at_ms || a.resets_at_ms > now) && !dismissed.includes(alertKey(a)))
    : [];
  live.sort((a, b) => b.level - a.level || (a.resets_at_ms || Infinity) - (b.resets_at_ms || Infinity));
  const a = live[0];
  const label = (limit: string) => {
    const bot = limit.startsWith("openrouter_bot_daily:") ? limit.slice("openrouter_bot_daily:".length) : null;
    if (bot !== null) return `${byId[bot]?.name ?? bot}'s questions today`;
    return OR_ALERT_NAMES[limit] || limit.replace(/_/g, " ");
  };
  const when = a ? resetsIn(a.resets_at_ms) : "";
  const dismiss = () => {
    if (!a) return;
    const all = Object.values(alerts).map(alertKey);
    const next = [...readDismissed().filter((k) => all.includes(k)), alertKey(a)];
    try { localStorage.setItem(DISMISSED, JSON.stringify(next)); } catch { /* blocked */ }
    tick((n) => n + 1);
  };
  return (
    <div className="offline alert-banner" id="usageAlert" role="status" hidden={!a} title={a?.message}>
      <span className="alert-text" id="usageAlertText">
        {a ? <><b>{`${label(a.limit)} at ${Math.round(a.used * 100)}%`}</b>{when ? ` · ${when}` : ""}</> : null}
      </span>
      <button type="button" className="alert-more" id="usageAlertMore" onClick={() => openEngines(app)}>Details</button>
      <button type="button" id="usageAlertClose" aria-label="Dismiss" title="Dismiss" onClick={dismiss}>✕</button>
    </div>
  );
}

function SetupBox() {
  const { send } = useApp();
  const setup = useSel((s) => s.home.setup);
  const ready = !setup || setup.checks.every((c) => c.ok || c.optional);
  return (
    <div className="setup" id="setupBox" hidden={ready}>
      <h3>oxbot needs a few things first</h3>
      <ul className="checks" id="setupChecks">
        {(setup?.checks ?? []).filter((c) => !c.ok && !c.optional).map((c) => (
          <li key={c.id} className="bad">
            <span>✕</span>
            <span>
              <b>{c.name}</b>{` · ${c.detail}`}
              {c.fix ? (
                // Commands sit between two spaces; show them as copyable code.
                <div className="fix">{c.fix.split(/ {2}/).map((part, i) => (i % 2 === 1 ? <code key={i}>{part}</code> : part))}</div>
              ) : null}
            </span>
          </li>
        ))}
      </ul>
      <div className="btns"><button className="primary" type="button" id="setupAgain" onClick={() => send({ type: "check_setup" })}>Check again</button></div>
    </div>
  );
}

function Offline() {
  const { phone, hasKey } = useApp();
  const status = useSel((s) => s.connection.status);
  const ever = useSel((s) => s.connection.everConnected);
  const failed = useSel((s) => s.chat.failedOnce);
  // Shown once a connection was lost (or never came), not while first connecting.
  const hidden = status === "open" || (!ever && !failed);
  const text = phone
    ? `Not connected to ${phone.farm || "the home computer"}. Waiting for home Wi-Fi…`
    : !hasKey
      ? "This window isn't oxbot's own. Open oxbot from its app, or with the link `oxbot serve` printed."
      : "Lost the connection to oxbot. Reconnecting…";
  return (
    <div className="offline" id="offline" role="status" hidden={hidden}>
      <span className="offline-dot" aria-hidden="true" />
      <span id="offlineText">{text}</span>
    </div>
  );
}

function EngineNote({ info }: { info: BotInfo | undefined }) {
  const app = useApp();
  const settings = useSel((s) => s.connection.settings);
  const enginesOwner = useSel((s) => s.home.engines?.owner);
  const owner = settings?.owner_name || enginesOwner || "the owner";
  let text = "";
  let action: string | null = null;
  // Every bot needs the home's AI account first.
  if (info && !isLocked(info) && settings && !settings.openrouter_ready) {
    if (settings.keychain_waiting) text = KEYCHAIN_WAITING;
    else if (settings.farm) { text = "The home's AI account isn't connected yet."; action = "Connect it"; }
    else text = `The home's AI account isn't connected yet; ask ${owner}.`;
  } else if (info && !isLocked(info) && settings && !settings.codex_ready) {
    if (settings.farm) { text = "The AI helper isn't ready on the home computer."; action = "Set up"; }
    else text = `The AI helper isn't ready on the home computer; ask ${owner}.`;
  }
  return (
    <div className="engine-note" id="engineNote" role="status" hidden={!text}>
      <span id="engineNoteText">{text}</span>
      <button className="ghost" type="button" id="engineNoteBtn" hidden={!action} onClick={() => openEngines(app)}>{action ?? ""}</button>
    </div>
  );
}

/** The status line under the bot's name. */
function statusText(info: BotInfo, busy: boolean, activity: string | null, dictating: boolean): string {
  if (isLocked(info)) return `private to ${info.created_by || "someone"}`;
  if (dictating) return "Listening… (on the home computer)";
  const shared = info.sharing === "people" ? `shared with ${info.members?.join(", ") || "picked people"}` : info.private ? "private" : "";
  if (busy) return activity || "Working…";
  if (info.central) return "leads your bots";
  return [shared, purposeLine(info.instructions)].filter(Boolean).join(" · ");
}

/** Follows the open bot and catches up after a reconnect, like the old page. */
function useFollow(app: App) {
  const current = useSel((s) => s.ui.current);
  const status = useSel((s) => s.connection.status);
  const locked = useSel((s) => (s.ui.current ? isLocked(s.bots.byId[s.ui.current]) : false));
  const known = useSel((s) => (s.ui.current ? s.ui.current in s.bots.byId : false));
  const settings = useSel((s) => s.connection.settings);
  const owner = !!(settings?.owner && settings.dev_tools);

  // The socket (re)opened: every followed chat catches up.
  useEffect(() => {
    if (status !== "open") return;
    const s = app.store.getState();
    for (const [id, b] of Object.entries(s.chat.bots)) if (b.subscribed) follow(app, id, b);
  }, [status, app]);

  // A bot opened: follow it (once), stop dictating, and ask for its thumbs.
  useEffect(() => {
    if (!current) return;
    stopDictation(app);
    const s = app.store.getState();
    const b = s.chat.bots[current];
    if (known && !b?.subscribed && !locked) {
      app.dispatch(local("chat/subscribed", { bot: current }));
      follow(app, current, b);
    }
  }, [current, known, locked, app]);

  useEffect(() => {
    if (!current || !owner) return;
    const info = app.store.getState().bots.byId[current];
    if (info && !info.central && !info.locked) app.send({ type: "list_ratings", bot: current });
  }, [current, owner, app]);
}

export function ChatMain() {
  const app = useApp();
  const { dispatch } = app;
  const current = useSel((s) => s.ui.current);
  const sideOpen = useSel((s) => s.ui.sideOpen);
  const info = useSel((s) => (s.ui.current ? s.bots.byId[s.ui.current] : undefined));
  const chat = useSel((s) => (s.ui.current ? s.chat.bots[s.ui.current] : undefined));
  const dictating = useSel((s) => !!s.chat.dictating);
  const notice = useSel((s) => s.chat.notice);
  const uploadingNow = useSel((s) => uploading(s.chat, s.ui.current));
  const [drag, setDrag] = useState(false);
  const dragDepth = useRef(0);
  useFollow(app);

  // Messages from the chat's own state (e.g. dictation failed) toast.
  useEffect(() => { if (notice) app.toast(notice.text, notice.error); }, [notice, app]);

  // The names the tests use: bots.get(id).info and .events, openReport().
  useEffect(() => {
    exposeFn("openReport", ((bot: string, id: string) => openReport(app, bot, id)) as never);
    expose("bots", () => {
      const s = app.store.getState();
      return new Map(Object.entries(s.bots.byId).map(([id, i]) => [id, { info: i, events: s.chat.bots[id]?.events ?? [] }]));
    });
  }, [app]);

  const locked = isLocked(info);
  const busy = chat ? chat.busy : !!info?.busy;
  const manage = !!info?.can_manage;
  const moreLabel = manage ? "Bot settings" : "Reports";

  // Drop anywhere on the chat (not the sidebar's own bot dragging).
  const hasFiles = (ev: DragEvent) => !!ev.dataTransfer && [...ev.dataTransfer.types].includes("Files");

  return (
    <main
      className="chat"
      id="chatMain"
      onDragEnter={(ev) => { if (!hasFiles(ev)) return; ev.preventDefault(); dragDepth.current++; setDrag(true); }}
      onDragOver={(ev) => { if (!hasFiles(ev)) return; ev.preventDefault(); ev.dataTransfer.dropEffect = "copy"; }}
      onDragLeave={(ev) => { if (!hasFiles(ev)) return; if (--dragDepth.current <= 0) { dragDepth.current = 0; setDrag(false); } }}
      onDrop={(ev) => { if (!hasFiles(ev)) return; ev.preventDefault(); dragDepth.current = 0; setDrag(false); addFiles(app, ev.dataTransfer.files); }}
    >
      <header className="chat-head">
        <button className="menu" id="menu" type="button" aria-label="Show bots" onClick={() => dispatch(local("ui/side", { open: !sideOpen }))}>☰</button>
        <span
          className={`avatar head-avatar${busy && !locked && info ? " working" : ""}`}
          id="botAvatar"
          hidden={!current}
          // biome-ignore lint: a fixed SVG made from the bot id, no user text
          dangerouslySetInnerHTML={{ __html: current ? avatarSvg(current, !!info?.central) : "" }}
        />
        <div>
          <div className="head-title">
            <h1 id="botName">{current ? info?.name || current : "oxbot"}</h1>
          </div>
          <div className="status" id="botStatus" data-bot={current ?? undefined}>{info ? statusText(info, busy, chat?.activity ?? null, dictating) : ""}</div>
        </div>
        <button
          className="head-more"
          id="botMore"
          type="button"
          aria-label={info ? moreLabel : "Bot settings"}
          title={info ? moreLabel : "Bot settings"}
          hidden={!info || locked}
          onClick={() => { if (info && !locked) dispatch(local("ui/open", { dialog: "botSettings" })); }}
        >
          ···
        </button>
      </header>
      <SetupBox />
      <Offline />
      <UsageAlertBanner />
      <Log />
      <EngineNote info={info} />
      <Cards />
      <Composer info={info} busy={busy} uploadingNow={uploadingNow} />
      <div className="drop-zone" id="dropZone" hidden={!drag || !current}>Drop files to attach</div>
    </main>
  );
}
