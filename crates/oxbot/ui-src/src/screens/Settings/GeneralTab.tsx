// Settings → General: what's missing on the home computer, notifications,
// the home's AI account, startup, and about.
import { type FormEvent, useEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { Banner, Button, cx, Field, Hint, Input, pct, Section, Switch, vars } from "../../components/ui";
import { local } from "../../state/store";
import { isOwner } from "../Compare/devTools";
import { fixParts, missingChecks, money, setupReady } from "./format";

export const KEYCHAIN_WAITING = "Waiting for the Mac's Keychain: allow oxbot in the dialog.";

export function GeneralTab({ hidden }: { hidden: boolean }) {
  return (
    <div id="setGeneral" className="settings-pane" role="tabpanel" hidden={hidden}>
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
  const homeComputer = useSel((s) => !!s.connection.settings?.home_computer);
  const ready = setupReady(setup);
  const optionalOff = !!(setup && homeComputer && setup.checks.some((c) => !c.ok && c.optional));
  return (
    <Section id="setupSection" hidden={ready && !optionalOff}>
      <Banner
        tone={ready ? "info" : "warn"}
        actions={
          <Button size="sm" id="setupAgain2" onClick={() => send({ type: "check_setup" })}>
            Check again
          </Button>
        }
      >
        <p className={cx("setup-line", ready && "optional")} id="setupLine">
          {ready ? "Optional, not turned on:" : "The home computer needs a fix:"}
        </p>
        <ul className="checks" id="setupList">
          {missingChecks(setup, optionalOff).map((c) => (
            <li key={c.id} className={c.ok ? "ok" : c.optional ? "opt" : "bad"}>
              <span aria-hidden="true">{c.ok ? "✓" : c.optional ? "–" : "✕"}</span>
              <span>
                <b>{c.name}</b>
                {` · ${c.detail}`}
                {c.fix ? (
                  <span className="fix">
                    {fixParts(c.fix).map((p, i) => (p.code ? <code key={i}>{p.text}</code> : p.text))}
                  </span>
                ) : null}
              </span>
            </li>
          ))}
        </ul>
      </Banner>
    </Section>
  );
}

// Notifications: everyone sees them; only the home computer changes them.
function NotifySection() {
  const { send } = useApp();
  const settings = useSel((s) => s.connection.settings);
  return (
    <Section id="notifySection" title="Notifications">
      <Switch
        id="usageAlerts"
        checked={!!settings && settings.usage_alerts !== false}
        disabled={!!settings && !settings.home_computer}
        onChange={(e) => send({ type: "set_usage_alerts", on: e.target.checked })}
      >
        Tell me when AI use passes 50% and 80% of a limit
      </Switch>
      {/* A bot waiting for an OK or a sign-in while oxbot is hidden always notifies. */}
      <Hint id="notifyHint">
        {!settings ? "" : settings.home_computer ? "Mac notifications need permission in System Settings → Notifications." : "Set on the home computer."}
      </Hint>
    </Section>
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

  const homeComputer = !!settings?.home_computer;
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
        ? homeComputer
          ? "Not connected. Bots need an AI account to think."
          : `Not connected yet; ask ${owner}.`
        : "Connected to OpenRouter · free models first";

  // This month's paid AI against its limit, and today's free questions.
  const prefs = or?.prefs;
  const key = or?.key ?? null;
  const cap = (prefs?.paid_monthly_cents ?? 0) / 100;
  const spent = or?.spent_this_month_usd ?? 0;
  const used = cap > 0 ? Math.min(100, (spent / cap) * 100) : 0;
  const parts = [key?.is_free_tier ? "No credit bought: free models only" : `${money(spent)} of ${money(cap)} paid AI this month`];
  if (key?.free_limit) parts.push(`${key.free_used ?? 0} of ${key.free_limit} free questions today`);
  const capShown = signed && homeComputer && !key?.is_free_tier;

  // The limit field follows the server, except while it's being typed in.
  useEffect(() => {
    const input = capRef.current;
    if (input && signed && !capFormRef.current?.contains(document.activeElement)) input.value = String(Math.round(cap));
  }, [signed, cap]);

  // The AI helper on the home computer: only when something's wrong.
  const helper = engines?.codex ?? null;
  const cxBad = !!(helper && settings && !settings.codex_ready && signed);

  const saveCap = (ev: FormEvent) => {
    ev.preventDefault();
    if (!or) return;
    const dollars = Math.max(0, Math.min(500, Math.round(Number(capRef.current?.value || 0))));
    send({ type: "set_engine_prefs", prefs: { ...or.prefs, paid_fallback: true, paid_monthly_cents: dollars * 100 } });
    capRef.current?.blur();
    toast("Saved.");
  };

  return (
    <Section id="aiSection" title="AI">
      <div className="ai-line">
        <i className={cx("dot", signed && ready && "on")} id="orDot" aria-hidden="true" />
        <span id="orStatus">{status}</span>
        <span className="spacer" />
        <Button kind="primary" id="orSignIn" hidden={!homeComputer || signed || waiting || !or} onClick={() => send({ type: "open_router_sign_in" })}>
          Connect OpenRouter
        </Button>
        <Button size="sm" id="orSignOut" hidden={!homeComputer || !signed} onClick={() => send({ type: "open_router_sign_out" })}>
          Sign out
        </Button>
      </div>
      <Hint id="orLink" hidden={signed || !orAuthUrl}>
        If no window opened,{" "}
        <a id="orLinkA" href={orAuthUrl ?? "#"} target="_blank" rel="noopener noreferrer">
          open this link
        </a>
        .
      </Hint>
      <div className="ai-month" id="orMonth" hidden={!signed}>
        <div className="meter-bar">
          <i id="orMonthBar" className={used >= 80 ? "high" : undefined} style={vars({ fill: pct(used, 100) })} />
        </div>
        <small id="orMonthText">{signed ? parts.join(" · ") : ""}</small>
      </div>
      <form className="cap-form" id="orCapForm" hidden={!capShown} ref={capFormRef} onSubmit={saveCap}>
        <Field label="Monthly limit for paid AI ($)">
          <Input type="number" id="orPaidCap" min="0" max="500" step="1" inputMode="decimal" ref={capRef} />
        </Field>
        <Button type="submit">Save</Button>
      </form>
      <Banner tone="warn" id="cxStatus" hidden={!cxBad}>
        {cxBad ? helper?.problem || "The AI helper isn't ready on the home computer." : ""}
      </Banner>
    </Section>
  );
}

function LoginSection() {
  const { send } = useApp();
  const at = useSel((s) => s.connection.settings?.launch_at_login);
  return (
    <Section id="loginSection" title="Startup" hidden={at === null || at === undefined}>
      <Switch id="launchAtLogin" checked={!!at} onChange={(e) => send({ type: "set_launch_at_login", on: e.target.checked })}>
        Open oxbot when I log in to my Mac
      </Switch>
    </Section>
  );
}

function AboutSection() {
  const { dispatch } = useApp();
  const mac = useSel((s) => s.connection.serverName);
  const settings = useSel((s) => s.connection.settings);
  const people = useSel((s) => s.home.people);
  const you = settings?.you;
  const color = people.find((p) => p.name === you)?.color;
  return (
    <Section title="About">
      <p className="about-line">
        <span id="aboutMac">{mac || "…"}</span> · version <span id="aboutVersion">{settings ? settings.version : "…"}</span> ·{" "}
        <span id="aboutYou">
          {you ? (
            <>
              <i className="person-dot" style={vars({ person: color })} />
              {you}
            </>
          ) : null}
        </span>
      </p>
      <p className="field-hint mono" id="aboutData">
        {settings ? settings.data_dir : "…"}
      </p>
      {isOwner(settings) ? (
        <div className="btn-row">
          <Button size="sm" kind="quiet" id="openPieces" onClick={() => dispatch(local("ui/open", { dialog: "pieces" }))}>
            Design pieces
          </Button>
        </div>
      ) : null}
    </Section>
  );
}
