// Compare (#21, a developer tool for the owner): a bot's test cases replayed
// on other models, the runs, and a scoreboard. Opened with
// `ui/open {dialog: "compare", arg: "<bot>"}` or `"<bot>:cmpCases"`.
import { type FormEvent, type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import {
  Button,
  Checkbox,
  Dialog,
  DialogFrame,
  Field,
  Hint,
  Input,
  List,
  ListRow,
  Note,
  Pill,
  pct,
  Section,
  Select,
  Tabs,
  vars,
} from "../../components/ui";
import type { BotEvent, IntentMatch, RunInfo, Target } from "../../net/protocol";
import type { CmpTab, RunDetail } from "../../state/cards";
import { local } from "../../state/store";
import { md, type MdNode } from "../Chat/md";
import { comparable, isOwner } from "./devTools";
import { cmpMoney, cmpNum, cmpPct, cmpSecs, heldBack, targetName, VERDICT } from "./format";
import "./compare.css";

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
    <Dialog id="compareDlg" size="xl" open={open && !!bot} onClose={onClose} labelledBy="cmpTitle">
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
  const choices: Target[] = (ready ? (engines?.models ?? []) : []).map((m) => ({ engine: "codex", model: m.id }));

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
  const fill = pct(p?.done ?? 0, p?.total ?? 0);

  return (
    <DialogFrame title={`Compare · ${name || bot}`} titleId="cmpTitle" onClose={onClose} closeId="cmpClose" bodyClassName="cmp-body">
      <Tabs label="Compare" tabs={TABS} value={cmp.tab} onChange={tab} className="cmp-tabs" />
      <div id="cmpScore" className="cmp-pane" role="tabpanel" hidden={cmp.tab !== "cmpScore"}>
        <Section>
          <div className="cmp-table-wrap">
            <table className="cmp-table">
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
                            <Pill tone="ok" className="good">
                              good enough
                            </Pill>
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
                    <td colSpan={17} className="cmp-empty">
                      No runs yet.
                    </td>
                  </tr>
                ) : null}
              </tbody>
            </table>
          </div>
          <Hint>Pass uses your marks. Good enough: 5+ runs, within 10 points of the best, no step you said no to.</Hint>
        </Section>
      </div>
      <div id="cmpCases" className="cmp-pane" role="tabpanel" hidden={cmp.tab !== "cmpCases"}>
        <Section>
          <List className="cmp-list" id="cmpCaseList">
            {cmp.cases
              .filter((c) => !c.archived)
              .map((c) => {
                const about = [(c.sites ?? []).join(", "), c.uses_sign_ins ? "signed in" : "", c.network === "xhr" ? "reads by POST allowed" : ""]
                  .filter(Boolean)
                  .join(" · ");
                return (
                  <ListRow
                    key={c.id}
                    lead={
                      <Checkbox
                        value={c.id}
                        aria-label={c.title}
                        checked={cases.includes(c.id)}
                        onChange={(e) => {
                          setCases(e.target.checked ? [...cases, c.id] : cases.filter((x) => x !== c.id));
                          changed();
                        }}
                      >
                        <span className="visually-hidden">{c.title}</span>
                      </Checkbox>
                    }
                    title={
                      <span className="cmp-case-title">
                        <span>{c.title}</span>
                        <Pill tone={c.kind === "acts" ? "warn" : undefined} className={c.kind}>
                          {c.kind}
                        </Pill>
                      </span>
                    }
                    sub={
                      about || (c.last ?? []).length ? (
                        <span className="cmp-case-sub">
                          {about ? <span>{about}</span> : null}
                          {(c.last ?? []).length ? (
                            <span className="cmp-vdots">
                              {(c.last ?? []).map((r) => (
                                <span key={`${r.engine}/${r.model}`} className={`vdot ${r.verdict || ""}`} title={`${targetName(r)}: ${r.verdict || r.status}`} />
                              ))}
                            </span>
                          ) : null}
                        </span>
                      ) : undefined
                    }
                    trail={<DeleteCase id={c.id} />}
                  />
                );
              })}
          </List>
          <Note id="cmpCasesEmpty" hidden={cmp.cases.length > 0}>
            No test cases yet. Pick "Test case…" under one of this bot's replies.
          </Note>
        </Section>
        <Section id="cmpRunBox" title="Run selected on" titleId="cmpRunBoxH">
          <div className="cmp-targets" id="cmpTargets">
            {choices.map((t) => (
              <Checkbox
                key={t.model}
                value={JSON.stringify(t)}
                checked={targets.includes(t.model)}
                onChange={(e) => {
                  setTargets(e.target.checked ? [...targets, t.model] : targets.filter((x) => x !== t.model));
                  changed();
                }}
              >
                {targetName(t)}
              </Checkbox>
            ))}
            {!choices.length ? <Note>Connect the AI account in Settings → AI models to pick models.</Note> : null}
          </div>
          <Field label="Times each" className="cmp-repeats">
            <Select
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
            </Select>
          </Field>
          <Hint id="cmpEstimate" hidden={!cmp.estimate}>
            {cmp.estimate}
          </Hint>
          <div className="btn-row">
            <Button
              kind="secondary"
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
            </Button>
            <Button
              kind="primary"
              id="cmpStart"
              disabled={!cmp.canStart}
              onClick={() => {
                send({ type: "start_batch", ...picked() });
                dispatch(local("cards/cmpStarted"));
              }}
            >
              Start
            </Button>
            <Button kind="secondary" id="cmpStop" hidden={!running} onClick={() => p && send({ type: "stop_batch", id: p.batch })}>
              Stop
            </Button>
          </div>
          <div className="cmp-progress" id="cmpProgress" hidden={!running} style={vars({ fill })}>
            <i />
          </div>
          <Hint id="cmpProgressText" hidden={!p}>
            {!p ? "" : p.status === "paused" ? `Paused: ${p.paused_reason || "waiting"}` : `${p.done} of ${p.total}${running ? "" : ` · ${p.status}`}`}
          </Hint>
          <Hint>Test runs use their own browser and block every write, except links that act when opened.</Hint>
        </Section>
      </div>
      <div id="cmpRuns" className="cmp-pane" role="tabpanel" hidden={cmp.tab !== "cmpRuns"}>
        <Section id="cmpRunListBox" hidden={!!cmp.detail}>
          <List className="cmp-list" id="cmpRunList">
            {cmp.runs.map((r) => {
              const openRun = () => send({ type: "open_run", run: r.id });
              return (
                <ListRow
                  key={r.id}
                  className="run"
                  tabIndex={0}
                  onClick={openRun}
                  onKeyDown={(ev) => {
                    if (ev.key === "Enter") openRun();
                  }}
                  title={`${r.case_title || "Run"} · ${targetName(r)}`}
                  sub={[
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
                  trail={
                    <Pill tone={r.verdict === "pass" ? "ok" : r.verdict === "partial" ? "warn" : r.verdict === "fail" ? "danger" : undefined}>
                      {r.verdict ? VERDICT[r.verdict] : r.status === "running" ? "running…" : "–"}
                    </Pill>
                  }
                />
              );
            })}
          </List>
          <Note id="cmpRunsEmpty" hidden={cmp.runs.length > 0}>
            No runs yet.
          </Note>
        </Section>
        <Prefs reservePct={reservePct} />
        <Section id="cmpRunDetail" hidden={!cmp.detail}>
          <div className="btn-row">
            <Button kind="quiet" size="sm" id="cmpRunBack" onClick={() => dispatch(local("cards/cmpBack"))}>
              ← Runs
            </Button>
          </div>
          {cmp.detail ? <Detail d={cmp.detail} /> : <div className="side-by-side" />}
        </Section>
      </div>
    </DialogFrame>
  );
}

function DeleteCase({ id }: { id: string }) {
  const { send } = useApp();
  const [armed, setArmed] = useState(false);
  return (
    <Button
      kind={armed ? "danger" : "quiet"}
      size="sm"
      onClick={() => {
        if (armed) send({ type: "delete_test_case", id });
        else setArmed(true);
      }}
    >
      {armed ? "Yes, delete" : "Delete"}
    </Button>
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
    <Section id="cmpPrefsSection" hidden={reservePct == null} title="Comparison" titleId="cmpPrefsH">
      <form className="engine-prefs cmp-prefs" id="cmpPrefsForm" onSubmit={submit}>
        <Field label="Free questions kept for live bots (%)" hint="Test runs get a copy of your sign-ins and delete it when they end.">
          <Input type="number" id="cmpReserve" min={0} max={90} step={1} required value={reserve} onChange={(e) => setReserve(e.target.value)} />
        </Field>
        <div className="btn-row">
          <Button
            kind="danger"
            id="cmpDeleteRuns"
            className="cmp-start"
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
          </Button>
          <Button kind="primary" type="submit">
            Save
          </Button>
        </div>
      </form>
    </Section>
  );
}

/** A run beside the original it replays. */
function Detail({ d }: { d: RunDetail }) {
  const { send } = useApp();
  const run = d.run;
  const judge = run.judge;
  const judgeText = judge?.checks?.length
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
        <Button
          key={v}
          kind="secondary"
          size="sm"
          className="cmp-verdict"
          title={VERDICT[v]}
          aria-pressed={run.owner_verdict === v}
          onClick={() => send({ type: "set_run_verdict", run: run.id, verdict: run.owner_verdict === v ? null : v })}
        >
          {label}
        </Button>
      ))}
      {/* Runs judged before Claude left oxbot keep what the judge said. */}
      {judgeText ? <Hint>{judgeText}</Hint> : null}
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
  events: BotEvent[] | null;
  isOriginal: boolean;
  match: IntentMatch | null;
  verdicts?: ReactNode;
}) {
  if (!run)
    return (
      <div id={id}>
        <h4 className="cmp-side-title">{title}</h4>
        <Note>Not kept.</Note>
      </div>
    );
  const intents: string[] = [];
  if (!isOriginal && match) {
    for (const [list, mark] of [
      [match.matched ?? [], "✓ matched"],
      [match.missing ?? [], "✗ missing"],
      [match.extra ?? [], "＋ extra"],
      [match.should_not ?? [], "⚠ said no before"],
    ] as const)
      for (const line of list) intents.push(`${mark}: ${line}`);
  } else for (const i of run.intent_list ?? []) intents.push(`would ${i.action} ${i.target} on ${i.site}`);
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
  const lines = (events ?? []).map((e) => <EventLine key={`${e.seq}-${e.kind}`} e={e} />).filter(Boolean);
  return (
    <div id={id}>
      <h4 className="cmp-side-title">{title}</h4>
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
      <Hint>{nums}</Hint>
      {events?.length ? <div className="transcript">{lines}</div> : null}
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
