// Compare (#21, a developer tool for the owner): a bot's test cases replayed
// on other models, the runs, and a scoreboard. Opened with
// `ui/open {dialog: "compare", arg: "<bot>"}` or `"<bot>:cmpCases"`.
import { type FormEvent, type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Dialog } from "../../components/Dialog";
import type { BotEvent, IntentMatch, RunInfo, Target } from "../../net/protocol";
import type { CmpTab, RunDetail } from "../../state/cards";
import { local } from "../../state/store";
import { md, type MdNode } from "../Chat/md";
import { comparable, isOwner } from "./devTools";
import { cmpMoney, cmpNum, cmpPct, cmpSecs, heldBack, targetName, VERDICT } from "./format";

const TABS: [CmpTab, string][] = [
  ["cmpScore", "Scoreboard"],
  ["cmpCases", "Test cases"],
  ["cmpRuns", "Runs"],
];

export function CompareDialog() {
  const { send, dispatch } = useApp();
  const open = useSel((s) => s.ui.dialog === "compare");
  const bot = useSel((s) => s.cards.cmp.bot);
  const owner = useSel((s) => isOwner(s.connection.settings));
  const current = useSel((s) => s.ui.current);
  const canCompare = useSel((s) => comparable(s.ui.current ? s.bots.byId[s.ui.current] : undefined));
  const onClose = useCallback(() => dispatch(local("ui/close", { dialog: "compare" })), [dispatch]);

  // The owner's settings, once; and the open bot's thumbs and test cases.
  const prefsAsked = useRef(false);
  useEffect(() => {
    if (owner && !prefsAsked.current) {
      prefsAsked.current = true;
      send({ type: "list_compare_prefs" });
    }
  }, [owner, send]);
  useEffect(() => {
    if (owner && current && canCompare) send({ type: "list_ratings", bot: current });
  }, [owner, current, canCompare, send]);

  return (
    <Dialog id="compareDlg" className="wide" open={open && !!bot} onClose={onClose} labelledBy="cmpTitle">
      {bot ? <Compare key={bot} bot={bot} onClose={onClose} /> : null}
    </Dialog>
  );
}

function Compare({ bot, onClose }: { bot: string; onClose: () => void }) {
  const { send, dispatch } = useApp();
  const cmp = useSel((s) => s.cards.cmp);
  const name = useSel((s) => s.bots.byId[bot]?.name);
  const engines = useSel((s) => s.home.engines);
  const settings = useSel((s) => s.connection.settings);
  const reservePct = useSel((s) => s.cards.reservePct);
  const [cases, setCases] = useState<string[]>([]);
  const [targets, setTargets] = useState<string[]>([]);
  const [repeats, setRepeats] = useState("1");

  useEffect(() => {
    if (!engines) send({ type: "list_engines" });
    send({ type: "scoreboard", bot });
    send({ type: "list_test_cases", bot });
    send({ type: "list_runs", bot, case: null });
    // Only on opening.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bot, send]);

  // A batch that ended changes the numbers.
  const status = cmp.batch?.status;
  useEffect(() => {
    if (status === "done" || status === "stopped") {
      send({ type: "scoreboard", bot });
      send({ type: "list_test_cases", bot });
    }
  }, [status, bot, send]);

  // Each free model bots can use now.
  const ready = !!(settings?.openrouter_ready && settings.codex_ready);
  const choices: Target[] = (ready ? engines?.models ?? [] : []).map((m) => ({ engine: "codex", model: m.id }));

  const tab = (t: CmpTab) => {
    dispatch(local("cards/cmpTab", { tab: t }));
    // Fresh numbers each time (marks change them).
    if (t === "cmpScore") send({ type: "scoreboard", bot });
    if (t === "cmpCases") send({ type: "list_test_cases", bot });
  };
  const changed = () => dispatch(local("cards/cmpEstimate", { text: "" }));
  const picked = () => ({
    bot,
    cases: cases.filter((id) => cmp.cases.some((c) => c.id === id && !c.archived)),
    targets: choices.filter((t) => targets.includes(t.model)),
    repeats: Number(repeats),
  });

  const p = cmp.batch;
  const running = !!p && (p.status === "running" || p.status === "paused" || p.status === "queued");

  return (
    <div className="settings">
      <header className="settings-head">
        <h2 id="cmpTitle">{`Compare · ${name || bot}`}</h2>
        <button className="ghost" id="cmpClose" type="button" onClick={onClose}>
          Close
        </button>
      </header>
      <div className="tabs" role="tablist" aria-label="Compare">
        {TABS.map(([id, label]) => (
          <button key={id} type="button" role="tab" data-cmp={id} aria-selected={cmp.tab === id} onClick={() => tab(id)}>
            {label}
          </button>
        ))}
      </div>
      <div id="cmpScore" role="tabpanel" hidden={cmp.tab !== "cmpScore"}>
        <section>
          <div className="tbl">
            <table className="usage">
              <thead>
                <tr>
                  <th>Model</th>
                  <th>Runs</th>
                  <th title="Your marks">Pass</th>
                  <th>Steps</th>
                  <th>Questions</th>
                  <th>Tokens in</th>
                  <th>Tokens out</th>
                  <th>Cost</th>
                  <th>Time</th>
                  <th title="Steps a live run would have asked you about">Approval requests</th>
                  <th title="Expected steps it did">Match</th>
                  <th title="Steps you had said no to">Said no</th>
                  <th>Refused</th>
                  <th>Errors</th>
                  <th title="Writes the test browser blocked">Blocked</th>
                  <th title="Your marks and an earlier judge's agreeing">Agree</th>
                  <th>👍 👎</th>
                </tr>
              </thead>
              <tbody id="cmpScoreRows">
                {cmp.rows.map((r) => {
                  const left = [
                    r.needs_sign_in && `${r.needs_sign_in} needed sign-in`,
                    r.rate_limited && `${r.rate_limited} rate limited`,
                    r.stopped && `${r.stopped} stopped`,
                  ]
                    .filter(Boolean)
                    .join(", ");
                  const cells = [
                    cmpNum(r.n),
                    cmpPct(r.pass_rate),
                    cmpNum(r.steps, 1),
                    cmpNum(r.requests, 1),
                    cmpNum(r.input_tokens),
                    cmpNum(r.output_tokens),
                    cmpMoney(r.cost_usd),
                    cmpSecs(r.ms),
                    cmpNum(r.intents, 1),
                    cmpPct(r.intent_match),
                    cmpNum(r.should_not),
                    cmpNum(r.refusals),
                    cmpNum(r.errors),
                    cmpNum(r.net_blocked),
                    cmpPct(r.agreement),
                    `${r.thumbs_up} / ${r.thumbs_down}`,
                  ];
                  return (
                    <tr key={`${r.engine}/${r.model}`}>
                      <td title={left ? `Left out: ${left}` : undefined}>
                        {targetName(r)}
                        {r.good_enough ? (
                          <>
                            {" "}
                            <span className="pill good">good enough</span>
                          </>
                        ) : null}
                      </td>
                      {cells.map((v, i) => (
                        // biome-ignore lint/suspicious/noArrayIndexKey: fixed columns
                        <td key={i}>{v}</td>
                      ))}
                    </tr>
                  );
                })}
                {!cmp.rows.length ? (
                  <tr>
                    <td colSpan={17}>No runs yet.</td>
                  </tr>
                ) : null}
              </tbody>
            </table>
          </div>
          <p className="hint">
            Pass rate uses your marks. Medians per run. "Good enough": at least 5 runs, within 10 points of the best, and never a step you had said no to.
          </p>
        </section>
      </div>
      <div id="cmpCases" role="tabpanel" hidden={cmp.tab !== "cmpCases"}>
        <section>
          <ul className="cmp-list" id="cmpCaseList">
            {cmp.cases
              .filter((c) => !c.archived)
              .map((c) => (
                <li key={c.id}>
                  <input
                    type="checkbox"
                    value={c.id}
                    aria-label={c.title}
                    checked={cases.includes(c.id)}
                    onChange={(e) => {
                      setCases(e.target.checked ? [...cases, c.id] : cases.filter((x) => x !== c.id));
                      changed();
                    }}
                  />
                  <div>
                    <span>{c.title}</span> <span className={`pill ${c.kind}`}>{c.kind}</span>
                    <small>
                      {[c.sites.join(", "), c.uses_sign_ins ? "signed in" : "", c.network === "xhr" ? "reads by POST allowed" : ""]
                        .filter(Boolean)
                        .join(" · ")}
                    </small>
                    <small>
                      {c.last.map((r) => (
                        <span key={`${r.engine}/${r.model}`} className={`vdot ${r.verdict || ""}`} title={`${targetName(r)}: ${r.verdict || r.status}`} />
                      ))}
                    </small>
                  </div>
                  <DeleteCase id={c.id} />
                </li>
              ))}
          </ul>
          <p className="hint" id="cmpCasesEmpty" hidden={cmp.cases.length > 0}>
            No test cases yet. Under one of this bot's replies, choose "Test case…".
          </p>
        </section>
        <section id="cmpRunBox">
          <h3>Run selected on…</h3>
          <div className="targets" id="cmpTargets">
            {choices.map((t) => (
              <label className="check" key={t.model}>
                <input
                  type="checkbox"
                  value={JSON.stringify(t)}
                  checked={targets.includes(t.model)}
                  onChange={(e) => {
                    setTargets(e.target.checked ? [...targets, t.model] : targets.filter((x) => x !== t.model));
                    changed();
                  }}
                />
                {targetName(t)}
              </label>
            ))}
            {!choices.length ? (
              <p className="hint">Models show here once the home's AI account is connected (Settings → AI models).</p>
            ) : null}
          </div>
          <label>
            Times each
            <select
              id="cmpRepeats"
              value={repeats}
              onChange={(e) => {
                setRepeats(e.target.value);
                changed();
              }}
            >
              <option>1</option>
              <option>2</option>
              <option>3</option>
            </select>
          </label>
          <p className="hint" id="cmpEstimate">
            {cmp.estimate}
          </p>
          <div className="btns">
            <button
              className="ghost"
              type="button"
              id="cmpEstimateBtn"
              onClick={() => {
                const q = picked();
                if (!q.cases.length || !q.targets.length) {
                  dispatch(local("cards/cmpEstimate", { text: "Pick at least one test case and one model." }));
                  return;
                }
                send({ type: "estimate_batch", ...q });
              }}
            >
              Estimate
            </button>
            <button
              className="primary"
              type="button"
              id="cmpStart"
              disabled={!cmp.canStart}
              onClick={() => {
                send({ type: "start_batch", ...picked() });
                dispatch(local("cards/cmpStarted"));
              }}
            >
              Start
            </button>
            <button className="ghost" type="button" id="cmpStop" hidden={!running} onClick={() => p && send({ type: "stop_batch", id: p.batch })}>
              Stop
            </button>
          </div>
          <div className="cmp-progress" id="cmpProgress" hidden={!running}>
            <i style={{ width: `${p?.total ? Math.round((p.done / p.total) * 100) : 0}%` }} />
          </div>
          <p className="hint" id="cmpProgressText">
            {!p ? "" : p.status === "paused" ? `Paused: ${p.paused_reason || "waiting"}` : `${p.done} of ${p.total}${running ? "" : ` · ${p.status}`}`}
          </p>
          <p className="hint">
            Test runs use their own browser with a copy of the websites you're signed in to (no history) and block every request that would write. A link that acts when opened (one-click unsubscribe, magic links) can't be told from a read, as in live runs.
          </p>
        </section>
      </div>
      <div id="cmpRuns" role="tabpanel" hidden={cmp.tab !== "cmpRuns"}>
        <section id="cmpRunListBox" hidden={!!cmp.detail}>
          <ul className="cmp-list" id="cmpRunList">
            {cmp.runs.map((r) => {
              const openRun = () => send({ type: "open_run", run: r.id });
              return (
                <li
                  key={r.id}
                  className="run"
                  tabIndex={0}
                  onClick={openRun}
                  onKeyDown={(ev) => {
                    if (ev.key === "Enter") openRun();
                  }}
                >
                  <div>
                    <span>{`${r.case_title || "Run"} · ${targetName(r)}`}</span>
                    <small>
                      {[
                        r.kind === "original" ? "the original" : r.kind,
                        r.status,
                        `${r.steps} steps`,
                        r.requests ? `${r.requests} questions` : "",
                        heldBack(r.intents),
                        r.net_blocked ? `${r.net_blocked} blocked` : "",
                        r.started_ms ? new Date(r.started_ms).toLocaleString() : "",
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </small>
                  </div>
                  <span className="pill">{r.verdict ? VERDICT[r.verdict] : r.status === "running" ? "running…" : "–"}</span>
                </li>
              );
            })}
          </ul>
          <p className="hint" id="cmpRunsEmpty" hidden={cmp.runs.length > 0}>
            No runs yet.
          </p>
        </section>
        <Prefs reservePct={reservePct} />
        <section id="cmpRunDetail" hidden={!cmp.detail}>
          <div className="btns">
            <button className="ghost" type="button" id="cmpRunBack" onClick={() => dispatch(local("cards/cmpBack"))}>
              ← Runs
            </button>
          </div>
          {cmp.detail ? <Detail d={cmp.detail} /> : <div className="side-by-side" />}
        </section>
      </div>
    </div>
  );
}

function DeleteCase({ id }: { id: string }) {
  const { send } = useApp();
  const [armed, setArmed] = useState(false);
  return (
    <button
      type="button"
      className="ghost"
      onClick={() => {
        if (armed) send({ type: "delete_test_case", id });
        else setArmed(true);
      }}
    >
      {armed ? "Yes, delete" : "Delete"}
    </button>
  );
}

function Prefs({ reservePct }: { reservePct: number | null }) {
  const { send, toast } = useApp();
  const [reserve, setReserve] = useState(reservePct == null ? "" : String(reservePct));
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (reservePct != null) setReserve(String(reservePct));
  }, [reservePct]);
  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    send({ type: "set_compare_prefs", reserve_pct: Number(reserve) });
    toast("Saved.");
  };
  return (
    <section id="cmpPrefsSection" hidden={reservePct == null}>
      <h3>Comparison</h3>
      <form className="engine-prefs" id="cmpPrefsForm" onSubmit={submit}>
        <label>
          Free questions kept for live bots, in percent
          <input type="number" id="cmpReserve" min={0} max={90} step={1} required value={reserve} onChange={(e) => setReserve(e.target.value)} />
        </label>
        <div className="btns">
          <button className="primary" type="submit">
            Save
          </button>
          <button
            className="ghost"
            type="button"
            id="cmpDeleteRuns"
            onClick={() => {
              if (!armed) {
                setArmed(true);
                return;
              }
              setArmed(false);
              send({ type: "delete_all_runs" });
            }}
          >
            {armed ? "Yes, delete all test runs" : "Delete all test runs"}
          </button>
        </div>
        <p className="hint">
          Test runs get a copy of the websites they need you signed in to, in their own browser without history, and delete it when they end. A link that acts when opened (one-click unsubscribe, magic links) can't be told from a read, as in live runs.
        </p>
      </form>
    </section>
  );
}

/** A run beside the original it replays. */
function Detail({ d }: { d: RunDetail }) {
  const { send } = useApp();
  const run = d.run;
  const judge = run.judge;
  const judgeText = judge?.checks
    ? `Judge: ${VERDICT[judge.verdict]} (${judge.score}/100). ${judge.checks.map((c) => `${c.met === "yes" ? "✓" : c.met === "no" ? "✗" : "?"} ${c.check}`).join(" · ")} ${judge.notes || ""}`
    : "";
  const verdicts = (
    <div className="verdicts">
      {(
        [
          ["pass", "✓"],
          ["partial", "½"],
          ["fail", "✗"],
        ] as const
      ).map(([v, label]) => (
        <button
          key={v}
          type="button"
          className="ghost"
          title={VERDICT[v]}
          aria-pressed={run.owner_verdict === v}
          onClick={() => send({ type: "set_run_verdict", run: run.id, verdict: run.owner_verdict === v ? null : v })}
        >
          {label}
        </button>
      ))}
      {/* Runs judged before Claude left oxbot keep what the judge said. */}
      {judgeText ? <p className="hint">{judgeText}</p> : null}
    </div>
  );
  return (
    <div className="side-by-side">
      <RunSide id="cmpOrig" title="Original" run={d.original} events={d.original_events} isOriginal match={null} />
      <RunSide
        id="cmpThis"
        title={`This run · ${targetName(run)}`}
        run={run}
        events={d.events}
        isOriginal={run.kind === "original"}
        match={run.intent_match}
        verdicts={verdicts}
      />
    </div>
  );
}

/** One side of a run's page: its answer, steps, numbers and transcript. */
function RunSide({
  id,
  title,
  run,
  events,
  isOriginal,
  match,
  verdicts,
}: {
  id: string;
  title: string;
  run: RunInfo | null;
  events: BotEvent[];
  isOriginal: boolean;
  match: IntentMatch | null;
  verdicts?: ReactNode;
}) {
  if (!run)
    return (
      <div id={id}>
        <h4>{title}</h4>
        <p className="hint">Not kept.</p>
      </div>
    );
  const intents: string[] = [];
  if (!isOriginal && match) {
    for (const [list, mark] of [
      [match.matched, "✓ matched"],
      [match.missing, "✗ missing"],
      [match.extra, "＋ extra"],
      [match.should_not, "⚠ said no before"],
    ] as const)
      for (const line of list) intents.push(`${mark}: ${line}`);
  } else for (const i of run.intent_list) intents.push(`would ${i.action} ${i.target} on ${i.site}`);
  const nums = [
    run.status,
    `${run.steps} steps`,
    run.requests ? `${run.requests} questions` : "",
    run.input_tokens ? `${cmpNum(run.input_tokens)} in / ${cmpNum(run.output_tokens)} out` : "",
    run.cost_usd ? cmpMoney(run.cost_usd) : "",
    run.started_ms && run.ended_ms ? cmpSecs(run.ended_ms - run.started_ms) : "",
    isOriginal ? (run.intents ? `${run.intents} approval request${run.intents === 1 ? "" : "s"}` : "") : heldBack(run.intents),
    run.refusals ? `${run.refusals} refused` : "",
    run.errors ? `${run.errors} errors` : "",
    run.net_blocked ? `${run.net_blocked} blocked` : "",
    run.web_age_days != null ? `web ${run.web_age_days} days newer` : "",
  ]
    .filter(Boolean)
    .join(" · ");
  const lines = events.map((e) => <EventLine key={`${e.seq}-${e.kind}`} e={e} />).filter(Boolean);
  return (
    <div id={id}>
      <h4>{title}</h4>
      {verdicts}
      <div className="msg bot">
        <MdNodes nodes={md(run.answer || "(no answer)")} />
      </div>
      {run.report ? <div className="tool">{`would save a report: ${run.report.title}`}</div> : null}
      {intents.length ? (
        <ul className="intents">
          {intents.map((t, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: lines can repeat
            <li key={i}>{t}</li>
          ))}
        </ul>
      ) : null}
      <p className="hint">{nums}</p>
      {events.length ? <div className="transcript">{lines}</div> : null}
    </div>
  );
}

/** Markdown nodes (from the chat's safe renderer) as React elements. */
function MdNodes({ nodes }: { nodes: MdNode[] }): ReactNode {
  return nodes.map((n, i) => {
    if (typeof n === "string") return n;
    const kids = <MdNodes nodes={n.children} />;
    switch (n.tag) {
      case "br":
        // biome-ignore lint/suspicious/noArrayIndexKey: static content
        return <br key={i} />;
      case "a":
        return (
          // biome-ignore lint/suspicious/noArrayIndexKey: static content
          <a key={i} href={n.href} target="_blank" rel="noopener noreferrer">
            {kids}
          </a>
        );
      default: {
        const Tag = n.tag;
        // biome-ignore lint/suspicious/noArrayIndexKey: static content
        return <Tag key={i}>{kids}</Tag>;
      }
    }
  });
}

const APPROVED: Record<string, string> = {
  allowed: "You allowed",
  allowed_always: "You always allowed",
  denied: "You said no to",
  timed_out: "No answer, so no to",
};
const SIGNED: Record<string, (site: string) => string> = {
  done: (s) => `Signed in to ${s}`,
  cancelled: (s) => `Didn't sign in to ${s}`,
  timed_out: (s) => `No answer; didn't sign in to ${s}`,
};

/** One line of a run's transcript, as the chat draws it. */
function EventLine({ e }: { e: BotEvent }) {
  const title = new Date(e.ts_ms).toLocaleString();
  switch (e.kind) {
    case "user_message":
      return (
        <div className="mine" title={title}>
          <div className="msg me">{e.text}</div>
        </div>
      );
    case "assistant_message":
      return (
        <div className="msg bot" title={title}>
          <MdNodes nodes={md(e.text)} />
        </div>
      );
    case "tool_use":
      return <div className="tool" title={title}>{`${e.name} ${e.summary}`.trim()}</div>;
    case "turn_done":
      return <div className="done" title={title}>{e.ok ? `done in ${(e.duration_ms / 1000).toFixed(1)}s` : "stopped"}</div>;
    case "dry_run":
      return <div className="dryline" title={title}>{`would ${e.action} ${e.target} on ${e.site}`}</div>;
    case "net_blocked":
      return <div className="dryline" title={title}>{`blocked a ${e.method} to ${e.host}`}</div>;
    case "error":
      return <div className="err" title={title}>{e.message}</div>;
    case "approval":
      return <div className="redline" title={title}>{`⚠ ${APPROVED[e.outcome] || e.outcome}: ${e.action} ${e.target} on ${e.site}`}</div>;
    case "blocked":
      return <div className="redline" title={title}>{`⚠ ${e.message}`}</div>;
    case "allowed_by_rule":
      return <div className="tool" title={title}>{`always allowed: ${e.action} ${e.target} on ${e.site}`}</div>;
    case "note":
      return <div className="tool note-line" title={title}>{e.text}</div>;
    case "sign_in":
      return <div className="signline" title={title}>{SIGNED[e.outcome]?.(e.site) ?? e.site}</div>;
    case "report":
      return (
        <div className="report-card" title={title}>
          <div className="kicker">Report</div>
          <div className="title">{e.title}</div>
          <div className="summary">{e.summary}</div>
        </div>
      );
    default:
      return null;
  }
}
