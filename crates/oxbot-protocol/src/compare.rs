//! Comparing engines and models per bot (#21): test cases, practice runs
//! and the scoreboard. Every message here is the Farm owner's only.

use serde::{Deserialize, Serialize};

use crate::{ApprovalKind, EngineKind};

/// What a test case is run on: an engine and one model, with no fallback.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Target {
    /// The engine.
    pub engine: EngineKind,
    /// The model id (`vendor/model:free`).
    pub model: String,
}

/// How much a practice run's pages may send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum TrialNetwork {
    /// Only reads (GET): every other request is blocked.
    #[default]
    Strict,
    /// Also reads sent as POST by scripts.
    Xhr,
}

/// A verdict on a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum RunVerdict {
    /// It did the task.
    Pass,
    /// Partly.
    Partial,
    /// It didn't.
    Fail,
}

/// A test case, for the list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TestCaseInfo {
    /// Its id.
    pub id: String,
    /// The bot.
    pub bot: String,
    /// Its title.
    pub title: String,
    /// The message it sends.
    pub prompt: String,
    /// `reads` or `acts` (it would change something live).
    pub kind: String,
    /// The owner's checks, one per line.
    pub rubric: String,
    /// The sites the original opened.
    #[serde(default)]
    pub sites: Vec<String>,
    /// It needs the browser's sign-ins (its runs get a copy of them).
    #[serde(default)]
    pub uses_sign_ins: bool,
    /// How much its runs' pages may send.
    #[serde(default)]
    pub network: TrialNetwork,
    /// When it was made (ms since the Unix epoch).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_ms: i64,
    /// Hidden from the list.
    #[serde(default)]
    pub archived: bool,
    /// The latest result on each engine and model.
    #[serde(default)]
    pub last: Vec<CaseResult>,
}

/// A case's latest result on one engine and model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CaseResult {
    /// The engine.
    pub engine: EngineKind,
    /// The model.
    pub model: String,
    /// The run's status.
    pub status: String,
    /// Its verdict, if any.
    #[serde(default)]
    pub verdict: Option<RunVerdict>,
}

/// A step a practice run recorded instead of doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TrialIntent {
    /// What kind of step.
    pub kind: ApprovalKind,
    /// The action, e.g. `click`.
    pub action: String,
    /// What it acts on, e.g. `button "Post"`.
    pub target: String,
    /// The site.
    pub site: String,
    /// For a click: `change`, `final`, `plain`, `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<String>,
    /// One of the bot's "Always allow" rules would have let it run live.
    #[serde(default)]
    pub by_rule: bool,
}

/// How a run's recorded steps compare with what the case expects, each as
/// one line (`click button "Post" on jobs.example.test`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IntentMatch {
    /// Expected, and done.
    pub matched: Vec<String>,
    /// Expected, not done.
    pub missing: Vec<String>,
    /// Changes done that weren't expected.
    pub extra: Vec<String>,
    /// Done though the owner said no to it in the original.
    pub should_not: Vec<String>,
}

/// One of the owner's checks, as a judge saw it (runs judged before
/// Claude left oxbot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JudgeCheck {
    /// The check.
    pub check: String,
    /// `yes`, `no` or `unclear`.
    pub met: String,
    /// Why, in a line.
    pub why: String,
}

/// What a judge said about a run (runs judged before Claude left oxbot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JudgeView {
    /// Each check.
    pub checks: Vec<JudgeCheck>,
    /// Its verdict.
    pub verdict: RunVerdict,
    /// 0 to 100.
    pub score: u32,
    /// Notes.
    #[serde(default)]
    pub notes: String,
    /// The model that judged.
    pub model: String,
    /// Judged by a local model.
    #[serde(default)]
    pub local: bool,
}

/// A report a practice run would have saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TrialReport {
    /// Its title.
    pub title: String,
    /// One line about it.
    pub summary: String,
    /// The report as the bot wrote it.
    pub markdown: String,
    /// The pages it cites.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// One run of a case (or the original turn), with its numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunInfo {
    /// Its id.
    pub id: String,
    /// Its batch.
    #[serde(default)]
    pub batch: Option<String>,
    /// Its case.
    #[serde(default)]
    pub case_id: Option<String>,
    /// The case's title.
    #[serde(default)]
    pub case_title: String,
    /// The bot.
    pub bot: String,
    /// `original` or `replay`.
    pub kind: String,
    /// The engine.
    pub engine: EngineKind,
    /// Who served the model (`openrouter`).
    pub provider: String,
    /// The model.
    pub model: String,
    /// When it started.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub started_ms: Option<i64>,
    /// When it ended.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub ended_ms: Option<i64>,
    /// `queued`, `running`, `done`, `failed`, `stopped`, `needs_sign_in`,
    /// `over_budget`, `rate_limited` or `judge_error`.
    pub status: String,
    /// Tool calls.
    pub steps: u32,
    /// Model requests.
    pub requests: u32,
    /// Input tokens.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub input_tokens: u64,
    /// Output tokens.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub output_tokens: u64,
    /// Cached input tokens.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cached_tokens: u64,
    /// US dollars.
    pub cost_usd: f64,
    /// Steps a live run would have asked about (cards).
    pub intents: u32,
    /// Steps refused.
    pub refusals: u32,
    /// Errors.
    pub errors: u32,
    /// Requests the network guard stopped.
    pub net_blocked: u32,
    /// The final answer.
    pub answer: String,
    /// A report it would have saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<TrialReport>,
    /// What it recorded instead of doing.
    #[serde(default)]
    pub intent_list: Vec<TrialIntent>,
    /// How that matched the case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_match: Option<IntentMatch>,
    /// The verdict: the owner's, else the judge's.
    #[serde(default)]
    pub verdict: Option<RunVerdict>,
    /// `owner`, `judge` or `local_judge`.
    #[serde(default)]
    pub verdict_by: Option<String>,
    /// What the judge said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeView>,
    /// The owner's own verdict.
    #[serde(default)]
    pub owner_verdict: Option<RunVerdict>,
    /// Days between the original turn and this run (live sites change).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub web_age_days: Option<i64>,
}

/// One engine and model's row on a bot's scoreboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScoreRow {
    /// The engine.
    pub engine: EngineKind,
    /// The model.
    pub model: String,
    /// Runs counted.
    pub n: u32,
    /// Share passed, 0 to 1 (partial counts half), over runs with a
    /// verdict; `None` without one.
    #[serde(default)]
    pub pass_rate: Option<f64>,
    /// Runs with a verdict.
    #[serde(default)]
    pub judged: u32,
    /// Runs left out: no sign-in possible.
    #[serde(default)]
    pub needs_sign_in: u32,
    /// Runs left out: rate limited.
    #[serde(default)]
    pub rate_limited: u32,
    /// Runs left out: stopped.
    #[serde(default)]
    pub stopped: u32,
    /// Median tool calls.
    #[serde(default)]
    pub steps: Option<f64>,
    /// Median model requests.
    #[serde(default)]
    pub requests: Option<f64>,
    /// Median input tokens (runs that counted them).
    #[serde(default)]
    pub input_tokens: Option<f64>,
    /// Median output tokens.
    #[serde(default)]
    pub output_tokens: Option<f64>,
    /// Median US dollars.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Median time in milliseconds.
    #[serde(default)]
    pub ms: Option<f64>,
    /// Mean steps recorded instead of done.
    #[serde(default)]
    pub intents: Option<f64>,
    /// Share of expected steps done, 0 to 1.
    #[serde(default)]
    pub intent_match: Option<f64>,
    /// Steps done that the owner had said no to.
    #[serde(default)]
    pub should_not: u32,
    /// Refusals in all.
    #[serde(default)]
    pub refusals: u32,
    /// Errors in all.
    #[serde(default)]
    pub errors: u32,
    /// Blocked requests in all.
    #[serde(default)]
    pub net_blocked: u32,
    /// Share of runs with both where the owner and the judge agreed.
    #[serde(default)]
    pub agreement: Option<f64>,
    /// Live thumbs up on this engine and model's replies.
    #[serde(default)]
    pub thumbs_up: u32,
    /// Live thumbs down.
    #[serde(default)]
    pub thumbs_down: u32,
    /// At least 5 runs, a pass rate within 10 points of Codex's best, and
    /// no step the owner had said no to.
    #[serde(default)]
    pub good_enough: bool,
}

/// What a batch would take, before it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BatchEstimate {
    /// The bot.
    pub bot: String,
    /// Runs in all.
    pub runs: u32,
    /// `OpenRouter` requests it may take.
    pub openrouter_requests: u32,
    /// `OpenRouter` requests left today, if known.
    #[serde(default)]
    pub openrouter_left: Option<u32>,
    /// Kept for live bots.
    #[serde(default)]
    pub reserve: u32,
    /// Whether any target is a paid model.
    #[serde(default)]
    pub paid: bool,
    /// Whether it fits in what's left today.
    pub fits: bool,
    /// Targets refused for these cases, and why.
    #[serde(default)]
    pub refused: Vec<String>,
}

/// Settings → Engines → Comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(default)]
pub struct ComparePrefs {
    /// Share of the day's free `OpenRouter` requests kept for live bots,
    /// in percent (0 to 90).
    pub reserve_pct: u32,
}

impl Default for ComparePrefs {
    fn default() -> Self {
        Self { reserve_pct: 30 }
    }
}

/// What the "Test case…" sheet starts with for a finished turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CaseDraft {
    /// The bot.
    pub bot: String,
    /// The turn's `TurnDone` event.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: u64,
    /// The prompt's first line.
    pub title: String,
    /// One check per step the original asked about.
    pub rubric: String,
    /// `reads` or `acts`.
    pub kind: String,
    /// The sites it opened.
    pub sites: Vec<String>,
    /// Whether any of them are signed in.
    pub uses_sign_ins: bool,
}
