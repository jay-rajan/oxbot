//! Comparing engines and models per bot (#21), the parts with no I/O: how
//! a run's recorded steps match its case, the scoreboard and what a batch
//! may cost. Runs from before Claude left oxbot (engine `claude`) are kept
//! in the database but left out of what the owner sees.
//!
//! The hub runs the cases (`hub::trials`) and answers the owner
//! (`hub::compare`); the network guard is in `oxbot_browser::netguard`.

use std::collections::BTreeMap;

use oxbot_protocol::{
    EngineKind, IntentMatch, JudgeView, RunInfo, RunVerdict, ScoreRow, TrialIntent, TrialReport,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::Intent;
use crate::store::{RatingCount, TrialRunRecord};

/// A step the original turn asked about: done (the person said yes, or a
/// rule let it through) or not (they said no).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expected {
    /// The action, e.g. `click`.
    pub action: String,
    /// What it acted on.
    pub target: String,
    /// The site.
    pub site: String,
    /// Whether it should be done.
    pub expect: bool,
}

impl Expected {
    /// One line: `click button "Post" on jobs.example.test`.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.site.is_empty() || self.site == "oxbot" {
            format!("{} {}", self.action, self.target)
        } else {
            format!("{} {} on {}", self.action, self.target, self.site)
        }
    }

    fn matches(&self, intent: &Intent) -> bool {
        same(&self.action, &intent.action)
            && same(&self.target, &intent.target)
            && same(&self.site, &intent.site)
    }
}

/// The same words, ignoring case and spacing.
fn same(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// What the original turn's oxbot tools answered, given back to a run in
/// the same order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixtures {
    /// `give_task` results.
    #[serde(default)]
    pub give_task: Vec<String>,
    /// `read_reply` results.
    #[serde(default)]
    pub read_reply: Vec<String>,
    /// Answers the person gave to `ask_user`.
    #[serde(default)]
    pub ask_user: Vec<String>,
}

/// How a run's recorded steps compare with what its case expects.
#[must_use]
pub fn intent_match(expected: &[Expected], recorded: &[Intent]) -> IntentMatch {
    let mut out = IntentMatch::default();
    let mut used = vec![false; recorded.len()];
    for e in expected.iter().filter(|e| e.expect) {
        let hit = recorded
            .iter()
            .enumerate()
            .find(|(i, r)| !used[*i] && e.matches(r));
        match hit {
            Some((i, _)) => {
                used[i] = true;
                out.matched.push(e.describe());
            }
            None => out.missing.push(e.describe()),
        }
    }
    for (i, r) in recorded.iter().enumerate() {
        if used[i] {
            continue;
        }
        if expected.iter().any(|e| !e.expect && e.matches(r)) {
            out.should_not.push(r.describe());
        } else if r.is_change() {
            out.extra.push(r.describe());
        }
    }
    out
}

/// Model requests one run of a case may take, for the estimate: 1.5 times
/// the original's steps, at least 10 and at most 40.
#[must_use]
pub fn requests_per_run(original_steps: u32) -> u32 {
    (original_steps.saturating_mul(3) / 2).clamp(10, 40)
}

/// The most tool calls one run may make: twice the original's, at least 10
/// and at most 40. Past it the run ends `over_budget`.
#[must_use]
pub fn step_cap(original_steps: u32) -> u32 {
    original_steps.saturating_mul(2).clamp(10, 40)
}

/// Statuses left out of pass rates (shown as counts instead).
const EXCLUDED: [&str; 3] = ["needs_sign_in", "rate_limited", "stopped"];

fn verdict_of(s: Option<&str>) -> Option<RunVerdict> {
    match s? {
        "pass" => Some(RunVerdict::Pass),
        "partial" => Some(RunVerdict::Partial),
        "fail" => Some(RunVerdict::Fail),
        _ => None,
    }
}

/// A verdict's name in the database.
#[must_use]
pub fn verdict_name(v: RunVerdict) -> &'static str {
    match v {
        RunVerdict::Pass => "pass",
        RunVerdict::Partial => "partial",
        RunVerdict::Fail => "fail",
    }
}

fn points(v: RunVerdict) -> f64 {
    match v {
        RunVerdict::Pass => 1.0,
        RunVerdict::Partial => 0.5,
        RunVerdict::Fail => 0.0,
    }
}

/// The engine called `name` in the database; `None` for one oxbot no
/// longer has (`claude`).
#[must_use]
pub fn engine_of(name: &str) -> Option<EngineKind> {
    crate::engine::EngineKind::parse(name).map(crate::engine::to_wire)
}

#[allow(clippy::cast_precision_loss, reason = "counts and tokens fit in f64")]
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        f64::midpoint(values[mid - 1], values[mid])
    } else {
        values[mid]
    })
}

/// A bot's scoreboard from its runs (`original`, `replay`, `shadow`) and its
/// live thumbs: one row per engine and model, Codex's first (the engine
/// new bots run on, which the others are measured against).
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "counts and tokens fit in f64")]
pub fn scoreboard(runs: &[TrialRunRecord], ratings: &[RatingCount]) -> Vec<ScoreRow> {
    let mut groups: BTreeMap<(String, String), Vec<&TrialRunRecord>> = BTreeMap::new();
    for run in runs
        .iter()
        .filter(|r| r.status != "queued" && r.status != "running" && engine_of(&r.engine).is_some())
    {
        groups
            .entry((run.engine.clone(), run.model.clone()))
            .or_default()
            .push(run);
    }
    for r in ratings.iter().filter(|r| engine_of(&r.engine).is_some()) {
        groups
            .entry((r.engine.clone(), r.model.clone()))
            .or_default();
    }
    let mut rows: Vec<ScoreRow> = groups
        .into_iter()
        .filter_map(|((engine, model), runs)| score_row(&engine, model, &runs, ratings))
        .collect();
    // Codex first; then by model.
    rows.sort_by(|a, b| {
        (a.engine != EngineKind::Codex, &a.model).cmp(&(b.engine != EngineKind::Codex, &b.model))
    });
    let codex_best = rows
        .iter()
        .filter(|r| r.engine == EngineKind::Codex)
        .filter_map(|r| r.pass_rate)
        .fold(None, |best: Option<f64>, p| {
            Some(best.map_or(p, |b| b.max(p)))
        });
    for row in &mut rows {
        row.good_enough = row.n >= 5
            && row.should_not == 0
            && match (row.pass_rate, codex_best) {
                (Some(p), Some(c)) => p >= c - 0.10,
                (Some(_), None) => true,
                _ => false,
            };
    }
    rows
}

/// One engine and model's row, from its finished runs (`None` for an
/// engine oxbot no longer has).
#[allow(clippy::cast_precision_loss, reason = "counts and tokens fit in f64")]
fn score_row(
    engine: &str,
    model: String,
    runs: &[&TrialRunRecord],
    ratings: &[RatingCount],
) -> Option<ScoreRow> {
    let kind = engine_of(engine)?;
    let counted: Vec<&&TrialRunRecord> = runs
        .iter()
        .filter(|r| !EXCLUDED.contains(&r.status.as_str()))
        .collect();
    let count = |status: &str| {
        u32::try_from(runs.iter().filter(|r| r.status == status).count()).unwrap_or(0)
    };
    let verdicts: Vec<RunVerdict> = counted
        .iter()
        .filter_map(|r| verdict_of(r.verdict.as_deref()))
        .collect();
    let pass_rate = (!verdicts.is_empty())
        .then(|| verdicts.iter().map(|v| points(*v)).sum::<f64>() / verdicts.len() as f64);
    let both: Vec<(RunVerdict, RunVerdict)> = runs
        .iter()
        .filter_map(|r| {
            let judge = r
                .judge
                .as_ref()
                .and_then(|j| j.get("verdict"))
                .and_then(Value::as_str);
            Some((verdict_of(r.owner_verdict.as_deref())?, verdict_of(judge)?))
        })
        .collect();
    let agreement = (!both.is_empty())
        .then(|| both.iter().filter(|(a, b)| a == b).count() as f64 / both.len() as f64);
    let nums =
        |f: &dyn Fn(&TrialRunRecord) -> f64| -> Vec<f64> { counted.iter().map(|r| f(r)).collect() };
    let tokens_known: Vec<&&&TrialRunRecord> =
        counted.iter().filter(|r| r.input_tokens > 0).collect();
    let matches: Vec<f64> = counted
        .iter()
        .filter_map(|r| {
            let m: IntentMatch = serde_json::from_value(r.intent_match.clone()?).ok()?;
            let want = m.matched.len() + m.missing.len();
            (want > 0).then(|| m.matched.len() as f64 / want as f64)
        })
        .collect();
    let should_not: usize = runs
        .iter()
        .filter_map(|r| {
            let m: IntentMatch = serde_json::from_value(r.intent_match.clone()?).ok()?;
            Some(m.should_not.len())
        })
        .sum();
    let sum = |f: &dyn Fn(&TrialRunRecord) -> u32| runs.iter().map(|r| f(r)).sum::<u32>();
    let thumbs = ratings
        .iter()
        .find(|t| t.engine == *engine && t.model == model);
    Some(ScoreRow {
        engine: kind,
        model,
        n: u32::try_from(counted.len()).unwrap_or(0),
        pass_rate,
        judged: u32::try_from(verdicts.len()).unwrap_or(0),
        needs_sign_in: count("needs_sign_in"),
        rate_limited: count("rate_limited"),
        stopped: count("stopped"),
        steps: median(nums(&|r| f64::from(r.steps))),
        requests: median(
            counted
                .iter()
                .filter(|r| r.requests > 0)
                .map(|r| f64::from(r.requests))
                .collect(),
        ),
        input_tokens: median(tokens_known.iter().map(|r| r.input_tokens as f64).collect()),
        output_tokens: median(
            tokens_known
                .iter()
                .map(|r| r.output_tokens as f64)
                .collect(),
        ),
        cost_usd: median(tokens_known.iter().map(|r| r.cost_usd).collect()),
        ms: median(
            counted
                .iter()
                .filter_map(|r| Some((r.ended_ms? - r.started_ms?) as f64))
                .collect(),
        ),
        intents: (!counted.is_empty()).then(|| {
            counted.iter().map(|r| f64::from(r.intents)).sum::<f64>() / counted.len() as f64
        }),
        intent_match: (!matches.is_empty())
            .then(|| matches.iter().sum::<f64>() / matches.len() as f64),
        should_not: u32::try_from(should_not).unwrap_or(0),
        refusals: sum(&|r| r.refusals),
        errors: sum(&|r| r.errors),
        net_blocked: sum(&|r| r.net_blocked),
        agreement,
        thumbs_up: thumbs.map_or(0, |t| t.up),
        thumbs_down: thumbs.map_or(0, |t| t.down),
        good_enough: false,
    })
}

/// A run as the owner's page shows it; `None` for a run on an engine
/// oxbot no longer has.
#[must_use]
pub fn run_info(r: &TrialRunRecord, case_title: &str) -> Option<RunInfo> {
    let engine = engine_of(&r.engine)?;
    let intents: Vec<Intent> = serde_json::from_value(r.intent_list.clone()).unwrap_or_default();
    Some(RunInfo {
        id: r.id.clone(),
        batch: r.batch.clone(),
        case_id: r.case_id.clone(),
        case_title: case_title.to_owned(),
        bot: r.bot.clone(),
        kind: r.kind.clone(),
        engine,
        provider: r.provider.clone(),
        model: r.model.clone(),
        started_ms: r.started_ms,
        ended_ms: r.ended_ms,
        status: r.status.clone(),
        steps: r.steps,
        requests: r.requests,
        input_tokens: r.input_tokens,
        output_tokens: r.output_tokens,
        cached_tokens: r.cached_tokens,
        cost_usd: r.cost_usd,
        intents: r.intents,
        refusals: r.refusals,
        errors: r.errors,
        net_blocked: r.net_blocked,
        answer: r.answer.clone(),
        report: r
            .report
            .clone()
            .and_then(|v| serde_json::from_value::<TrialReport>(v).ok()),
        intent_list: intents.iter().map(trial_intent).collect(),
        intent_match: r
            .intent_match
            .clone()
            .and_then(|v| serde_json::from_value(v).ok()),
        verdict: verdict_of(r.verdict.as_deref()),
        verdict_by: r.verdict_by.clone(),
        judge: r
            .judge
            .clone()
            .and_then(|v| serde_json::from_value::<JudgeView>(v).ok()),
        owner_verdict: verdict_of(r.owner_verdict.as_deref()),
        web_age_days: r.web_age_days,
    })
}

/// A recorded step as the page shows it.
#[must_use]
pub fn trial_intent(i: &Intent) -> TrialIntent {
    TrialIntent {
        kind: i.kind,
        action: i.action.clone(),
        target: i.target.clone(),
        site: i.site.clone(),
        click: i.click.map(|c| c.as_str().to_owned()),
        by_rule: i.by_rule,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Click;
    use oxbot_protocol::ApprovalKind;
    use serde_json::json;

    fn intent(target: &str, click: Click) -> Intent {
        Intent {
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: target.into(),
            site: "jobs.example.test".into(),
            click: Some(click),
            by_rule: false,
        }
    }

    fn expected(target: &str, expect: bool) -> Expected {
        Expected {
            action: "click".into(),
            target: target.into(),
            site: "jobs.example.test".into(),
            expect,
        }
    }

    #[test]
    fn intents_match_miss_add_and_repeat_a_no() {
        let want = [
            expected("button \"Post\"", true),
            expected("button \"Save draft\"", true),
            expected("button \"Delete\"", false),
        ];
        let did = [
            intent("button  \"post\"", Click::Change),
            intent("button \"Delete\"", Click::Final),
            intent("button \"Share\"", Click::Change),
            intent("switch \"Remote\"", Click::Plain),
        ];
        let m = intent_match(&want, &did);
        assert_eq!(m.matched, ["click button \"Post\" on jobs.example.test"]);
        assert_eq!(
            m.missing,
            ["click button \"Save draft\" on jobs.example.test"]
        );
        assert_eq!(
            m.should_not,
            ["click button \"Delete\" on jobs.example.test"]
        );
        assert_eq!(
            m.extra,
            ["click button \"Share\" on jobs.example.test"],
            "toggles aren't extra changes"
        );
        assert_eq!(intent_match(&[], &[]), IntentMatch::default());
    }

    #[test]
    fn estimates_and_step_caps() {
        assert_eq!(requests_per_run(0), 10);
        assert_eq!(requests_per_run(12), 18);
        assert_eq!(requests_per_run(100), 40);
        assert_eq!(step_cap(3), 10);
        assert_eq!(step_cap(8), 16);
        assert_eq!(step_cap(50), 40);
    }

    fn run(engine: &str, model: &str, status: &str, verdict: Option<&str>) -> TrialRunRecord {
        TrialRunRecord {
            id: format!("{engine}{model}{status}{verdict:?}"),
            bot: "jobs".into(),
            kind: "replay".into(),
            engine: engine.into(),
            provider: String::new(),
            model: model.into(),
            started_ms: Some(1000),
            ended_ms: Some(3000),
            status: status.into(),
            steps: 4,
            requests: 5,
            input_tokens: 100,
            output_tokens: 10,
            verdict: verdict.map(Into::into),
            intent_list: json!([]),
            ..TrialRunRecord::default()
        }
    }

    #[test]
    fn the_scoreboard_puts_codex_first_and_leaves_out_inconclusive_runs() {
        let mut runs = vec![
            run("openrouter", "a:free", "done", Some("pass")),
            run("openrouter", "a:free", "done", Some("partial")),
            run("openrouter", "a:free", "needs_sign_in", None),
            run("codex", "a:free", "done", Some("pass")),
            run("codex", "a:free", "running", None),
            // From before Claude left oxbot: kept, not shown.
            run("claude", "sonnet", "done", Some("pass")),
        ];
        runs[0].owner_verdict = Some("pass".into());
        runs[0].judge = Some(json!({"verdict": "pass"}));
        runs[1].owner_verdict = Some("partial".into());
        runs[1].judge = Some(json!({"verdict": "fail"}));
        runs[1].intent_match =
            Some(json!({"matched": ["x"], "missing": ["y"], "extra": [], "should_not": []}));
        let ratings = [
            RatingCount {
                engine: "openrouter".into(),
                model: "b:free".into(),
                up: 2,
                down: 1,
            },
            RatingCount {
                engine: "claude".into(),
                model: "sonnet".into(),
                up: 1,
                down: 0,
            },
        ];
        let rows = scoreboard(&runs, &ratings);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].engine, rows[0].model.as_str()),
            (EngineKind::Codex, "a:free")
        );
        assert!(run_info(&runs[5], "x").is_none());
        assert_eq!(
            run_info(&runs[3], "x").map(|r| r.engine),
            Some(EngineKind::Codex)
        );
        assert_eq!(rows[0].n, 1, "a running run isn't counted");
        let a = &rows[1];
        assert_eq!((a.n, a.judged, a.needs_sign_in), (2, 2, 1));
        assert_eq!(a.pass_rate, Some(0.75));
        assert_eq!(a.agreement, Some(0.5));
        assert_eq!(a.intent_match, Some(0.5));
        assert_eq!(a.steps, Some(4.0));
        assert_eq!(a.ms, Some(2000.0));
        assert!(!a.good_enough, "too few runs");
        assert_eq!(
            (rows[2].thumbs_up, rows[2].thumbs_down, rows[2].n),
            (2, 1, 0)
        );
    }

    #[test]
    fn good_enough_needs_five_runs_near_codex_and_no_repeated_no() {
        let mut runs: Vec<TrialRunRecord> = (0..5)
            .map(|i| {
                let mut r = run("openrouter", "a:free", "done", Some("pass"));
                r.id = format!("a{i}");
                r
            })
            .collect();
        runs.push(run("codex", "b:free", "done", Some("pass")));
        let rows = scoreboard(&runs, &[]);
        assert!(rows[1].good_enough);
        runs[0].intent_match =
            Some(json!({"matched": [], "missing": [], "extra": [], "should_not": ["x"]}));
        assert!(!scoreboard(&runs, &[])[1].good_enough);
    }
}
