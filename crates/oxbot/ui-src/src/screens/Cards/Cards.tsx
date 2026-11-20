// Cards: sign-ins, changes (red), uploads (amber), new bots, tasks for
// private bots, scripts and schedules. Nothing happens until the user
// answers. A bot's cards show in its own chat, oldest first, so the newest
// sits right above the message box; other bots waiting for an answer get one
// line above them (Waiting), with Open to go there.
import { type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
import { FLAT, panFrom, toggleAt, toPage, type View } from "./zoom";
import { useApp, useSel } from "../../app/context";
import { Banner, Button, type ButtonKind, Card, Checkbox, Field, Input, Select, TextArea, vars } from "../../components/ui";
import type { Answer, ClientMsg, ScheduleEdit, SignInInput } from "../../net/protocol";
import {
  type CardApproval,
  type CardSignIn,
  type SignInFrame,
  cardList,
  DAYS,
  editOf,
  ruleOf,
  SCHED_KINDS,
  type SchedEdit,
  splitCards,
} from "../../state/cards";
import { local } from "../../state/store";
import { expose, exposeFn } from "../../web/testBridge";
import "./cards.css";

/** Seconds left of `total`, counting down from when it first showed. */
function useCountdown(total: number): number {
  const [left, setLeft] = useState(total);
  useEffect(() => {
    const start = Date.now();
    const tick = setInterval(() => {
      setLeft(Math.max(0, total - Math.floor((Date.now() - start) / 1000)));
    }, 1000);
    return () => clearInterval(tick);
  }, [total]);
  return left;
}

/** A long task checking in: it keeps going when the time runs out. */
function CheckIn({ who, steps, seconds, onGo, onStop }: { who: string; steps: string; seconds: number; onGo: () => void; onStop: () => void }) {
  const left = useCountdown(seconds);
  return (
    <Card className="checkpoint" title={`${who} has taken ${steps}`}>
      <p className="plan-text">It wrote where it is in the chat. It keeps going in {left} s unless you stop it.</p>
      <div className="btn-row btns">
        <Btn label="Keep going" kind="primary" onClick={onGo} />
        <Btn label="Stop" kind="secondary" cls="deny" onClick={onStop} />
      </div>
    </Card>
  );
}

/** How often the sign-in picture refreshes, in ms. */
const FRAME_MS = 500;
/** Two taps this close together are a double tap (zoom), in ms. */
const DOUBLE_MS = 280;

/** The sign-in page, live from the home computer: tap it, type into it.
 * Works on any device; the home computer does the browsing. On a phone the
 * picture is small, so a double tap zooms in on that spot (drag to move,
 * double tap again to zoom out); a single tap clicks there. */
function SignInView({ id, frame, send }: { id: number; frame: SignInFrame | undefined; send: (m: ClientMsg) => void }) {
  const [text, setText] = useState("");
  const [view, setView] = useState<View>(FLAT);
  const [dragging, setDragging] = useState(false);
  const viewRef = useRef(view);
  viewRef.current = view;
  const box = useRef<HTMLDivElement>(null);
  const press = useRef<{ x: number; y: number; start: View; moved: boolean } | null>(null);
  const lastTap = useRef<{ t: number; x: number; y: number } | null>(null);
  const pending = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const ask = () => {
      if (!document.hidden) send({ type: "sign_in_frame", id });
    };
    ask();
    const tick = setInterval(ask, FRAME_MS);
    return () => {
      clearInterval(tick);
      if (pending.current) clearTimeout(pending.current);
    };
  }, [id, send]);
  const input = (input: SignInInput) => send({ type: "sign_in_input", id, input });
  const where = (e: React.PointerEvent) => {
    const r = box.current!.getBoundingClientRect();
    return { px: e.clientX - r.left, py: e.clientY - r.top, w: r.width, h: r.height };
  };
  const down = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!frame || !box.current) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    press.current = { x: e.clientX, y: e.clientY, start: viewRef.current, moved: false };
  };
  const move = (e: React.PointerEvent<HTMLDivElement>) => {
    const p = press.current;
    if (!p || !box.current) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    if (!p.moved && Math.hypot(dx, dy) < 8) return;
    p.moved = true;
    if (p.start.s > 1) {
      setDragging(true);
      const { w, h } = where(e);
      setView(panFrom(p.start, dx, dy, w, h));
    }
  };
  const up = (e: React.PointerEvent<HTMLDivElement>) => {
    const p = press.current;
    press.current = null;
    setDragging(false);
    if (!p || p.moved || !frame || !box.current) return;
    const { px, py, w, h } = where(e);
    const now = Date.now();
    const last = lastTap.current;
    if (last && now - last.t < DOUBLE_MS && Math.hypot(px - last.x, py - last.y) < 40) {
      // A double tap: zoom, and the first tap doesn't click.
      if (pending.current) clearTimeout(pending.current);
      pending.current = null;
      lastTap.current = null;
      setView(toggleAt(viewRef.current, px, py, w, h));
      return;
    }
    lastTap.current = { t: now, x: px, y: py };
    const at = toPage(viewRef.current, px, py, w, h, frame.width, frame.height);
    pending.current = setTimeout(() => {
      pending.current = null;
      input({ kind: "tap", ...at });
    }, DOUBLE_MS);
  };
  const typeOnly = () => {
    if (!text) return;
    input({ kind: "text", text });
    setText("");
  };
  // Types what's in the box (if anything), then presses Enter.
  const sendText = () => {
    if (text) input({ kind: "text", text });
    input({ kind: "key", key: "Enter" });
    setText("");
  };
  const zoomed = view.s > 1;
  return (
    <div className="signin-view">
      {frame ? (
        <>
          <div className="where">{frame.url}</div>
          <div
            ref={box}
            className={`signin-box${zoomed ? " zoomed" : ""}`}
            onPointerDown={down}
            onPointerMove={move}
            onPointerUp={up}
            onPointerCancel={() => {
              press.current = null;
              setDragging(false);
            }}
          >
            <img
              className={`signin-page${dragging ? " dragging" : ""}`}
              alt="The sign-in page"
              draggable={false}
              src={`data:image/jpeg;base64,${frame.image}`}
              style={vars({ zx: `${view.x}px`, zy: `${view.y}px`, zs: view.s })}
            />
          </div>
          <div className="signin-hint">{zoomed ? "Drag to move · double-tap to zoom out" : "Tap to click · double-tap to zoom in"}</div>
        </>
      ) : (
        <div className="where">Opening the sign-in page…</div>
      )}
      <div className="signin-type">
        <Input
          id={`signin-text-${id}`}
          value={text}
          placeholder="Tap a field above, then type here"
          autoComplete="off"
          enterKeyHint="send"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              sendText();
            }
          }}
        />
        <Btn label="Send" kind="primary" onClick={sendText} />
      </div>
      <div className="signin-keys">
        <Btn label="Type only" kind="quiet" onClick={typeOnly} />
        <Btn label="Tab" kind="quiet" onClick={() => input({ kind: "key", key: "Tab" })} />
        <Btn label="⌫" kind="quiet" onClick={() => input({ kind: "key", key: "Backspace" })} />
        <Btn label="▲" kind="quiet" onClick={() => input({ kind: "scroll", dy: -400 })} />
        <Btn label="▼" kind="quiet" onClick={() => input({ kind: "scroll", dy: 400 })} />
      </div>
    </div>
  );
}

/** A card's button; `cls` names it for the eye ("deny") as before. */
function Btn({ label, kind, cls, onClick }: { label: string; kind: ButtonKind; cls?: string; onClick: () => void }) {
  return (
    <Button kind={kind} className={cls} onClick={onClick}>
      {label}
    </Button>
  );
}

/** Where the card area shows the newest card from its heading down (the bottom, when that card fits). */
function newestTop(box: HTMLElement): number {
  const last = box.lastElementChild;
  if (!last) return 0;
  const want = box.scrollTop + last.getBoundingClientRect().top - box.getBoundingClientRect().top - 10;
  return Math.max(0, Math.min(want, box.scrollHeight - box.clientHeight));
}

export function Cards() {
  const { store, send } = useApp();
  const cards = useSel((s) => s.cards);
  const byId = useSel((s) => s.bots.byId);
  const current = useSel((s) => s.ui.current);
  const all = cardList(cards);
  const list = splitCards(all, current, (id) => id in byId).here;
  const botName = (id: string) => byId[id]?.name || id;

  const boxRef = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const shown = useRef(new Set<string>());
  const bigShots = useRef(new Set<string>()); // cards whose screenshot the user enlarged

  useEffect(() => {
    expose("approvals", () => new Map(Object.values(store.getState().cards.approvals).map((a) => [a.id, a])));
    // The page redraws itself; tests call this to check a redraw keeps the scroll.
    exposeFn("renderCards", () => {});
  }, [store]);

  const keys = list.map((c) => c.key).join(",");
  useLayoutEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const now = keys ? keys.split(",") : [];
    const fresh = now.some((k) => !shown.current.has(k));
    shown.current = new Set(now);
    for (const k of [...bigShots.current]) if (!shown.current.has(k)) bigShots.current.delete(k);
    // A new card scrolls into view; otherwise stay where the user was.
    if (fresh) {
      box.scrollTop = newestTop(box);
      follow.current = true;
    } else if (follow.current) box.scrollTop = Math.max(box.scrollTop, newestTop(box));
    document.title = all.length ? `⚠ (${all.length}) oxbot` : "oxbot";
    document.body.classList.toggle("has-cards", now.length > 0);
  }, [keys, all.length]);

  const onScroll = () => {
    const box = boxRef.current;
    if (box) follow.current = box.scrollTop >= newestTop(box) - 8;
  };
  // A screenshot finished loading: keep showing the newest card if the user was.
  const onLoad = () => {
    const box = boxRef.current;
    if (box && follow.current) box.scrollTop = Math.max(box.scrollTop, newestTop(box));
  };

  const answer = (id: number, ans: Answer, schedule: ScheduleEdit | null = null) =>
    send({ type: "answer", id, answer: ans, schedule });

  function signInCard(s: CardSignIn) {
    return (
      <Card className="signin" key={`s:${s.id}`} title={`${botName(s.bot)} needs you to sign in to ${s.site}${s.before ? ` before ${s.before}` : ""}`}>
        <div className="where">
          {s.window_open
            ? `Sign in to ${s.site} below (or in the window on the home computer), then press I'm signed in. Bots never see your password.`
            : `${s.why ? `${s.why}. ` : ""}You type your password yourself, from this device. Every bot then stays signed in.`}
        </div>
        {s.window_open ? <SignInView id={s.id} frame={cards.signInFrames[s.id]} send={send} /> : null}
        <div className="btn-row btns">
          {s.window_open ? (
            <Btn label="I'm signed in" kind="primary" onClick={() => send({ type: "sign_in_done", id: s.id })} />
          ) : (
            <Btn label="Sign in" kind="primary" onClick={() => send({ type: "open_sign_in", id: s.id })} />
          )}
          <Btn label="Cancel" kind="secondary" onClick={() => send({ type: "cancel_sign_in", id: s.id })} />
        </div>
      </Card>
    );
  }

  // The page the bot is on. Click to enlarge; the area scrolls to it.
  function shot(a: CardApproval, key: string, who: string) {
    const big = bigShots.current.has(key);
    return (
      <img
        src={a.screenshot ?? undefined}
        alt={`The page ${who} is on right now`}
        className={big ? "big" : undefined}
        onLoad={onLoad}
        onClick={(ev) => {
          const img = ev.currentTarget;
          if (bigShots.current.has(key)) bigShots.current.delete(key);
          else bigShots.current.add(key);
          img.classList.toggle("big", bigShots.current.has(key));
          img.scrollIntoView({ block: "nearest" });
        }}
      />
    );
  }

  function setEdit(id: number, e: SchedEdit) {
    store.dispatch(local("cards/schedEdit", { id, edit: e }));
  }

  // A schedule card: Create, Change (edit it right here first) or Don't.
  function scheduleCard(a: CardApproval, who: string, key: string) {
    const d = a.schedule;
    if (!d) return null;
    const isNew = a.kind === "schedule";
    const head = isNew ? `${who} wants to set up a schedule` : `${who} wants to ${a.action}`;
    const where = isNew
      ? `It runs in ${d.bot_name} as ${a.asker || "you"}, with their bots, the websites they're signed in to and their AI spending, and each answer goes to their devices. Times are ${d.tz} time.${a.detail ? ` ${a.detail}.` : ""}`
      : `In ${d.bot_name}.`;
    let btns: ReactNode;
    let form: ReactNode = null;
    if (!isNew) {
      btns = (
        <div className="btn-row btns">
          <Btn label={a.action.startsWith("delete") ? "Delete it" : "Resume it"} kind="primary" onClick={() => answer(a.id, "allow_once")} />
          <Btn label="Don't" kind="secondary" cls="deny" onClick={() => answer(a.id, "deny")} />
        </div>
      );
    } else {
      const e = editOf(cards, a);
      if (!e.open) {
        btns = (
          <div className="btn-row btns">
            <Btn label="Create" kind="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Change" kind="secondary" onClick={() => setEdit(a.id, { ...e, open: true })} />
            <Btn label="Don't" kind="secondary" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
        );
      } else {
        form = <ScheduleForm e={e} onChange={(next) => setEdit(a.id, next)} />;
        btns = (
          <div className="btn-row btns">
            <Btn label="Create" kind="primary" onClick={() => answer(a.id, "allow_once", { task: e.task.trim(), rule: ruleOf(e) })} />
            <Btn label="Back" kind="secondary" onClick={() => setEdit(a.id, { ...e, open: false })} />
            <Btn label="Don't" kind="secondary" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
        );
      }
    }
    return (
      <Card className="sched" key={key} title={head}>
        <div className="sched-text">{d.text}</div>
        <div className="where">{where}</div>
        {d.from ? <div className="from">{`⚠ Not straight from a message of yours: ${d.from}. Check it's what you want.`}</div> : null}
        {form}
        {btns}
      </Card>
    );
  }

  function approvalCard(a: CardApproval, key: string) {
    const who = botName(a.bot);
    if ((a.kind === "schedule" || a.kind === "change_schedule") && a.schedule) return scheduleCard(a, who, key);
    if (a.kind === "create_bot") {
      // The Assistant wants to make a bot: its name and instructions. The
      // server says who it's for ("create a private bot for Sam").
      return (
        <Card className="newbot" key={key} title={`${who} wants to ${a.action || "create a bot"}: ${a.target}`}>
          <div className="where">It will follow these instructions. You can change or remove it later.</div>
          {a.detail ? <pre>{a.detail}</pre> : null}
          <div className="btn-row btns">
            <Btn label="Create" kind="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Don't create" kind="secondary" onClick={() => answer(a.id, "deny")} />
          </div>
        </Card>
      );
    }
    if (a.kind === "give_task") {
      // The Assistant has read something oxbot doesn't trust (a.page says
      // what), then wants to use a private bot: the whole task.
      return (
        <Card tone="danger" className="change" key={key} title={`⚠ ${who} wants to ${a.action} ${a.target}`}>
          <div className="where">{`${who} has read ${a.page || "web content or another bot's reply"} in this conversation, which may have asked for this. The task, word for word:`}</div>
          <pre>{a.detail || ""}</pre>
          <div className="btn-row btns">
            <Btn label="Give the task" kind="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Don't give it" kind="danger" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
        </Card>
      );
    }
    if (a.kind === "script") {
      // A small script in the bot's own folder: what it does in the bot's
      // words, then the program, word for word.
      const ask = `${a.action} ${a.target}?`;
      const parts = /^What it does: ([\s\S]*?)\n\n([\s\S]*)$/.exec(a.detail || "");
      return (
        <Card tone="warn" className="script" key={key} title={`${ask.charAt(0).toUpperCase()}${ask.slice(1)}`}>
          <p className="what">{parts ? parts[1] : "The bot didn't say what it does: read the program below, or say No."}</p>
          <div className="where">
            It can only see this bot's folder and can't use the internet. It runs on the home computer for at most 30 seconds. The program:
          </div>
          <pre>{parts ? parts[2] : a.detail || ""}</pre>
          <div className="btn-row btns">
            <Btn label="Yes" kind="primary" onClick={() => answer(a.id, "allow_once")} />
            {a.always ? <Btn label="Always for this bot" kind="secondary" onClick={() => answer(a.id, "always_allow")} /> : null}
            <Btn label="No" kind="secondary" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
          {a.always ? (
            <div className="hint">{`Always lets ${who} run small scripts in its own folder without asking, unless it has read a web page or a file in the conversation. Remove it in the bot's ··· settings.`}</div>
          ) : null}
        </Card>
      );
    }
    if (a.kind === "plan") {
      // A bot's plan as it starts browsing: the site, the place and what
      // it will do. It goes ahead unless the person stops it; replying in
      // the chat changes the plan.
      return (
        <Card className="plan" key={key} title={`${who} is starting on ${a.site}`}>
          {a.detail ? <div className="where">{`For: ${a.detail}`}</div> : null}
          <p className="plan-text">{a.target}</p>
          <div className="btn-row btns">
            <Btn label="OK" kind="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Stop" kind="secondary" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
          <div className="hint">It goes ahead unless you stop it. To change the plan, reply in the chat.</div>
        </Card>
      );
    }
    if (a.kind === "checkpoint") {
      return (
        <CheckIn
          key={key}
          who={who}
          steps={a.target}
          seconds={Number(a.detail) || 60}
          onGo={() => answer(a.id, "allow_once")}
          onStop={() => answer(a.id, "deny")}
        />
      );
    }
    if (a.kind === "home_area") {
      // The household's area: every bot uses it, for everyone in the home.
      return (
        <Card className="home-area" key={key} title={`${who} wants to save your home's area: ${a.target}`}>
          <div className="where">Every bot will use it for everyone in the home. You can change it in Settings → People.</div>
          <div className="btn-row btns">
            <Btn label="Save" kind="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Don't save" kind="secondary" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
        </Card>
      );
    }
    const change = a.kind === "change";
    return (
      <Card tone={change ? "danger" : "warn"} className={change ? "change" : undefined} key={key} title={`⚠ ${who} wants to ${a.action} ${a.target} on ${a.site}`}>
        <div className="where">{`${a.page ? `${a.page} · ` : ""}${change ? "This changes something on the site, where you may be signed in." : "This sends a file from the home computer to the site."}`}</div>
        {a.detail ? <pre>{a.detail}</pre> : null}
        {a.screenshot?.startsWith("data:image/") ? shot(a, key, who) : null}
        <div className="btn-row btns">
          <Btn label="Allow once" kind="primary" onClick={() => answer(a.id, "allow_once")} />
          {/* Offered by the server for this viewer only (never for pay, delete, Enter, uploads or the Assistant). */}
          {a.always ? <Btn label="Always allow" kind="secondary" onClick={() => answer(a.id, "always_allow")} /> : null}
          <Btn label="Don't allow" kind={change ? "danger" : "secondary"} cls="deny" onClick={() => answer(a.id, "deny")} />
        </div>
        {a.always ? (
          <div className="hint">{`Always lets ${who} ${a.action} ${a.target} on ${a.site} without asking. Remove it in the bot's ··· settings.`}</div>
        ) : null}
      </Card>
    );
  }

  return (
    <div className="cards" id="cards" aria-live="assertive" ref={boxRef} onScroll={onScroll}>
      {list.map((c) => (c.kind === "sign_in" ? signInCard(c.m) : approvalCard(c.m, c.key)))}
    </div>
  );
}

/** Shown on each bot waiting elsewhere; past this many, "+N more". */
const WAITING_SHOWN = 3;

/** One line above the box: the other bots waiting for an answer, each with
 *  Open to go to its chat (where its request is). */
export function Waiting() {
  const { dispatch } = useApp();
  const cards = useSel((s) => s.cards);
  const byId = useSel((s) => s.bots.byId);
  const current = useSel((s) => s.ui.current);
  const { others } = splitCards(cardList(cards), current, (id) => id in byId);
  if (!others.length) return null;
  const more = others.length - WAITING_SHOWN;
  return (
    <Banner tone="warn" className="waiting" id="waiting" role="status">
      {others.slice(0, WAITING_SHOWN).map((o) => {
        const name = byId[o.bot]?.name || o.bot;
        return (
          <span className="waiting-bot" key={o.bot} data-bot={o.bot}>
            <span className="nm">{`${name} ${o.signIn ? "needs you to sign in" : "needs your OK"}`}</span>
            <Button kind="quiet" size="sm" aria-label={`Open ${name}`} onClick={() => dispatch(local("ui/select", { bot: o.bot }))}>
              Open
            </Button>
          </span>
        );
      })}
      {more > 0 ? <span className="waiting-more">{`+${more} more`}</span> : null}
    </Banner>
  );
}

// The fields to change a proposed schedule: what to do, and when.
function ScheduleForm({ e, onChange }: { e: SchedEdit; onChange: (e: SchedEdit) => void }) {
  return (
    <div className="sched-edit">
      <Field label="What the bot does each time">
        <TextArea value={e.task} maxLength={1000} rows={2} onChange={(ev) => onChange({ ...e, task: ev.target.value })} />
      </Field>
      <div className="row">
        <Field label="How often">
          <Select value={e.kind} onChange={(ev) => onChange({ ...e, kind: ev.target.value as SchedEdit["kind"] })}>
            {SCHED_KINDS.map(([v, t]) => (
              <option key={v} value={v}>
                {t}
              </option>
            ))}
          </Select>
        </Field>
        {e.kind === "once" ? (
          <Field label="Day">
            <Input type="date" value={e.date} onChange={(ev) => onChange({ ...e, date: ev.target.value })} />
          </Field>
        ) : null}
        {e.kind === "monthly" ? (
          <Field label="Day of the month">
            <Input type="number" min={1} max={31} value={e.day} onChange={(ev) => onChange({ ...e, day: ev.target.value })} />
          </Field>
        ) : null}
        <Field label="Time">
          <Input type="time" value={e.time} onChange={(ev) => onChange({ ...e, time: ev.target.value })} />
        </Field>
      </div>
      {e.kind === "weekly" ? (
        <div className="days">
          {DAYS.map(([v, t]) => (
            <Checkbox
              key={v}
              checked={e.days.includes(v)}
              onChange={(ev) =>
                onChange({ ...e, days: ev.target.checked ? [...e.days, v] : e.days.filter((d) => d !== v) })
              }
            >
              {t}
            </Checkbox>
          ))}
        </div>
      ) : null}
    </div>
  );
}
