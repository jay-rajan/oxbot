// Day and Week on a time grid: hours down the side, each day a column,
// timed events at their hour (overlapping ones side by side), all-day ones
// in a row on top, and a line at the time now. Tapping an empty hour adds
// an event then. It opens scrolled to the morning (or to now, today).
import { type MouseEvent, type ReactNode, useEffect, useRef } from "react";
import { vars } from "../../components/ui";
import { type Occurrence, placeDay, weekday } from "../../state/calendar";
import { longDay } from "./CalendarView";

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HOURS = Array.from({ length: 24 }, (_, h) => h);

/** An hour as this device writes it, short: "5 PM" (or "17"). */
function hourLabel(h: number): string {
  return new Date(2000, 0, 1, h).toLocaleTimeString([], { hour: "numeric" });
}

function nowMinutes(): number {
  const d = new Date();
  return d.getHours() * 60 + d.getMinutes();
}

export function TimeGrid({
  days,
  byDay,
  today,
  chip,
  onAdd,
  onDay,
}: {
  days: readonly string[];
  byDay: ReadonlyMap<string, Occurrence[]>;
  today: string;
  /** An event's button; `placed` when it's on the grid. */
  chip: (o: Occurrence, compact?: boolean) => ReactNode;
  /** Add an event on `day` at `time` ("17:00"). */
  onAdd: (day: string, time: string) => void;
  /** Open a day (its header). */
  onDay?: (day: string) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const showsToday = days.includes(today);
  // Start at 7 AM, or an hour before now on today.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const hour = el.scrollHeight / 24;
    const from = showsToday ? Math.max(0, nowMinutes() / 60 - 1) : 7;
    el.scrollTop = Math.min(from, 17) * hour;
  }, [showsToday, days[0]]);

  const allDay = days.map((d) => (byDay.get(d) ?? []).filter((o) => !o.time));
  const anyAllDay = allDay.some((l) => l.length);

  const addAt = (day: string) => (e: MouseEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget) return;
    const box = e.currentTarget.getBoundingClientRect();
    const hour = Math.max(0, Math.min(23, Math.floor(((e.clientY - box.top) / box.height) * 24)));
    onAdd(day, `${String(hour).padStart(2, "0")}:00`);
  };

  return (
    <div className="cal-tg" style={vars({ cols: days.length })}>
      {/* One day: its date is the title already. */}
      <div className="cal-tg-head" hidden={days.length === 1}>
        <span className="cal-tg-gutter" />
        {days.map((d) => (
          <button
            key={d}
            type="button"
            className={`cal-tg-day${d === today ? " today" : ""}`}
            onClick={onDay ? () => onDay(d) : undefined}
            disabled={!onDay}
            aria-label={longDay(d)}
          >
            <span className="cal-wd">{WEEKDAYS[weekday(d)]}</span>
            <span className="cal-dn">{Number(d.slice(8))}</span>
          </button>
        ))}
      </div>
      {anyAllDay ? (
        <div className="cal-tg-allday">
          <span className="cal-tg-gutter cal-tg-label">All day</span>
          {allDay.map((list, i) => (
            <div key={days[i]} className="cal-tg-allday-cell">
              {list.map((o) => chip(o, true))}
            </div>
          ))}
        </div>
      ) : null}
      <div className="cal-tg-scroll" ref={scroller}>
        <div className="cal-tg-body">
          <div className="cal-tg-gutter cal-tg-hours" aria-hidden="true">
            {HOURS.map((h) => (
              <span key={h} className="cal-tg-hour">
                {h ? hourLabel(h) : ""}
              </span>
            ))}
          </div>
          {days.map((d) => (
            // biome-ignore lint/a11y/useKeyWithClickEvents: each day also has New event and its + buttons
            <div
              key={d}
              className={`cal-tg-col${d === today ? " today" : ""}`}
              onClick={addAt(d)}
              title={`Add an event on ${longDay(d)}`}
            >
              {placeDay(byDay.get(d) ?? []).map((p) => (
                <div
                  key={`${p.o.ev.id}:${p.o.start}`}
                  className={`cal-tg-ev${p.height < 45 ? " short" : p.height < 90 ? " mid" : ""}`}
                  style={vars({ top: p.top, len: p.height, lane: p.lane, lanes: p.lanes })}
                >
                  {chip(p.o)}
                </div>
              ))}
              {d === today ? <span className="cal-tg-now" style={vars({ top: nowMinutes() })} aria-hidden="true" /> : null}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
