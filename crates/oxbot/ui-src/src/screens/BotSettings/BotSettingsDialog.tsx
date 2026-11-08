// Bot settings (its maker, or the home computer): name, what it's for, who
// it's shared with, the kind of job, its reports and schedules, delete.
// Anyone who may open the bot sees its reports and schedules here; only
// those who manage it change it.
import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Dialog } from "../../components/Dialog";
import type { BotInfo, ModelList, Person, Sharing } from "../../net/protocol";
import { local } from "../../state/store";
import { ScheduleList } from "./ScheduleList";
import { mayKeepPrivate, sharingOf } from "./sharing";

export function BotSettingsDialog() {
  const { dispatch } = useApp();
  const open = useSel((s) => s.ui.dialog === "botSettings");
  const current = useSel((s) => s.ui.current);
  const info = useSel((s) => (s.ui.current ? s.bots.byId[s.ui.current] : undefined));
  const onClose = useCallback(() => dispatch(local("ui/close", { dialog: "botSettings" })), [dispatch]);
  const show = open && !!info && !info.locked;
  return (
    <Dialog id="botDlg" open={show} onClose={onClose} labelledBy="bdTitle">
      {show && info && current ? <BotForm key={current} info={info} onClose={onClose} /> : null}
    </Dialog>
  );
}

const NO_LISTS: ModelList[] = [];
const sameList = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

function BotForm({ info: live, onClose }: { info: BotInfo; onClose: () => void }) {
  const { send, dispatch } = useApp();
  const people = useSel((s) => s.home.people);
  const settings = useSel((s) => s.connection.settings);
  const lists = useSel((s) => s.home.engines?.model_lists ?? NO_LISTS);
  const reports = useSel((s) => s.cards.reports);
  const schedules = useSel((s) => s.cards.schedules);
  const schedulesChanged = useSel((s) => s.cards.schedulesChanged);
  // What the bot was when the dialog opened: the form starts from it.
  const [b] = useState(live);
  const id = b.id;
  const manage = b.can_manage;
  const busy = b.busy;
  const central = b.central;
  const bdList = modelsOf(b);
  // Only the owner picks who sees their own bots; everyone else's are
  // shared with the whole home.
  const picks = mayKeepPrivate(b, settings) || sharingOf(b) !== "everyone";

  const [name, setName] = useState(b.name || b.id);
  const [purpose, setPurpose] = useState((b.instructions || "").trim());
  const [sharing, setSharing] = useState<Sharing>(sharingOf(b));
  const [picked, setPicked] = useState<string[]>(b.members ?? []);
  const [kind, setKind] = useState<string>(() => kindOf(lists, bdList));
  // The kinds of job came (or changed): pick the bot's again.
  const firstLists = useRef(lists);
  useEffect(() => {
    if (lists !== firstLists.current) setKind(kindOf(lists, modelsOf(b)));
  }, [lists, b]);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const [reportArmed, setReportArmed] = useState<string | null>(null);
  const timers = useRef<{ del?: ReturnType<typeof setTimeout>; rep?: ReturnType<typeof setTimeout> }>({});

  // Its reports and schedules: asked for when it opens, again when schedules change.
  useEffect(() => {
    dispatch(local("cards/settingsFor", { bot: id }));
    send({ type: "list_reports", bot: id });
    send({ type: "list_schedules", bot: id });
    const t = timers.current;
    return () => {
      clearTimeout(t.del);
      clearTimeout(t.rep);
      dispatch(local("cards/settingsFor", { bot: null }));
    };
  }, [id, send, dispatch]);
  const firstChange = useRef(schedulesChanged);
  useEffect(() => {
    if (schedulesChanged !== firstChange.current) send({ type: "list_schedules", bot: id });
  }, [schedulesChanged, id, send]);

  // Read from the select itself at Save, as the old page did, so a change
  // made to the element directly (assistive tools, tests) still counts.
  const kindRef = useRef<HTMLSelectElement>(null);
  const pickedKindList = (): string[] => {
    const v = kindRef.current?.value ?? kind;
    if (v === "own") return bdList;
    const k = lists.find((l) => l.kind === v);
    return k ? [...(k.models ?? [])] : [];
  };

  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    if (!manage) {
      onClose();
      return;
    }
    const n = name.trim();
    if (n && n !== live.name) send({ type: "rename_bot", bot: id, name: n });
    if (!central && !busy) {
      const p = purpose.trim();
      if (p !== (b.instructions || "").trim()) send({ type: "set_bot_instructions", bot: id, instructions: p });
    }
    if (!central) {
      if (picks) {
        const people = sharing === "people" ? picked : [];
        const was = b.members || [];
        const same =
          sharing === sharingOf(b) &&
          (sharing !== "people" || (people.length === was.length && people.every((x) => was.includes(x))));
        if (!same) send({ type: "set_sharing", bot: id, sharing, people });
      }
      // The bot keeps its engine; only its models change here.
      const list = pickedKindList();
      if (!sameList(list, bdList)) send({ type: "set_engine", bot: id, engine: b.engine, model: null, models: list });
    }
    onClose();
  };

  const onDelete = () => {
    clearTimeout(timers.current.del);
    if (!deleteArmed) {
      setDeleteArmed(true);
      timers.current.del = setTimeout(() => setDeleteArmed(false), 5000);
      return;
    }
    setDeleteArmed(false);
    send({ type: "delete_bot", bot: id });
    onClose();
  };

  const purposeHint = !manage
    ? ""
    : busy
      ? "Wait until it's done working to change this."
      : "The bot follows this. Changing it starts a fresh conversation for the AI; the chat stays.";

  return (
    <form id="botForm" onSubmit={submit}>
      <h2 id="bdTitle">{manage ? "Bot settings" : b.name || b.id}</h2>
      <div className="manage" id="bdManage" hidden={!manage}>
        <label>
          Name
          <input id="bdName" required autoComplete="off" disabled={!manage} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
      </div>
      {/* What it's for: its instructions. Others read them; the Assistant's are built in. */}
      <div id="bdPurposeBox" hidden={central}>
        <label>
          What it's for
          <textarea
            id="bdPurpose"
            rows={4}
            maxLength={8000}
            placeholder="What should it do?"
            readOnly={!manage}
            disabled={manage && busy}
            value={purpose}
            onChange={(e) => setPurpose(e.target.value)}
          />
        </label>
        <p className="hint" id="bdPurposeHint" hidden={!manage}>
          {purposeHint}
        </p>
      </div>
      <div className="manage" id="bdManage2" hidden={!manage}>
        <fieldset className="share-pick" id="bdShareBox" hidden={central || !picks}>
          <legend>Shared with</legend>
          <div className="for-pick" role="radiogroup" aria-label="Shared with">
            {(
              [
                ["everyone", "Everyone in the home"],
                ["me", "Just me"],
                ["people", "Picked people"],
              ] as [Sharing, string][]
            ).map(([v, t]) => (
              <label key={v}>
                <input type="radio" name="bdSharing" value={v} checked={sharing === v} disabled={busy} onChange={() => setSharing(v)} />{" "}
                {t}
              </label>
            ))}
          </div>
          <PeoplePick b={b} people={people} chosen={picked} setChosen={setPicked} hidden={sharing !== "people"} />
        </fieldset>
        <p className="share-line" id="bdSharedLine" hidden={central || picks}>
          Shared with everyone in the home
        </p>
        <p className="hint" id="bdHint" hidden={!busy}>
          Wait until it's done working to change this.
        </p>
        <details className="advanced" id="bdAdvanced" hidden={!manage || central}>
          <summary>Advanced</summary>
          <label>
            Kind of job
            <select id="bdKind" ref={kindRef} disabled={busy} value={kind} onChange={(e) => setKind(e.target.value)}>
              <option value="">Automatic</option>
              {lists.map((l) => (
                <option key={l.kind} value={l.kind}>{`${l.label} — ${l.about}`}</option>
              ))}
              {kindOf(lists, bdList) === "own" ? <option value="own">Its own list of models</option> : null}
            </select>
          </label>
          <p className="hint">Picked from the bot's instructions when it was made. It decides which AI models the bot tries, free ones first.</p>
        </details>
      </div>
      <section className="reports-box" aria-labelledby="bdReportsTitle" hidden={!reports?.length}>
        <h3 id="bdReportsTitle">Reports</h3>
        <ul className="rules" id="bdReports">
          {(reports ?? []).map((r) => (
              <li className="report-row" key={r.id}>
                <button
                  type="button"
                  className="open"
                  onClick={() => dispatch(local("ui/open", { dialog: "report", arg: `${id}:${r.id}` }))}
                >
                  <span>{r.title}</span>
                  <small>{`${new Date(r.created_ms).toLocaleDateString([], { month: "short", day: "numeric" })} · ${r.sources} source${r.sources === 1 ? "" : "s"}`}</small>
                </button>
                {manage ? (
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => {
                      clearTimeout(timers.current.rep);
                      if (reportArmed !== r.id) {
                        setReportArmed(r.id);
                        timers.current.rep = setTimeout(() => setReportArmed(null), 5000);
                        return;
                      }
                      setReportArmed(null);
                      send({ type: "delete_report", bot: id, id: r.id });
                    }}
                  >
                    {reportArmed === r.id ? "Yes, delete" : "Delete"}
                  </button>
                ) : null}
              </li>
            ))}
        </ul>
      </section>
      <section className="rules-box" aria-labelledby="bdSchedTitle">
        <h3 id="bdSchedTitle">Schedules</h3>
        <ScheduleList id="bdScheds" list={schedules} withBot={false} />
      </section>
      <div className="row">
        <button className="ghost" type="button" id="bdDelete" hidden={!manage} onClick={onDelete}>
          {deleteArmed ? `Yes, delete ${b.name}` : "Delete bot"}
        </button>
        <span style={{ flex: 1 }} />
        <button className="ghost" type="button" id="bdCancel" onClick={onClose}>
          {manage ? "Cancel" : "Close"}
        </button>
        <button className="primary" type="submit" id="bdSave" hidden={!manage}>
          Save
        </button>
      </div>
    </form>
  );
}

/** The bot's own models (the server leaves an empty list out). */
function modelsOf(b: BotInfo): string[] {
  return b.models?.length ? b.models : b.model ? [b.model] : [];
}

/** The kind of job whose models are the bot's list: "" automatic, "own" none of them. */
function kindOf(lists: ModelList[], models: string[]): string {
  if (!models.length) return "";
  return lists.find((l) => sameList(l.models ?? [], models))?.kind ?? "own";
}

// The people a bot may be shared with: everyone oxbot knows but its maker.
function PeoplePick({
  b,
  people,
  chosen,
  setChosen,
  hidden,
}: {
  b: BotInfo;
  people: Person[];
  chosen: string[];
  setChosen: (names: string[]) => void;
  hidden: boolean;
}) {
  const others = people.filter((p) => p.name !== b.created_by);
  return (
    <div className="people-pick" id="bdPeople" hidden={hidden}>
      {!others.length ? <span className="hint">Nobody else yet: connect a device for someone first.</span> : null}
      {others.map((p) => (
        <label key={p.name}>
          <input
            type="checkbox"
            value={p.name}
            checked={chosen.includes(p.name)}
            disabled={b.busy}
            onChange={(e) => setChosen(e.target.checked ? [...chosen, p.name] : chosen.filter((n) => n !== p.name))}
          />
          <span className="maker-dot" style={{ background: p.color || "var(--muted)" }} />
          {p.name}
        </label>
      ))}
    </div>
  );
}
