// Settings → General: what's missing on the home computer, notifications,
// the home's AI account, startup, and about.
import { type FormEvent, useEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { fixParts, missingChecks, money, setupReady } from "./format";

export const KEYCHAIN_WAITING = "Waiting for the Mac's Keychain: allow oxbot in the dialog.";

export function GeneralTab({ hidden }: { hidden: boolean }) {
  return (
    <div id="setGeneral" role="tabpanel" hidden={hidden}>
      <SetupSection />
      <NotifySection />
      <AiSection />
      <LoginSection />
      <AboutSection />
    </div>
  );
}

// What's missing, with what to do about it. On the home computer, optional
// parts that are off (the microphone) are listed too, so a missing button
// has a reason and a fix.
function SetupSection() {
  const { send } = useApp();
  const setup = useSel((s) => s.home.setup);
  const farm = useSel((s) => !!s.connection.settings?.farm);
  const ready = setupReady(setup);
  const optionalOff = !!(setup && farm && setup.checks.some((c) => !c.ok && c.optional));
  return (
    <section id="setupSection" hidden={ready && !optionalOff}>
      <p className={`setup-line${ready ? " optional" : ""}`} id="setupLine">
        {ready ? "Optional, not turned on:" : "Something on the home computer needs a fix:"}
      </p>
      <ul className="checks" id="setupList">
        {missingChecks(setup, optionalOff).map((c) => (
          <li key={c.id} className={c.ok ? "ok" : c.optional ? "opt" : "bad"}>
            <span>{c.ok ? "✓" : c.optional ? "–" : "✕"}</span>
            <span>
              <b>{c.name}</b>
              {` · ${c.detail}`}
              {c.fix ? (
                <div className="fix">
                  {fixParts(c.fix).map((p, i) => (p.code ? <code key={i}>{p.text}</code> : p.text))}
                </div>
              ) : null}
            </span>
          </li>
        ))}
      </ul>
      <div className="btns">
        <button className="ghost" type="button" id="setupAgain2" onClick={() => send({ type: "check_setup" })}>
          Check again
        </button>
      </div>
    </section>
  );
}

// Notifications: everyone sees them; only the home computer changes them.
function NotifySection() {
  const { send } = useApp();
  const settings = useSel((s) => s.connection.settings);
  return (
    <section id="notifySection">
      <h3>Notifications</h3>
      <label className="check">
        <input
          type="checkbox"
          id="usageAlerts"
          checked={!!settings && settings.usage_alerts !== false}
          disabled={!!settings && !settings.farm}
          onChange={(e) => send({ type: "set_usage_alerts", on: e.target.checked })}
        />{" "}
        Tell me when AI use passes 50% and 80% of a limit
      </label>
      <p className="hint" id="notifyHint">
        {/* A bot waiting for an OK or a sign-in while oxbot is hidden always notifies. */}
        {!settings
          ? ""
          : settings.farm
            ? "oxbot also tells you when a bot is waiting for you while its window is hidden. Mac notifications need permission in System Settings → Notifications."
            : "Set on the home computer."}
      </p>
    </section>
  );
}

// The home's AI account, this month's use, and (with credit bought) the
// monthly limit for paid AI. Models are picked automatically.
function AiSection() {
  const { send, toast } = useApp();
  const settings = useSel((s) => s.connection.settings);
  const engines = useSel((s) => s.home.engines);
  const orAuthUrl = useSel((s) => s.home.orAuthUrl);
  const capRef = useRef<HTMLInputElement>(null);
  const capFormRef = useRef<HTMLFormElement>(null);

  const farm = !!settings?.farm;
  const or = engines?.openrouter ?? null;
  const waiting = !!settings?.keychain_waiting;
  const signed = !!or?.signed_in;
  const ready = !!settings?.openrouter_ready;
  const owner = settings?.owner_name || engines?.owner || "the owner";
  const status = waiting
    ? KEYCHAIN_WAITING
    : !or
      ? ready
        ? "Connected to OpenRouter."
        : "Checking…"
      : !signed
        ? farm
          ? "Not connected. Bots need an AI account to think."
          : `Not connected yet; ask ${owner}.`
        : "Connected to OpenRouter · free models first";

  // This month's paid AI against its limit, and today's free questions.
  const prefs = or?.prefs;
  const key = or?.key ?? null;
  const cap = (prefs?.paid_monthly_cents ?? 0) / 100;
  const spent = or?.spent_this_month_usd ?? 0;
  const pct = cap > 0 ? Math.min(100, (spent / cap) * 100) : 0;
  const parts = [key?.is_free_tier ? "No credit bought: free models only" : `${money(spent)} of ${money(cap)} paid AI this month`];
  if (key?.free_limit) parts.push(`${key.free_used ?? 0} of ${key.free_limit} free questions today`);
  const capShown = signed && farm && !key?.is_free_tier;

  // The limit field follows the server, except while it's being typed in.
  useEffect(() => {
    const input = capRef.current;
    if (input && signed && !capFormRef.current?.contains(document.activeElement)) input.value = String(Math.round(cap));
  }, [signed, cap]);

  // The AI helper on the home computer: only when something's wrong.
  const cx = engines?.codex ?? null;
  const cxBad = !!(cx && settings && !settings.codex_ready && signed);

  const saveCap = (ev: FormEvent) => {
    ev.preventDefault();
    if (!or) return;
    const dollars = Math.max(0, Math.min(500, Math.round(Number(capRef.current?.value || 0))));
    send({ type: "set_engine_prefs", prefs: { ...or.prefs, paid_fallback: true, paid_monthly_cents: dollars * 100 } });
    capRef.current?.blur();
    toast("Saved.");
  };

  return (
    <section id="aiSection">
      <h3>AI</h3>
      <div className="ai-line">
        <i className={`gate-dot${signed && ready ? " on" : ""}`} id="orDot" aria-hidden="true" />
        <span id="orStatus">{status}</span>
        <span style={{ flex: 1 }} />
        <button className="primary" type="button" id="orSignIn" hidden={!farm || signed || waiting || !or} onClick={() => send({ type: "open_router_sign_in" })}>
          Connect OpenRouter
        </button>
        <button className="ghost" type="button" id="orSignOut" hidden={!farm || !signed} onClick={() => send({ type: "open_router_sign_out" })}>
          Sign out
        </button>
      </div>
      <p className="hint" id="orLink" hidden={signed || !orAuthUrl}>
        If no window opened,{" "}
        <a id="orLinkA" href={orAuthUrl ?? "#"} target="_blank" rel="noopener noreferrer">
          open this link
        </a>
        .
      </p>
      <div className="ai-month" id="orMonth" hidden={!signed}>
        <div className="meter-bar">
          <i id="orMonthBar" className={pct >= 80 ? "high" : undefined} style={{ width: `${pct}%` }} />
        </div>
        <span className="hint" id="orMonthText">
          {signed ? parts.join(" · ") : ""}
        </span>
      </div>
      <form className="add-site" id="orCapForm" hidden={!capShown} ref={capFormRef} onSubmit={saveCap}>
        <label className="cap-label" htmlFor="orPaidCap">
          Monthly limit for paid AI ($)
        </label>
        <input type="number" id="orPaidCap" min="0" max="500" step="1" inputMode="decimal" ref={capRef} />
        <button className="ghost" type="submit">
          Save
        </button>
      </form>
      <p className="hint" id="cxStatus" hidden={!cxBad}>
        {cxBad ? cx?.problem || "The AI helper isn't ready on the home computer." : ""}
      </p>
    </section>
  );
}

function LoginSection() {
  const { send } = useApp();
  const at = useSel((s) => s.connection.settings?.launch_at_login);
  return (
    <section id="loginSection" hidden={at === null || at === undefined}>
      <h3>Startup</h3>
      <label className="check">
        <input type="checkbox" id="launchAtLogin" checked={!!at} onChange={(e) => send({ type: "set_launch_at_login", on: e.target.checked })} />
        {" Open oxbot when I log in to my Mac"}
      </label>
    </section>
  );
}

function AboutSection() {
  const mac = useSel((s) => s.connection.serverName);
  const settings = useSel((s) => s.connection.settings);
  const people = useSel((s) => s.home.people);
  const you = settings?.you;
  const color = people.find((p) => p.name === you)?.color || "var(--muted)";
  return (
    <section>
      <h3>About</h3>
      <p className="about-line">
        <span id="aboutMac">{mac || "…"}</span> · version <span id="aboutVersion">{settings ? settings.version : "…"}</span> ·{" "}
        <span id="aboutYou">
          {you ? (
            <>
              <i className="you-dot" style={{ background: color }} />
              {you}
            </>
          ) : null}
        </span>
      </p>
      <p className="hint mono" id="aboutData">
        {settings ? settings.data_dir : "…"}
      </p>
    </section>
  );
}
