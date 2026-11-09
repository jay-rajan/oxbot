// The chat log: every stored event as the hand-written page drew it, the
// reply streaming in, the "Working…" line, meters, report cards, the owner's
// thumbs, and the blurred picture of someone else's private bot.
import { Fragment, type ReactNode, memo, useCallback, useEffect, useLayoutEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import type { BotEvent, BotInfo } from "../../net/protocol";
import { type BotChat, type Meter, type Ratings, newBotChat } from "../../state/chat";
import { local } from "../../state/store";
import { isLocked, maybeLoadOlder, openReport, sizeText } from "./actions";
import { type MdNode, md } from "./md";
import { Board } from "../../components/Board";
import { Button, Chip, vars } from "../../components/ui";

const LOCK = (
  <svg className="lock" viewBox="0 0 12 12" aria-label="Private" role="img">
    <rect x="2" y="5.2" width="8" height="5.8" rx="1.3" fill="currentColor" />
    <path d="M3.9 5.4V3.9a2.1 2.1 0 0 1 4.2 0v1.5" fill="none" stroke="currentColor" strokeWidth="1.4" />
  </svg>
);

export const money = (c: number) => (c > 0 && c < 0.01 ? "<$0.01" : `$${c.toFixed(2)}`);

function MdNodes({ nodes }: { nodes: MdNode[] }): ReactNode {
  return nodes.map((n, i) => {
    if (typeof n === "string") return <Fragment key={i}>{n}</Fragment>;
    const kids = <MdNodes nodes={n.children} />;
    switch (n.tag) {
      case "br": return <br key={i} />;
      case "p": return <p key={i}>{kids}</p>;
      case "pre": return <pre key={i}>{kids}</pre>;
      case "code": return <code key={i}>{kids}</code>;
      case "strong": return <strong key={i}>{kids}</strong>;
      case "em": return <em key={i}>{kids}</em>;
      case "a": return <a key={i} href={n.href} target="_blank" rel="noopener noreferrer">{kids}</a>;
      case "h3": return <h3 key={i}>{kids}</h3>;
      case "h4": return <h4 key={i}>{kids}</h4>;
      case "ul": return <ul key={i}>{kids}</ul>;
      case "ol": return <ol key={i}>{kids}</ol>;
      case "li": return <li key={i}>{kids}</li>;
      case "table": return <div key={i} className="md-table"><table>{kids}</table></div>;
      case "thead": return <thead key={i}>{kids}</thead>;
      case "tbody": return <tbody key={i}>{kids}</tbody>;
      case "tr": return <tr key={i}>{kids}</tr>;
      case "th": return <th key={i}>{kids}</th>;
      case "td": return <td key={i}>{kids}</td>;
    }
  });
}

export function MdView({ text }: { text: string }) {
  return <MdNodes nodes={md(text)} />;
}

/** What one event needs from around it. */
interface Ctx {
  people: Record<string, string>;
  homeComputer: boolean;
  /** The owner's thumbs (null: not shown). */
  rated: Ratings | null;
}

const OUTCOME: Record<string, string> = { allowed: "You allowed", allowed_always: "You always allowed", denied: "You said no to", timed_out: "No answer, so no to" };

function RateRow({ e, r }: { e: BotEvent; r: Ratings }) {
  const { send, dispatch } = useApp();
  const up = r.ratings[e.seq];
  const kept = r.cases.includes(e.seq);
  return (
    <span className="rate-row">
      {([["👍", true, "Good reply"], ["👎", false, "Bad reply"]] as const).map(([label, value, title]) => (
        <Button key={label} kind="quiet" size="sm" title={title} aria-pressed={up === value} onClick={() => send({ type: "rate_turn", bot: e.bot, seq: e.seq, up: up === value ? null : value })}>
          {label}
        </Button>
      ))}
      <Button
        kind="quiet"
        size="sm"
        title={kept ? "Kept as a test case: see Compare in the bot's menu" : "Keep this task to replay on other models"}
        onClick={() => {
          if (kept) dispatch(local("ui/open", { dialog: "compare", arg: `${e.bot}:cmpCases` }));
          else {
            // The draft comes back as case_draft; the case dialog shows it.
            send({ type: "draft_test_case", bot: e.bot, seq: e.seq });
            dispatch(local("ui/open", { dialog: "case", arg: `${e.bot}:${e.seq}` }));
          }
        }}
      >
        {kept ? "✓ test case" : "Test case…"}
      </Button>
    </span>
  );
}

function ReportCard({ e }: { e: Extract<BotEvent, { kind: "report" }> }) {
  const app = useApp();
  const open = () => openReport(app, e.bot, e.id);
  return (
    <div
      className="report-card"
      role="button"
      tabIndex={0}
      aria-label={`Open the report: ${e.title}`}
      title={new Date(e.ts_ms).toLocaleString()}
      onClick={open}
      onKeyDown={(ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); open(); } }}
    >
      <div className="kicker">Report</div>
      <div className="title">{e.title}</div>
      <div className="summary">{e.summary}</div>
      <div className="when">{`${new Date(e.ts_ms).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" })} · ${e.sources} source${e.sources === 1 ? "" : "s"}`}</div>
    </div>
  );
}

/** The files sent with a message. On the home computer a click opens one there. */
function SentFiles({ e, homeComputer }: { e: Extract<BotEvent, { kind: "user_message" }>; homeComputer: boolean }) {
  const { send } = useApp();
  return (
    <div className="msg-files">
      {e.files.map((f) => {
        const sub = f.kind === "video" ? `Bots can't watch video: ${f.note || "still frames"} sent` : f.note || sizeText(f.size);
        return homeComputer ? (
          <Chip key={f.id} sub={sub} title="Open it on the home computer" onClick={() => send({ type: "open_file", bot: e.bot, seq: e.seq, id: f.id })}>{f.name}</Chip>
        ) : (
          <Chip key={f.id} sub={sub} title="Files open on the home computer">{f.name}</Chip>
        );
      })}
    </div>
  );
}

/** One stored event, or nothing for kinds the chat doesn't show. */
const EventView = memo(function EventView({ e, ctx }: { e: BotEvent; ctx: Ctx }) {
  const title = new Date(e.ts_ms).toLocaleString();
  switch (e.kind) {
    case "user_message": {
      const color = e.from ? ctx.people[e.from] || "var(--muted)" : undefined;
      return (
        <div className="mine" title={title} style={color ? vars({ person: color }) : undefined}>
          {e.from ? <div className="who"><i className="person-dot" />{e.from}</div> : null}
          {e.text || !e.files?.length ? <div className="msg me">{e.text}</div> : null}
          {e.files?.length ? <SentFiles e={e} homeComputer={ctx.homeComputer} /> : null}
        </div>
      );
    }
    case "assistant_message": return <div className="msg bot" title={title}><MdView text={e.text} /></div>;
    case "tool_use": return <div className="tool" title={title}>{`${e.name} ${e.summary}`.trim()}</div>;
    case "turn_done":
      return (
        <div className="done" title={title}>
          {e.ok ? `done in ${(e.duration_ms / 1000).toFixed(1)}s` : "stopped"}
          {e.seq && ctx.rated ? <RateRow e={e} r={ctx.rated} /> : null}
        </div>
      );
    // Only a practice run's own transcript has these.
    case "dry_run": return <div className="dryline" title={title}>{`would ${e.action} ${e.target} on ${e.site}`}</div>;
    case "net_blocked": return <div className="dryline" title={title}>{`blocked a ${e.method} to ${e.host}`}</div>;
    case "error": return <div className="err" title={title}>{e.message}</div>;
    case "approval": return <div className="redline" title={title}>{`⚠ ${OUTCOME[e.outcome] || e.outcome}: ${e.action} ${e.target} on ${e.site}`}</div>;
    case "report": return <ReportCard e={e} />;
    case "blocked": return <div className="redline" title={title}>{`⚠ ${e.message}`}</div>;
    case "allowed_by_rule": return <div className="tool" title={title}>{`always allowed: ${e.action} ${e.target} on ${e.site}`}</div>;
    case "note": return <div className="tool note-line" title={title}>{e.text}</div>;
    case "sign_in": {
      const t = { done: `Signed in to ${e.site}`, cancelled: `Didn't sign in to ${e.site}`, timed_out: `No answer; didn't sign in to ${e.site}` }[e.outcome] || e.site;
      return <div className="signline" title={title}>{t}</div>;
    }
    default: return null;
  }
});

function MeterView({ m }: { m: Meter }) {
  return <div className="meter">{`· ${m.model.replace(/^.*\//, "")} · ${m.requests} question${m.requests === 1 ? "" : "s"} · ${money(m.cost_usd || 0)}`}</div>;
}

function LockedView({ info }: { info: BotInfo }) {
  // Nothing of the chat is sent here: these blurred lines are only a picture.
  const maker = info.created_by || "its maker";
  return (
    <div className="locked-view">
      {([[70, false], [45, true], [85, false], [55, true], [65, false]] as const).map(([w, me], i) => (
        <div key={i} className={`ghostline${me ? " me" : ""}`} style={vars({ w: `${w}%` })} />
      ))}
      <div className="note">
        {LOCK}
        <div>{`${maker}'s private bot`}</div>
        <small>{info.sharing === "people" ? `Only ${maker} and the people they shared it with can open it.` : `Only ${maker} can open it.`}</small>
      </div>
    </div>
  );
}

const EMPTY_PEOPLE: Record<string, string> = {};

export function Log() {
  const app = useApp();
  const current = useSel((s) => s.ui.current);
  const info = useSel((s) => (s.ui.current ? s.bots.byId[s.ui.current] : undefined));
  const chat = useSel((s) => (s.ui.current ? s.chat.bots[s.ui.current] : undefined));
  const peopleList = useSel((s) => s.home.people);
  const settings = useSel((s) => s.connection.settings);
  const ratings = useSel((s) => (s.ui.current ? s.chat.ratings[s.ui.current] : undefined));
  const ref = useRef<HTMLDivElement>(null);
  // The chat follows new messages while the user is at the bottom; once
  // they scroll up to read, it stays where they are.
  const atBottom = useRef(true);
  const fromBottom = useRef(0);
  const shown = useRef<{ bot: string | null; firstSeq: number }>({ bot: null, firstSeq: 0 });

  const people = peopleList.length ? Object.fromEntries(peopleList.map((p) => [p.name, p.color])) : EMPTY_PEOPLE;
  const locked = isLocked(info);
  const owner = !!(settings?.owner && settings.dev_tools);
  const comparable = !!info && !info.central && !locked;
  const ctxRated = owner && comparable ? (ratings ?? { ratings: {}, cases: [] }) : null;
  const homeComputer = !!settings?.home_computer;
  // Kept the same object while nothing in it changed, so events don't redraw.
  const ctxRef = useRef<Ctx | null>(null);
  const prev = ctxRef.current;
  const ctx: Ctx = prev && prev.people === people && prev.homeComputer === homeComputer && prev.rated === ctxRated ? prev : { people, homeComputer, rated: ctxRated };
  ctxRef.current = ctx;

  const scrollDown = useCallback((force = false) => {
    const log = ref.current;
    if (!log) return;
    if (force || atBottom.current) { log.scrollTop = log.scrollHeight; atBottom.current = true; }
    fromBottom.current = log.scrollHeight - log.scrollTop;
  }, []);

  const b: BotChat = chat ?? newBotChat();
  // After each change: an earlier page drawn above keeps what's on screen in
  // place; a new bot starts at the bottom; otherwise follow if at the bottom.
  useLayoutEffect(() => {
    const log = ref.current;
    if (!log) return;
    const was = shown.current;
    if (was.bot !== current) {
      atBottom.current = true;
      scrollDown(true);
      // Pictures and code blocks can grow the chat after it's drawn.
      requestAnimationFrame(() => scrollDown());
    } else if (b.firstSeq && was.firstSeq && b.firstSeq < was.firstSeq && b.events.length) {
      log.scrollTop = log.scrollHeight - fromBottom.current;
    } else scrollDown();
    shown.current = { bot: current, firstSeq: b.firstSeq };
    fromBottom.current = log.scrollHeight - log.scrollTop;
  });

  // After a history page for the open chat: page back if near the top.
  useEffect(() => {
    if (!current || !b.histN) return;
    const raf = requestAnimationFrame(() => maybeLoadOlder(app, ref.current));
    return () => cancelAnimationFrame(raf);
  }, [app, current, b.histN]);

  // Cards opening below the log make it shorter: keep the latest message in view.
  useEffect(() => {
    const log = ref.current;
    if (!log || typeof ResizeObserver !== "function") return;
    const ro = new ResizeObserver(() => scrollDown());
    ro.observe(log);
    return () => ro.disconnect();
  }, [scrollDown]);

  const onScroll = () => {
    const log = ref.current;
    if (!log) return;
    atBottom.current = log.scrollHeight - log.scrollTop - log.clientHeight < 160;
    fromBottom.current = log.scrollHeight - log.scrollTop;
    maybeLoadOlder(app, log);
  };

  let body: ReactNode = null;
  if (current && info && locked) body = <LockedView info={info} />;
  else if (current) {
    const empty = b.events.length === 0 && !b.stream;
    const meters = new Map<number, Meter[]>();
    for (const m of b.meters) meters.set(m.after, [...(meters.get(m.after) ?? []), m.meter]);
    const where = info?.private ? `${info.created_by || "its maker"}'s own browser` : "the home browser";
    body = (
      <>
        {empty ? (
          <div className="empty">{`Send a message to start. The bot follows its instructions, browses the web in ${where}, and keeps files in its own folder.`}</div>
        ) : (
          b.events.map((e) => (
            <Fragment key={e.seq}>
              <EventView e={e} ctx={ctx} />
              {meters.get(e.seq)?.map((m, i) => <MeterView key={i} m={m} />)}
            </Fragment>
          ))
        )}
        {b.stream ? <div className="msg bot streaming"><MdView text={b.stream} /></div> : null}
        {b.busy && !b.stream ? (
          <div className="typing">
            <span className="ttt"><Board /></span>
            <span className="txt">{(b.activity || "Working…").replace(/…$/, "")}</span>
          </div>
        ) : null}
      </>
    );
  }

  return (
    // biome-ignore lint: a picture finishing loading scrolls the chat (load doesn't bubble)
    <div className="log" id="log" aria-live="polite" ref={ref} onScroll={onScroll} onLoadCapture={() => scrollDown()}>
      {body}
    </div>
  );
}

