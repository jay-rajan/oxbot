// The Home's calendar: Day, Week, Month and List, everyone's events on
// one calendar. Tapping a day adds an event on it; tapping an event opens
// it. A change shows a line with Undo. Under 900px the week is a list of
// days (calendar.css).
import { useEffect, useMemo, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, IconButton, MenuIcon, Segmented, vars } from "../../components/ui";
import type { HomeEventInfo } from "../../net/protocol";
import {
  addDays,
  type CalView,
  occurrences,
  type Occurrence,
  rangeOf,
  step,
  weekday,
} from "../../state/calendar";
import { local } from "../../state/store";
import { DeletedDialog } from "./DeletedDialog";
import { type Draft, EventDialog } from "./EventDialog";
import { HomeIcon } from "../Sidebar/HomeIcon";
import { HomeMode } from "./HomeMode";
import { TimeGrid } from "./TimeGrid";
import "./calendar.css";

const VIEWS = [
  ["day", "Day"],
  ["week", "Week"],
  ["month", "Month"],
  ["list", "List"],
] as const;

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** Today, on this device's clock. */
export function today(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** "17:30" as this device writes times ("5:30 PM"). */
export function clock(hm: string): string {
  const [h, m] = hm.split(":").map(Number);
  return new Date(2000, 0, 1, h ?? 0, m ?? 0).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** "2026-11-16" written out ("Monday, November 16"). */
export function longDay(day: string, opts: Intl.DateTimeFormatOptions = { weekday: "long", month: "long", day: "numeric" }): string {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1).toLocaleDateString([], opts);
}

/** The range's title: "Nov 16 – 22", "November 2026", "Monday, November 16". */
function title(view: CalView, day: string, from: string, to: string): string {
  if (view === "day") return longDay(day);
  if (view === "month") return longDay(day, { month: "long", year: "numeric" });
  const a = longDay(from, { month: "short", day: "numeric" });
  const b = from.slice(0, 7) === to.slice(0, 7) ? String(Number(to.slice(8))) : longDay(to, { month: "short", day: "numeric" });
  return `${a} – ${b}`;
}

function personColor(people: { name: string; color: string }[], name: string): string | undefined {
  return people.find((p) => p.name === name)?.color;
}

export function CalendarView() {
  const { send, dispatch } = useApp();
  const view = useSel((s) => s.calendar.view);
  const stored = useSel((s) => s.calendar.day);
  const changed = useSel((s) => s.calendar.changed);
  const got = useSel((s) => s.calendar);
  const people = useSel((s) => s.home.people);
  const status = useSel((s) => s.connection.status);
  const now = today();
  const day = stored || now;
  const [from, to] = rangeOf(view, day);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [showDeleted, setShowDeleted] = useState(false);

  // Ask for the days shown; again when the Home changes or the connection
  // comes back.
  useEffect(() => {
    if (status === "open") send({ type: "home_events", from, to });
  }, [from, to, changed, status, send]);

  const shown = got.from === from && got.to === to ? got.events : [];
  const byDay = useMemo(() => occurrences(shown, from, to), [shown, from, to]);

  const go = (d: string) => dispatch(local("calendar/day", { day: d }));
  const setView = (v: CalView) => dispatch(local("calendar/view", { view: v }));
  const add = (d: string, time?: string) => setDraft(time ? { day: d, time } : { day: d });
  const open = (ev: HomeEventInfo) => setDraft({ day: ev.event.starts.slice(0, 10), ev });

  const chip = (o: Occurrence, compact = false) => {
    const color = personColor(people, o.ev.event.people[0] ?? "");
    return (
      <button
        key={`${o.ev.id}:${o.start}:${o.day}`}
        type="button"
        className={`cal-ev${o.time ? "" : " all-day"}${compact ? " compact" : ""}`}
        style={vars({ who: color })}
        title={o.time ? `${o.ev.event.title} · ${clock(o.time)}${o.endTime ? ` – ${clock(o.endTime)}` : ""}` : o.ev.event.title}
        onClick={(e) => {
          e.stopPropagation();
          open(o.ev);
        }}
      >
        {o.time ? (
          <span className="cal-ev-time">
            {clock(o.time)}
            {o.endTime && !compact ? ` – ${clock(o.endTime)}` : ""}
          </span>
        ) : null}
        <span className="cal-ev-title">{o.ev.event.title}</span>
        {!compact && (o.ev.event.people.length || o.ev.event.place) ? (
          <span className="cal-ev-who">{[o.ev.event.people.join(", "), o.ev.event.place].filter(Boolean).join(" · ")}</span>
        ) : null}
      </button>
    );
  };

  const dayColumn = (d: string) => {
    const list = byDay.get(d) ?? [];
    return (
      <section key={d} className={`cal-day${d === now ? " today" : ""}`} aria-label={longDay(d)}>
        <button type="button" className="cal-date" onClick={() => go(d)} title={`Open ${longDay(d)}`}>
          <span className="cal-wd">{WEEKDAYS[weekday(d)]}</span>
          <span className="cal-dn">{Number(d.slice(8))}</span>
        </button>
        <div className="cal-events">
          {list.map((o) => chip(o))}
          <button type="button" className="cal-add" onClick={() => add(d)} aria-label={`Add an event on ${longDay(d)}`}>
            +
          </button>
        </div>
      </section>
    );
  };

  let body: React.ReactNode;
  if (view === "week") {
    const days = Array.from({ length: 7 }, (_, i) => addDays(from, i));
    // Wide: the time grid. Narrow (calendar.css): a list of days.
    body = (
      <>
        <div className="cal-week-grid">
          <TimeGrid
            days={days}
            byDay={byDay}
            today={now}
            chip={chip}
            onAdd={add}
            onDay={(d) => {
              go(d);
              setView("day");
            }}
          />
        </div>
        <div className="cal-week">{days.map(dayColumn)}</div>
      </>
    );
  } else if (view === "day") {
    body = <TimeGrid days={[day]} byDay={byDay} today={now} chip={chip} onAdd={add} />;
  } else if (view === "month") {
    const n = Math.round((Date.parse(to) - Date.parse(from)) / 86_400_000) + 1;
    const month = day.slice(0, 7);
    body = (
      <div className="cal-month">
        {WEEKDAYS.map((w) => (
          <div key={w} className="cal-month-wd" aria-hidden="true">
            {w}
          </div>
        ))}
        {Array.from({ length: n }, (_, i) => {
          const d = addDays(from, i);
          const list = byDay.get(d) ?? [];
          return (
            <div
              key={d}
              className={`cal-cell${d.slice(0, 7) === month ? "" : " other"}${d === now ? " today" : ""}`}
            >
              <button
                type="button"
                className="cal-cell-dn"
                onClick={() => {
                  go(d);
                  setView("day");
                }}
                aria-label={longDay(d)}
              >
                {Number(d.slice(8))}
              </button>
              {list.slice(0, 3).map((o) => chip(o, true))}
              {list.length > 3 ? <span className="cal-more">+{list.length - 3} more</span> : null}
            </div>
          );
        })}
      </div>
    );
  } else {
    const ds = [...byDay.keys()].sort();
    body = (
      <div className="cal-list">
        {ds.length ? (
          ds.map((d) => (
            <section key={d} className={`cal-list-day${d === now ? " today" : ""}`}>
              <h2 className="cal-list-date">{longDay(d)}</h2>
              {(byDay.get(d) ?? []).map((o) => chip(o))}
            </section>
          ))
        ) : (
          <p className="cal-empty">Nothing in the next six weeks.</p>
        )}
      </div>
    );
  }

  return (
    <main className="cal" id="calendar" aria-label="Calendar">
      <header className="cal-head">
        <IconButton className="menu cal-menu" label="Bots" onClick={() => dispatch(local("ui/side", { open: true }))}>
          <MenuIcon />
        </IconButton>
        <HomeIcon id="home-calendar" />
        <h1>Calendar</h1>
        <HomeMode where="view" view="Calendar" />
        <span className="cal-break" aria-hidden="true" />
        <Segmented className="cal-views" name="calView" label="Calendar view" options={VIEWS} value={view} onChange={setView} />
        <div className="cal-range">
          <IconButton label="Earlier" size="sm" onClick={() => go(step(view, day, -1))}>
            ‹
          </IconButton>
          <span className="cal-title">{title(view, day, from, to)}</span>
          <IconButton label="Later" size="sm" onClick={() => go(step(view, day, 1))}>
            ›
          </IconButton>
          <Button kind="quiet" size="sm" onClick={() => go(now)} disabled={day === now}>
            Today
          </Button>
        </div>
        <div className="cal-actions">
          <Button
            kind="quiet"
            size="sm"
            onClick={() => {
              send({ type: "deleted_events" });
              setShowDeleted(true);
            }}
          >
            Recently deleted
          </Button>
          <Button kind="primary" id="newEvent" onClick={() => add(view === "day" ? day : day < from || day > to ? from : day)}>
            New event
          </Button>
        </div>
      </header>
      <SavedLine />
      <div className="cal-body">{body}</div>
      <EventDialog draft={draft} onClose={() => setDraft(null)} />
      <DeletedDialog open={showDeleted} onClose={() => setShowDeleted(false)} />
    </main>
  );
}

const DONE: Record<string, string> = {
  add: "Added",
  change: "Changed",
  delete: "Deleted",
  restore: "Brought back",
};

/** "Added “Soccer”. Undo", for this page's last change, for a while. */
function SavedLine() {
  const { send, dispatch } = useApp();
  const saved = useSel((s) => s.calendar.saved);
  const botName = useSel((s) => (saved?.bot ? s.bots.byId[saved.bot]?.name || saved.bot : ""));
  useEffect(() => {
    if (!saved) return;
    const t = setTimeout(() => dispatch(local("calendar/clearSaved")), 10_000);
    return () => clearTimeout(t);
  }, [saved, dispatch]);
  if (!saved) return null;
  const undone = saved.action === "undo";
  return (
    <div className="cal-saved" role="status">
      <span>
        {undone
          ? `Undone: “${saved.title}”.`
          : botName
            ? `${botName} ${(DONE[saved.action] ?? "Saved").toLowerCase()} “${saved.title}”.`
            : `${DONE[saved.action] ?? "Saved"} “${saved.title}”.`}
      </span>
      {undone ? null : (
        <Button kind="quiet" size="sm" onClick={() => send({ type: "undo_home_change", change: saved.change })}>
          Undo
        </Button>
      )}
    </div>
  );
}
