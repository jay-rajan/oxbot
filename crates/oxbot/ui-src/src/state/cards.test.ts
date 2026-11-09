import { describe, expect, it } from "vitest";
import type { Approval, BatchEstimate, Day, RunInfo, ServerMsg } from "../net/protocol";
import { cardList, cardsReducer, editOf, estimateText, initialCards, ruleOf, splitCards, waitingBots, type CardsState } from "./cards";
import { type Action, local } from "./store";

const srv = (msg: unknown): Action => ({ kind: "server", msg: msg as ServerMsg });
const run = (state: CardsState, ...actions: Action[]) => actions.reduce(cardsReducer, state);

const approval = (id: number, extra: Partial<Approval> = {}): Approval => ({
  id,
  bot: "recipes",
  kind: "change",
  action: "click",
  target: 'button "Post"',
  site: "jobs.example.test",
  page: null,
  detail: null,
  screenshot: null,
  asker: null,
  always: false,
  schedule: null,
  ...extra,
});
const signIn = (id: number) => ({ type: "sign_in_needed", id, bot: "mail", url: "https://x", site: "x.test", why: "", window_open: false });

describe("cards", () => {
  it("shows a bot's cards only in its own chat, and lists the other bots waiting", () => {
    const s = run(
      initialCards,
      srv({ type: "approval_needed", ...approval(1, { bot: "meal" }) }),
      srv(signIn(2)),
      srv({ type: "approval_needed", ...approval(3) }),
      srv({ type: "approval_needed", ...approval(4, { bot: "meal" }) }),
      srv({ type: "approval_needed", ...approval(5, { bot: "gone" }) }),
    );
    const listed = (b: string) => b !== "gone";
    const { here, others } = splitCards(cardList(s), "recipes", listed);
    expect(here.map((c) => c.key)).toEqual(["a:3", "a:5"]);
    expect(others).toEqual([
      { bot: "meal", signIn: false },
      { bot: "mail", signIn: true },
    ]);
    expect(splitCards(cardList(s), null, listed).here).toHaveLength(5);
    expect([...waitingBots(s)].sort()).toEqual(["gone", "mail", "meal", "recipes"]);
  });

  it("keeps cards oldest first; one sent again keeps its place", () => {
    const s = run(
      initialCards,
      srv({ type: "approval_needed", ...approval(1) }),
      srv(signIn(7)),
      srv({ type: "approval_needed", ...approval(2) }),
      srv({ type: "approval_needed", ...approval(1, { screenshot: "data:image/png;base64,x" }) }),
    );
    expect(cardList(s).map((c) => c.key)).toEqual(["a:1", "s:7", "a:2"]);
    expect(s.approvals[1]?.screenshot).toBe("data:image/png;base64,x");
  });

  it("drops resolved cards, and everything on hello", () => {
    let s = run(initialCards, srv({ type: "approval_needed", ...approval(1) }), srv(signIn(3)));
    s = run(s, srv({ type: "approval_resolved", id: 1 }), srv({ type: "sign_in_resolved", id: 3 }));
    expect(cardList(s)).toEqual([]);
    s = run(s, srv({ type: "approval_needed", ...approval(4) }), srv({ type: "hello", server_name: "Mac", server_version: "1", started_ms: 1 }));
    expect(cardList(s)).toEqual([]);
  });

  it("forgets a schedule card's changes once it's answered", () => {
    const sched = approval(5, {
      kind: "schedule",
      schedule: { task: "Plan", rule: { kind: "weekdays", time: "08:15" }, text: "Every weekday", tz: "UTC", bot_name: "Recipes", from: null },
    });
    let s = run(initialCards, srv({ type: "approval_needed", ...sched }));
    const e = editOf(s, sched);
    expect(e).toMatchObject({ open: false, task: "Plan", kind: "weekdays", time: "08:15" });
    s = run(s, local("cards/schedEdit", { id: 5, edit: { ...e, open: true, kind: "weekly", days: ["fri", "mon"] } }));
    expect(editOf(s, sched).open).toBe(true);
    expect(ruleOf(editOf(s, sched))).toEqual({ kind: "weekly", days: ["mon", "fri"], time: "08:15" });
    s = run(s, srv({ type: "approval_resolved", id: 5 }));
    expect(s.schedEdits).toEqual({});
  });

  it("turns a schedule edit into its rule", () => {
    const base = { open: true, task: "x", time: "07:00", date: "2026-12-01", days: [] as Day[], day: "31" };
    expect(ruleOf({ ...base, kind: "once" })).toEqual({ kind: "once", at: "2026-12-01T07:00" });
    expect(ruleOf({ ...base, kind: "monthly" })).toEqual({ kind: "monthly", day: 31, time: "07:00" });
    expect(ruleOf({ ...base, kind: "daily" })).toEqual({ kind: "daily", time: "07:00" });
  });

  it("keeps reports and schedules only for the open settings' bot", () => {
    let s = run(initialCards, local("cards/settingsFor", { bot: "recipes" }));
    expect(s.reports).toBeNull();
    s = run(s, srv({ type: "reports", bot: "other", reports: [] }));
    expect(s.reports).toBeNull();
    s = run(s, srv({ type: "reports", bot: "recipes", reports: [] }), srv({ type: "schedules", bot: "recipes", schedules: [] }));
    expect(s.reports).toEqual([]);
    expect(s.schedules).toEqual([]);
    s = run(s, srv({ type: "schedules_changed" }));
    expect(s.schedulesChanged).toBe(1);
    s = run(s, local("cards/settingsFor", { bot: null }));
    expect(s.reports).toBeNull();
    // Settings → Schedules: every one, kept apart from a bot's.
    s = run(s, srv({ type: "schedules", schedules: [] }));
    expect(s.allSchedules).toEqual([]);
    expect(s.schedules).toBeNull();
  });

  it("opens Compare fresh on a bot and tab", () => {
    let s = run(initialCards, local("ui/open", { dialog: "compare", arg: "recipes:cmpCases" }));
    expect(s.cmp).toMatchObject({ bot: "recipes", tab: "cmpCases", runs: [] });
    s = run(s, local("ui/open", { dialog: "compare", arg: "journal" }));
    expect(s.cmp).toMatchObject({ bot: "journal", tab: "cmpScore" });
    expect(run(s, local("ui/open", { dialog: "settings" }))).toBe(s);
  });

  it("follows runs, estimates and run details for the compared bot", () => {
    const r = { id: "r1", bot: "recipes", status: "running" } as RunInfo;
    let s = run(initialCards, local("ui/open", { dialog: "compare", arg: "recipes" }));
    s = run(s, srv({ type: "trial_run", ...r }));
    expect(s.cmp.runs.map((x) => x.status)).toEqual(["running"]);
    s = run(s, srv({ type: "run_detail", run: r, events: [], original: null, original_events: [] }));
    s = run(s, srv({ type: "trial_run", ...r, status: "done" }));
    expect(s.cmp.runs.map((x) => x.status)).toEqual(["done"]);
    expect(s.cmp.detail?.run.status).toBe("done");
    s = run(s, srv({ type: "trial_event", run: "r1", event: { bot: "recipes", seq: 1, ts_ms: 0, kind: "note", text: "hi" } }));
    expect(s.cmp.detail?.events).toHaveLength(1);
    const est: BatchEstimate = { bot: "recipes", runs: 1, openrouter_requests: 0, openrouter_left: null, reserve: 0, paid: false, fits: true, refused: [] };
    s = run(s, srv({ type: "batch_estimate", ...est }));
    expect(s.cmp).toMatchObject({ estimate: "1 run · 0 paid", canStart: true });
    s = run(s, local("cards/cmpEstimate"));
    expect(s.cmp).toMatchObject({ estimate: "", canStart: false });
    // Another bot's numbers don't land here.
    expect(run(s, srv({ type: "scoreboard", bot: "journal", rows: [] }))).toBe(s);
  });

  it("writes an estimate that doesn't fit", () => {
    expect(
      estimateText({ bot: "b", runs: 4, openrouter_requests: 40, openrouter_left: 30, reserve: 10, paid: true, fits: false, refused: ["x: no"] }),
    ).toBe(
      "4 runs · ≈ 40 free questions of 30 left today (10 kept for live bots) · paid models count against your daily limit. Not run: x: no. That doesn't fit what's left today: pick fewer cases or models.",
    );
  });

  it("counts case drafts and keeps ratings per bot", () => {
    let s = run(initialCards, srv({ type: "case_draft", bot: "recipes", seq: 9, title: "t", rubric: "", kind: "reads", sites: [], uses_sign_ins: false }));
    s = run(s, srv({ type: "case_draft", bot: "recipes", seq: 9, title: "t", rubric: "", kind: "reads", sites: [], uses_sign_ins: false }));
    expect(s.caseDraft?.n).toBe(2);
    s = run(s, srv({ type: "ratings", bot: "recipes", ratings: { 9: true }, cases: [9] }));
    expect(s.rated.recipes).toEqual({ ratings: { 9: true }, cases: [9] });
  });
});
