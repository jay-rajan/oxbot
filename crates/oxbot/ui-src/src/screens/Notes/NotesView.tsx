// Records: every file the family kept, by year and topic, newest
// first, with a search box (title, topic, tags, people, summary, month).
// One short row each: what it is, its day and who it's about. Open shows
// it in the device's own app (a phone keeps copies of its records; the
// home computer opens its own file, or shows it in Finder); tapping the
// row shows and changes what it's filed by. New files come in through the
// Records chat beside it.
import { useEffect, useMemo, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Button, IconButton, Input, MenuIcon } from "../../components/ui";
import type { HomeNoteInfo } from "../../net/protocol";
import { byYearTopic, dayFrom, search, topics as topicsOf } from "../../state/notes";
import { local } from "../../state/store";
import { deviceBridge } from "../../web/address";
import { longDay } from "../Calendar/CalendarView";
import { HomeMode } from "../Calendar/HomeMode";
import { HomeIcon } from "../Sidebar/HomeIcon";
import { NoteDialog } from "./NoteDialog";
import "../Calendar/calendar.css";
import "./notes.css";

const KIND_ICON: Record<string, string> = { pdf: "📄", image: "🖼️", text: "📝", document: "📃", sheet: "📊", slides: "📽️", video: "🎞️" };

export function NotesView() {
  const { send, dispatch, toast } = useApp();
  const notes = useSel((s) => s.notes.notes);
  const loaded = useSel((s) => s.notes.loaded);
  const changed = useSel((s) => s.notes.changed);
  const status = useSel((s) => s.connection.status);
  const homeComputer = useSel((s) => !!s.connection.settings?.home_computer);
  const [query, setQuery] = useState("");
  const [topic, setTopic] = useState("");
  const [editing, setEditing] = useState<HomeNoteInfo | null>(null);
  const [deleted, setDeleted] = useState<{ note: HomeNoteInfo; back: boolean } | null>(null);

  useEffect(() => {
    if (status === "open") send({ type: "home_notes" });
  }, [changed, status, send]);

  // A phone keeps copies of its records: a new list may mean new ones.
  const device = useMemo(() => deviceBridge(window), []);
  useEffect(() => {
    if (loaded) device?.syncRecords?.().catch(() => {});
  }, [notes, loaded, device]);
  const open = (n: HomeNoteInfo) => {
    if (device?.openRecord) device.openRecord(n.id).catch((e) => toast(String(e), true));
    else send({ type: "open_note", id: n.id, reveal: false });
  };
  const canOpen = homeComputer || !!device?.openRecord;

  const topics = useMemo(() => topicsOf(notes), [notes]);
  const found = useMemo(() => search(notes, query, topic), [notes, query, topic]);
  const groups = useMemo(() => byYearTopic(found), [found]);

  return (
    <main className="cal notes" id="notes" aria-label="Records">
      <header className="cal-head">
        <IconButton className="menu cal-menu" label="Bots" onClick={() => dispatch(local("ui/side", { open: true }))}>
          <MenuIcon />
        </IconButton>
        <HomeIcon id="home-notes" />
        <h1>Records</h1>
        <HomeMode where="view" view="Records" />
      </header>
      {deleted ? (
        <div className="cal-saved" role="status">
          <span>{deleted.back ? `Brought back “${deleted.note.title}”.` : `Deleted “${deleted.note.title}”. It can be brought back for 30 days.`}</span>
          {deleted.back ? null : (
            <Button
              kind="quiet"
              size="sm"
              onClick={() => {
                send({ type: "restore_note", id: deleted.note.id });
                setDeleted({ ...deleted, back: true });
              }}
            >
              Undo
            </Button>
          )}
          <Button kind="quiet" size="sm" onClick={() => setDeleted(null)}>
            Dismiss
          </Button>
        </div>
      ) : null}
      <div className="notes-find">
        <Input
          id="notesSearch"
          type="search"
          value={query}
          placeholder="Find: blood test, insurance 2025, Akhila…"
          aria-label="Find a record"
          onChange={(x) => setQuery(x.target.value)}
        />
        {topics.length > 1 ? (
          <div className="notes-topics" role="group" aria-label="Topics">
            {["", ...topics].map((t) => (
              <button key={t || "all"} type="button" className="notes-topic" aria-pressed={topic === t ? "true" : "false"} onClick={() => setTopic(t)}>
                {t || "All"}
              </button>
            ))}
          </div>
        ) : null}
      </div>
      <div className="cal-body notes-body">
        {!loaded ? null : !notes.length ? (
          <p className="cal-empty">
            Nothing kept yet. Attach a file in the Records chat: a bill, a report, a school form. You choose whether the AI may read it.
          </p>
        ) : !found.length ? (
          <p className="cal-empty">Nothing matches. Try fewer words, or ask the Records bot.</p>
        ) : (
          groups.map(([year, byTopic]) => (
            <section key={year} className="notes-year" aria-label={year}>
              <h2>{year}</h2>
              {byTopic.map(([t, list]) => (
                <div key={t} className="notes-group">
                  <h3>
                    📁 {t} <span>{list.length}</span>
                  </h3>
                  <ul className="notes-list">
                    {list.map((n) => (
                      <li key={n.id} className="note-row-item">
                        <button type="button" className="note-main" onClick={() => setEditing(n)} title="See and change how it's filed">
                          <span className="note-icon" aria-hidden="true">
                            {KIND_ICON[n.kind] ?? "📄"}
                          </span>
                          <span className="note-text">
                            <span className="note-title">
                              {n.title}
                              {n.private ? <span className="note-lock" title="Private: the AI never read it"> 🔒</span> : null}
                            </span>
                            <span className="note-meta" title={dayFrom(n)}>
                              {longDay(n.day, { month: "short", day: "numeric", year: "numeric" })}
                              {n.people.length ? ` · ${n.people.join(", ")}` : ""}
                            </span>
                          </span>
                        </button>
                        {canOpen ? (
                          <span className="note-actions">
                            <Button kind="quiet" size="sm" onClick={() => open(n)}>
                              Open
                            </Button>
                            {homeComputer ? (
                              <Button kind="quiet" size="sm" onClick={() => send({ type: "open_note", id: n.id, reveal: true })}>
                                Finder
                              </Button>
                            ) : null}
                          </span>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                </div>
              ))}
            </section>
          ))
        )}
      </div>
      <NoteDialog note={editing} onClose={() => setEditing(null)} onDeleted={(n) => setDeleted({ note: n, back: false })} />
    </main>
  );
}
