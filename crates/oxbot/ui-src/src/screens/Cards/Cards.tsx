// Cards: sign-ins, changes (red), uploads (amber), new bots, tasks for
// private bots, scripts and schedules. Nothing happens until the user
// answers. Every open card shows, whichever bot is open, each titled with
// its bot; oldest first, so the newest sits right above the message box.
import { type ReactNode, useEffect, useLayoutEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import type { Answer, ScheduleEdit } from "../../net/protocol";
import {
  type CardApproval,
  type CardSignIn,
  cardList,
  DAYS,
  editOf,
  ruleOf,
  SCHED_KINDS,
  type SchedEdit,
} from "../../state/cards";
import { local } from "../../state/store";
import { expose, exposeFn } from "../../web/testBridge";

function Btn({ label, cls, onClick }: { label: string; cls: string; onClick: () => void }) {
  return (
    <button type="button" className={cls} onClick={onClick}>
      {label}
    </button>
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
  const list = cardList(cards);
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
    const open = now.length;
    document.title = open ? `⚠ (${open}) oxbot` : "oxbot";
    document.body.classList.toggle("has-cards", open > 0);
  }, [keys]);

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
      <div className="card signin" key={`s:${s.id}`}>
        <h3>{`${botName(s.bot)} needs you to sign in to ${s.site}`}</h3>
        <div className="where">
          {s.window_open
            ? "Sign in in the Chrome window that opened. Then press I'm signed in, or just close the window."
            : `${s.why ? `${s.why}. ` : ""}You type your password yourself, in a normal Chrome window. Every bot then stays signed in there.`}
        </div>
        <div className="btns">
          {s.window_open ? (
            <Btn label="I'm signed in" cls="primary" onClick={() => send({ type: "sign_in_done", id: s.id })} />
          ) : (
            <Btn label="Open sign-in window" cls="primary" onClick={() => send({ type: "open_sign_in", id: s.id })} />
          )}
          <Btn label="Cancel" cls="ghost" onClick={() => send({ type: "cancel_sign_in", id: s.id })} />
        </div>
      </div>
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
        <div className="btns">
          <Btn label={a.action.startsWith("delete") ? "Delete it" : "Resume it"} cls="primary" onClick={() => answer(a.id, "allow_once")} />
          <Btn label="Don't" cls="deny" onClick={() => answer(a.id, "deny")} />
        </div>
      );
    } else {
      const e = editOf(cards, a);
      if (!e.open) {
        btns = (
          <div className="btns">
            <Btn label="Create" cls="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Change" cls="ghost" onClick={() => setEdit(a.id, { ...e, open: true })} />
            <Btn label="Don't" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
        );
      } else {
        form = <ScheduleForm e={e} onChange={(next) => setEdit(a.id, next)} />;
        btns = (
          <div className="btns">
            <Btn label="Create" cls="primary" onClick={() => answer(a.id, "allow_once", { task: e.task.trim(), rule: ruleOf(e) })} />
            <Btn label="Back" cls="ghost" onClick={() => setEdit(a.id, { ...e, open: false })} />
            <Btn label="Don't" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
        );
      }
    }
    return (
      <div className="card sched" key={key}>
        <h3>{head}</h3>
        <div className="sched-text">{d.text}</div>
        <div className="where">{where}</div>
        {d.from ? <div className="from">{`⚠ Not straight from a message of yours: ${d.from}. Check it's what you want.`}</div> : null}
        {form}
        {btns}
      </div>
    );
  }

  function approvalCard(a: CardApproval, key: string) {
    const who = botName(a.bot);
    if ((a.kind === "schedule" || a.kind === "change_schedule") && a.schedule) return scheduleCard(a, who, key);
    if (a.kind === "create_bot") {
      // The Assistant wants to make a bot: its name and instructions. The
      // server says who it's for ("create a private bot for Sam").
      return (
        <div className="card newbot" key={key}>
          <h3>{`${who} wants to ${a.action || "create a bot"}: ${a.target}`}</h3>
          <div className="where">It will follow these instructions. You can change or remove it later.</div>
          {a.detail ? <pre>{a.detail}</pre> : null}
          <div className="btns">
            <Btn label="Create" cls="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Don't create" cls="ghost" onClick={() => answer(a.id, "deny")} />
          </div>
        </div>
      );
    }
    if (a.kind === "give_task") {
      // The Assistant has read something oxbot doesn't trust (a.page says
      // what), then wants to use a private bot: the whole task.
      return (
        <div className="card change" key={key}>
          <h3>{`⚠ ${who} wants to ${a.action} ${a.target}`}</h3>
          <div className="where">{`${who} has read ${a.page || "web content or another bot's reply"} in this conversation, which may have asked for this. The task, word for word:`}</div>
          <pre>{a.detail || ""}</pre>
          <div className="btns">
            <Btn label="Give the task" cls="primary" onClick={() => answer(a.id, "allow_once")} />
            <Btn label="Don't give it" cls="deny red" onClick={() => answer(a.id, "deny")} />
          </div>
        </div>
      );
    }
    if (a.kind === "script") {
      // A small script in the bot's own folder: the program, word for word.
      const ask = `${a.action} ${a.target}?`;
      return (
        <div className="card script" key={key}>
          <h3>{`${ask.charAt(0).toUpperCase()}${ask.slice(1)}`}</h3>
          <div className="where">
            It can only see this bot's folder and can't use the internet. It runs on the home computer for at most 30 seconds. The program:
          </div>
          <pre>{a.detail || ""}</pre>
          <div className="btns">
            <Btn label="Yes" cls="primary" onClick={() => answer(a.id, "allow_once")} />
            {a.always ? <Btn label="Always for this bot" cls="ghost" onClick={() => answer(a.id, "always_allow")} /> : null}
            <Btn label="No" cls="deny" onClick={() => answer(a.id, "deny")} />
          </div>
          {a.always ? (
            <div className="hint">{`Always lets ${who} run small scripts in its own folder without asking, unless it has read a web page or a file in the conversation. Remove it in the bot's ··· settings.`}</div>
          ) : null}
        </div>
      );
    }
    const change = a.kind === "change";
    return (
      <div className={change ? "card change" : "card"} key={key}>
        <h3>{`⚠ ${who} wants to ${a.action} ${a.target} on ${a.site}`}</h3>
        <div className="where">{`${a.page ? `${a.page} · ` : ""}${change ? "This changes something on the site, where you may be signed in." : "This sends a file from the home computer to the site."}`}</div>
        {a.detail ? <pre>{a.detail}</pre> : null}
        {a.screenshot?.startsWith("data:image/") ? shot(a, key, who) : null}
        <div className="btns">
          <Btn label="Allow once" cls="primary" onClick={() => answer(a.id, "allow_once")} />
          {/* Offered by the server for this viewer only (never for pay, delete, Enter, uploads or the Assistant). */}
          {a.always ? <Btn label="Always allow" cls="ghost" onClick={() => answer(a.id, "always_allow")} /> : null}
          <Btn label="Don't allow" cls={change ? "deny red" : "deny"} onClick={() => answer(a.id, "deny")} />
        </div>
        {a.always ? (
          <div className="hint">{`Always lets ${who} ${a.action} ${a.target} on ${a.site} without asking. Remove it in the bot's ··· settings.`}</div>
        ) : null}
      </div>
    );
  }

  return (
    <div className="cards" id="cards" aria-live="assertive" ref={boxRef} onScroll={onScroll}>
      {list.map((c) => (c.kind === "sign_in" ? signInCard(c.m) : approvalCard(c.m, c.key)))}
    </div>
  );
}

// The fields to change a proposed schedule: what to do, and when.
function ScheduleForm({ e, onChange }: { e: SchedEdit; onChange: (e: SchedEdit) => void }) {
  return (
    <div className="sched-edit">
      <label>
        What the bot does each time
        <textarea value={e.task} maxLength={1000} onChange={(ev) => onChange({ ...e, task: ev.target.value })} />
      </label>
      <div className="row">
        <label>
          How often
          <select value={e.kind} onChange={(ev) => onChange({ ...e, kind: ev.target.value as SchedEdit["kind"] })}>
            {SCHED_KINDS.map(([v, t]) => (
              <option key={v} value={v}>
                {t}
              </option>
            ))}
          </select>
        </label>
        {e.kind === "once" ? (
          <label>
            Day
            <input type="date" value={e.date} onChange={(ev) => onChange({ ...e, date: ev.target.value })} />
          </label>
        ) : null}
        {e.kind === "monthly" ? (
          <label>
            Day of the month
            <input type="number" min={1} max={31} value={e.day} onChange={(ev) => onChange({ ...e, day: ev.target.value })} />
          </label>
        ) : null}
        <label>
          Time
          <input type="time" value={e.time} onChange={(ev) => onChange({ ...e, time: ev.target.value })} />
        </label>
      </div>
      {e.kind === "weekly" ? (
        <div className="days">
          {DAYS.map(([v, t]) => (
            <label key={v}>
              <input
                type="checkbox"
                checked={e.days.includes(v)}
                onChange={(ev) =>
                  onChange({ ...e, days: ev.target.checked ? [...e.days, v] : e.days.filter((d) => d !== v) })
                }
              />
              {t}
            </label>
          ))}
        </div>
      ) : null}
    </div>
  );
}
