// Cards: sign-ins, changes (red), uploads (amber), new bots, tasks for
// private bots, scripts and schedules. Nothing happens until the user
// answers. Every open card shows, whichever bot is open, each titled with
// its bot; oldest first, so the newest sits right above the message box.
import { type ReactNode, useEffect, useLayoutEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, type ButtonKind, Card, Checkbox, Field, Input, Select, TextArea } from "../../components/ui";
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
import "./cards.css";

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
      <Card className="signin" key={`s:${s.id}`} title={`${botName(s.bot)} needs you to sign in to ${s.site}`}>
        <div className="where">
          {s.window_open
            ? `Sign in to ${s.site} in the window that opened, then close it.`
            : `${s.why ? `${s.why}. ` : ""}You type your password yourself, in a normal Chrome window. Every bot then stays signed in there.`}
        </div>
        <div className="btn-row btns">
          {s.window_open ? (
            <Btn label="I'm signed in" kind="primary" onClick={() => send({ type: "sign_in_done", id: s.id })} />
          ) : (
            <Btn label="Open sign-in window" kind="primary" onClick={() => send({ type: "open_sign_in", id: s.id })} />
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
      // A small script in the bot's own folder: the program, word for word.
      const ask = `${a.action} ${a.target}?`;
      return (
        <Card tone="warn" className="script" key={key} title={`${ask.charAt(0).toUpperCase()}${ask.slice(1)}`}>
          <div className="where">
            It can only see this bot's folder and can't use the internet. It runs on the home computer for at most 30 seconds. The program:
          </div>
          <pre>{a.detail || ""}</pre>
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
