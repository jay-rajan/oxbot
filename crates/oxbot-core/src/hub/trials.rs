//! Practice runs (#21): a test case run again on one engine and model, in a
//! sandbox where nothing can change the real world.
//!
//! - **Its own key.** A run uses the hub's runtime machinery under a
//!   synthetic bot id `t--<hex>` (`--` after a `t` never comes from a bot's
//!   name, and `bot::load` refuses it). [`Hub::bot`] never finds it, so it
//!   is never listed, tasked or shown; [`Hub::run_bot`] finds it where the
//!   engine plumbing needs it.
//! - **Its own history.** What it does is stored in `trial_events` and sent
//!   only to the owner, never as a chat event; its usage goes to its run.
//! - **Its own folder and browser.** `<data>/trials/<run>/work` holds a
//!   copy of the case's files; its Chrome runs on a copy of the profile
//!   (or an empty one) behind a network guard, outside `Hub::browsers`, so
//!   live bots keep their browser.
//! - **Its own gate.** Steps a live run would ask about are recorded, never
//!   shown or done (see `policy::trial_overlay`); oxbot's own tools answer
//!   from what the original turn got.
//!
//! One run at a time, app-wide. A restart stops what was running and
//! deletes its folders.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use oxbot_protocol::{
    ApprovalKind, Attachment, BotEvent, EventKind, ServerMsg, Target, TrialNetwork,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, oneshot};

use super::engines::{ChainOrigin, Entry};
use super::turns::sender_line;
use super::{Hub, HubError, Shared, lock, session::SessionOverrides};
use crate::bot::Bot;
use crate::compare::{self, Expected, Fixtures};
use crate::engine::{
    CallRecord, Decision, GateMode, HostToolCall, ModelChoice, PermissionRequest, ToolReply,
    TurnInput, from_wire,
};
use crate::id::{BotId, BrowserOwner};
use crate::policy::{self, Intent, TrialCall, TrialMode, TrialVerdict};
use crate::store::{TestCaseRecord, TrialRunRecord, now_ms};
use crate::tools;

/// What every practice run's key starts with.
pub(crate) const TRIAL_PREFIX: &str = "t--";

/// Whether `id` is a practice run's key.
#[must_use]
pub(crate) fn is_trial(id: &BotId) -> bool {
    id.as_str().starts_with(TRIAL_PREFIX)
}

/// What a run tells the model when it asks to sign in.
pub(super) const NO_SIGN_IN: &str = "Sign-in isn't possible in a test run. Say which site needed \
    it and stop.";

/// How long a run waits for its browser's network guard before refusing a
/// browser tool.
const GUARD_WAIT: Duration = Duration::from_secs(5);

/// Practice runs: what's queued and what's running.
#[derive(Default)]
pub(super) struct Trials {
    /// Runs going on, by key (at most one, but shadows may overlap a
    /// batch's run).
    active: std::sync::Mutex<HashMap<BotId, Active>>,
    /// What to run next.
    queue: std::sync::Mutex<VecDeque<Job>>,
    wake: Notify,
    /// Batches the owner stopped.
    stopped: std::sync::Mutex<HashSet<String>>,
    /// One run at a time.
    one: Mutex<()>,
}

/// Something to run.
#[derive(Debug, Clone)]
pub(super) enum Job {
    /// Every queued run of a batch, in order.
    Batch(String),
    /// One shadow run next to a live turn.
    Shadow(ShadowJob),
}

/// A shadow: the owner's live message, run on another engine at the same
/// time.
#[derive(Debug, Clone)]
pub(super) struct ShadowJob {
    pub batch: String,
    pub run: String,
    pub bot: BotId,
    /// The live turn's `UserMessage` event.
    pub live_seq: u64,
    pub prompt: String,
    pub files: Vec<Attachment>,
    pub instructions: String,
    pub target: Target,
}

/// A run going on.
struct Active {
    run: TrialRunRecord,
    /// The synthetic bot the engine runs.
    bot: Bot,
    /// Its own Chrome, if it has one.
    shared: Option<Arc<Shared>>,
    mode: TrialMode,
    intents: Vec<Intent>,
    fixtures: Fixtures,
    step_cap: u32,
    over_budget: bool,
    asked_to_sign_in: bool,
    /// The last model request was turned away for limits.
    limited: bool,
    /// The report it would have saved.
    report: Option<Value>,
    /// Fired when its turn ends (`true`: without error).
    ended: Option<oneshot::Sender<bool>>,
}

/// What a run needs to know about its case.
pub(super) struct RunInput {
    pub prompt: String,
    pub files: Vec<Attachment>,
    /// The folder the files were copied to (`cases/<id>/files`), if any.
    pub files_dir: Option<PathBuf>,
    pub instructions: String,
    pub mode: TrialMode,
    pub uses_sign_ins: bool,
    pub fixtures: Fixtures,
    pub step_cap: u32,
    /// What the original turn's relayed task had read, if anything.
    pub tainted_by: Option<String>,
    /// The case's expectations, for the intent match.
    pub expected: Vec<Expected>,
    /// When the original turn ran.
    pub original_ms: Option<i64>,
}

impl RunInput {
    /// What running `case` needs.
    pub(super) fn of_case(case: &TestCaseRecord, cases_dir: &Path) -> Self {
        let files: Vec<Attachment> = serde_json::from_value(case.files.clone()).unwrap_or_default();
        let origin = &case.origin;
        Self {
            prompt: case.prompt.clone(),
            files_dir: (!files.is_empty()).then(|| cases_dir.join(&case.id).join("files")),
            files,
            instructions: case.instructions.clone(),
            mode: TrialMode::parse(&case.network).unwrap_or_default(),
            uses_sign_ins: case.uses_sign_ins,
            fixtures: serde_json::from_value(case.fixtures.clone()).unwrap_or_default(),
            step_cap: compare::step_cap(original_steps(case)),
            tainted_by: origin
                .get("tainted_by")
                .and_then(Value::as_str)
                .map(str::to_owned),
            expected: serde_json::from_value(case.expected.clone()).unwrap_or_default(),
            original_ms: origin.get("ms").and_then(Value::as_i64),
        }
    }
}

/// The original turn's tool calls, as the case keeps them.
pub(super) fn original_steps(case: &TestCaseRecord) -> u32 {
    case.origin
        .get("steps")
        .and_then(Value::as_u64)
        .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Where practice runs' folders live.
pub(super) fn trials_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("trials")
}

/// Where test cases' files live.
pub(super) fn cases_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cases")
}

/// The network guard mode for a case's network setting.
pub(super) fn trial_mode(network: TrialNetwork) -> TrialMode {
    match network {
        TrialNetwork::Strict => TrialMode::Strict,
        TrialNetwork::Xhr => TrialMode::XhrAllowed,
    }
}

fn net_mode(mode: TrialMode) -> oxbot_browser::NetMode {
    match mode {
        TrialMode::Strict => oxbot_browser::NetMode::Strict,
        TrialMode::XhrAllowed => oxbot_browser::NetMode::XhrAllowed,
    }
}

/// Who serves every run's model: both engines run on `OpenRouter`.
pub(super) const PROVIDER: &str = "openrouter";

impl Hub {
    /// Looks up a bot or a practice run's synthetic bot, for the engine
    /// plumbing only (everything people see uses [`Hub::bot`]).
    ///
    /// # Errors
    /// [`HubError::UnknownBot`] if there's neither.
    pub(super) fn run_bot(&self, id: &str) -> Result<Bot, HubError> {
        if let Ok(key) = BotId::new(id)
            && is_trial(&key)
        {
            return lock(&self.trials.active)
                .get(&key)
                .map(|a| a.bot.clone())
                .ok_or_else(|| HubError::UnknownBot(id.to_owned()));
        }
        self.bot(id)
    }

    /// A practice run's own Chrome.
    pub(super) fn trial_browser(&self, key: &BotId) -> Option<Arc<Shared>> {
        lock(&self.trials.active)
            .get(key)
            .and_then(|a| a.shared.clone())
    }

    /// Stores what a practice run did in its own history and tells the
    /// owner; counts its steps, refusals and errors. Its turn ending wakes
    /// the run.
    pub(super) fn record_trial(&self, key: &BotId, kind: EventKind) -> Result<BotEvent, HubError> {
        let mut interrupt = false;
        let run = {
            let mut active = lock(&self.trials.active);
            let Some(a) = active.get_mut(key) else {
                // The run ended already: nothing to keep it with.
                return Ok(BotEvent {
                    bot: key.to_string(),
                    seq: 0,
                    ts_ms: now_ms(),
                    kind,
                });
            };
            match &kind {
                EventKind::ToolUse { .. } => {
                    a.run.steps += 1;
                    if a.run.steps > a.step_cap && !a.over_budget {
                        a.over_budget = true;
                        interrupt = true;
                    }
                }
                EventKind::AssistantMessage { text } => {
                    a.run.answer.clone_from(text);
                }
                EventKind::Blocked { .. } => a.run.refusals += 1,
                EventKind::Error { .. } => a.run.errors += 1,
                EventKind::TurnDone { ok, .. } => {
                    if let Some(tx) = a.ended.take() {
                        let _ = tx.send(*ok);
                    }
                }
                _ => {}
            }
            a.run.id.clone()
        };
        let event = self.store.add_trial_event(&run, kind)?;
        let _ = self.updates.send(ServerMsg::TrialEvent {
            run,
            event: event.clone(),
        });
        if interrupt {
            tracing::info!(%key, "practice run hit its step cap; stopping it");
            if let Some(hub) = self.me.upgrade() {
                let key = key.clone();
                tokio::spawn(async move { hub.interrupt_trial(&key).await });
            }
        }
        Ok(event)
    }

    /// One model request of a practice run: onto its run.
    pub(super) fn trial_model_call(&self, key: &BotId, record: &CallRecord) {
        if let Some(a) = lock(&self.trials.active).get_mut(key) {
            a.run.requests += 1;
            a.run.input_tokens += record.prompt_tokens;
            a.run.output_tokens += record.completion_tokens;
            a.run.cached_tokens += record.cached_tokens;
            a.run.cost_usd += record.cost_usd;
            a.limited = matches!(
                record.outcome.as_str(),
                "rate_limited" | "busy" | "daily_limit"
            );
        }
    }

    /// Stops a practice run's turn.
    async fn interrupt_trial(&self, key: &BotId) {
        let mut runtimes = self.runtimes.lock().await;
        if let Some(r) = runtimes.get_mut(key).filter(|r| r.busy)
            && let Err(err) = r.session.interrupt().await
        {
            tracing::warn!(%key, %err, "couldn't stop a practice run");
        }
    }

    /// The gate for a practice run: the live verdict through
    /// [`policy::trial_overlay`]. Never shows a card and never uses an
    /// "Always allow" rule; a recorded step is told to the model as done.
    pub(super) fn trial_decision(
        &self,
        bot: &Bot,
        request: &PermissionRequest,
        verdict: &policy::Verdict,
        call: TrialCall<'_>,
    ) -> Result<Decision, HubError> {
        let mode = lock(&self.trials.active)
            .get(&bot.id)
            .map_or(TrialMode::Strict, |a| a.mode);
        match policy::trial_overlay(verdict, call, mode) {
            TrialVerdict::Run => Ok(Decision::Allow),
            TrialVerdict::Refuse(why) => {
                self.record(
                    &bot.id,
                    EventKind::Blocked {
                        message: format!("oxbot refused: {why}"),
                    },
                )?;
                Ok(Decision::Deny(why))
            }
            TrialVerdict::Record(mut intent) => {
                if let policy::Verdict::Ask(ask) = verdict {
                    // Noted, never used: a rule of the real bot would have
                    // let it run live.
                    let source = lock(&self.trials.active)
                        .get(&bot.id)
                        .and_then(|a| BotId::new(&a.run.bot).ok());
                    if let Some(id) = source {
                        let live = Bot { id, ..bot.clone() };
                        intent.by_rule = self.matching_rule(&live, ask).ok().flatten().is_some();
                    }
                }
                tracing::info!(bot = %bot.id, tool = request.tool, "practice run: recorded a step");
                Ok(Decision::Simulate(self.note_intent(&bot.id, intent)?))
            }
        }
    }

    /// [`Hub::trial_decision`] for a browser or file tool on `page_url`,
    /// clicking `label` (with what oxbot read about it, `element`).
    pub(super) fn trial_gate(
        &self,
        bot: &Bot,
        request: &PermissionRequest,
        verdict: &policy::Verdict,
        page_url: Option<&str>,
        label: Option<&str>,
        element: Option<&oxbot_browser::ElementFacts>,
    ) -> Result<Decision, HubError> {
        let action = request
            .tool
            .strip_prefix(&format!("mcp__{}__", oxbot_browser::MCP_SERVER_NAME));
        let site = page_url
            .and_then(oxbot_browser::web_host)
            .unwrap_or_else(|| "this site".to_owned());
        let call = TrialCall {
            action,
            label,
            element,
            site: &site,
        };
        self.trial_decision(bot, request, verdict, call)
    }

    /// Keeps a step a practice run recorded, and returns what the model is
    /// told.
    fn note_intent(&self, key: &BotId, intent: Intent) -> Result<String, HubError> {
        let text = policy::dry_run_text(&intent);
        self.record(
            key,
            EventKind::DryRun {
                action: intent.action.clone(),
                target: intent.target.clone(),
                site: intent.site.clone(),
                click: intent.click.map(|c| c.as_str().to_owned()),
            },
        )?;
        if let Some(a) = lock(&self.trials.active).get_mut(key) {
            a.run.intents += 1;
            a.intents.push(intent);
        }
        Ok(text)
    }

    /// The refusal for a practice run's browser tool `tool` while its
    /// browser's network guard isn't on in every tab (waiting a little for
    /// it), or without a browser of its own. `None`: it may browse.
    pub(super) async fn trial_browser_refusal(&self, key: &BotId, tool: &str) -> Option<Decision> {
        if !tool.starts_with(&format!("mcp__{}__", oxbot_browser::MCP_SERVER_NAME)) {
            return None;
        }
        #[cfg(any(test, feature = "test-people"))]
        if self.config.trials_without_guard {
            return None;
        }
        let ready = match self.trial_browser(key) {
            Some(shared) => shared.browser.net_ready(GUARD_WAIT).await,
            None => false,
        };
        (!ready).then(|| {
            Decision::Deny(
                "The test browser isn't ready, so oxbot can't browse in this test run.".to_owned(),
            )
        })
    }

    /// One of oxbot's own tools in a practice run: answered from what the
    /// original turn got, or recorded; never done for real. Both engines
    /// get a normal result.
    pub(super) async fn trial_host_tool(self: Arc<Self>, key: BotId, call: HostToolCall) {
        let HostToolCall { name, args, reply } = call;
        let name = name
            .strip_prefix(&format!("mcp__{}__", tools::TOOL_SERVER_NAME))
            .map_or(name.clone(), str::to_owned);
        let set = tools::ToolSet {
            central: self.run_bot(key.as_str()).is_ok_and(|b| b.is_central()),
            browser: self.trial_browser(&key).is_some(),
        };
        let (ok, text) = match tools::check_call(&name, &args, set) {
            Ok(()) => self.trial_tool(&key, &name, &args).await,
            Err(message) => (false, message),
        };
        reply.send(ToolReply { ok, text });
    }

    async fn trial_tool(self: &Arc<Self>, key: &BotId, name: &str, args: &Value) -> (bool, String) {
        let text = |k: &str| tools::text_arg(args, k).trim().to_owned();
        let record = |kind, action: &str, target: String| {
            let intent = Intent {
                kind,
                action: action.to_owned(),
                target,
                site: "oxbot".to_owned(),
                click: None,
                by_rule: false,
            };
            self.note_intent(key, intent)
                .map_or_else(|e| (false, e.to_string()), |t| (true, t))
        };
        match name {
            tools::SIGN_IN_TOOL => {
                if let Some(a) = lock(&self.trials.active).get_mut(key) {
                    a.asked_to_sign_in = true;
                }
                (false, NO_SIGN_IN.to_owned())
            }
            tools::CREATE_BOT_TOOL => record(
                ApprovalKind::CreateBot,
                "create a bot",
                format!("\"{}\"", text("name")),
            ),
            tools::GIVE_TASK_TOOL => {
                let fixture = lock(&self.trials.active).get_mut(key).and_then(|a| {
                    (!a.fixtures.give_task.is_empty()).then(|| a.fixtures.give_task.remove(0))
                });
                let (ok, recorded) = record(
                    ApprovalKind::GiveTask,
                    "give a task to",
                    format!("\"{}\"", text("bot")),
                );
                match fixture {
                    Some(answer) if ok => (true, answer),
                    _ => (ok, recorded),
                }
            }
            tools::READ_REPLY_TOOL => {
                let fixture = lock(&self.trials.active).get_mut(key).and_then(|a| {
                    (!a.fixtures.read_reply.is_empty()).then(|| a.fixtures.read_reply.remove(0))
                });
                (
                    true,
                    fixture.unwrap_or_else(|| "No reply in a test run.".to_owned()),
                )
            }
            tools::SAVE_REPORT_TOOL => self.trial_report(key, args).await,
            // Reading the team changes nothing: as live.
            tools::LIST_BOTS_TOOL => {
                Arc::clone(self)
                    .run_tool(key.clone(), name.to_owned(), args.clone())
                    .await
            }
            _ => (false, "unknown tool".to_owned()),
        }
    }

    /// `save_report` in a practice run: checked as live (its sources must
    /// be pages the run opened), kept with the run, never in the bot's
    /// reports.
    async fn trial_report(&self, key: &BotId, args: &Value) -> (bool, String) {
        let checked = {
            let runtimes = self.runtimes.lock().await;
            match runtimes.get(key) {
                Some(r) => crate::report::check(args, &r.visited),
                None => {
                    return (
                        false,
                        "Not saved: oxbot lost track of this turn.".to_owned(),
                    );
                }
            }
        };
        let draft = match checked {
            Ok(draft) => draft,
            Err(why) => return (false, why),
        };
        let report = json!({
            "title": draft.title,
            "summary": draft.summary,
            "markdown": draft.markdown,
            "sources": draft.sources.iter().map(|s| s.url.clone()).collect::<Vec<_>>(),
        });
        let title = draft.title.clone();
        let _ = self.record(
            key,
            EventKind::Report {
                id: String::new(),
                title: draft.title,
                summary: draft.summary,
                sources: u32::try_from(draft.sources.len()).unwrap_or(0),
            },
        );
        if let Some(a) = lock(&self.trials.active).get_mut(key) {
            a.report = Some(report);
        }
        (
            true,
            format!(
                "Saved \"{title}\". The user sees it in this chat. Now reply in 2 to 4 \
                 lines with the answer in brief; don't repeat the report."
            ),
        )
    }
}

/// The queue and its one worker.
impl Hub {
    /// At start: runs an earlier oxbot left going are stopped and their
    /// folders deleted; then the worker waits for jobs.
    pub(super) fn start_trials(self: &Arc<Self>) {
        match self.store.stop_unfinished_runs() {
            Ok(0) => {}
            Ok(n) => tracing::info!(runs = n, "stopped practice runs an earlier oxbot left"),
            Err(err) => tracing::warn!(%err, "couldn't stop old practice runs"),
        }
        let root = trials_dir(&self.config.data_dir);
        self.sweep_case_files();
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            oxbot_browser::trial::sweep(&root).await;
            run_queue(weak).await;
        });
    }

    /// Deletes the files of test cases that are gone (their bot was
    /// deleted, say).
    fn sweep_case_files(&self) {
        let Ok(entries) = std::fs::read_dir(cases_dir(&self.config.data_dir)) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches!(self.store.test_case(&name), Ok(None))
                && let Err(err) = std::fs::remove_dir_all(entry.path())
            {
                tracing::warn!(%err, "couldn't delete a deleted test case's files");
            }
        }
    }

    /// Queues `job`; shadows go first.
    pub(super) fn queue_trial(&self, job: Job) {
        {
            let mut queue = lock(&self.trials.queue);
            match job {
                Job::Shadow(_) => queue.push_front(job),
                Job::Batch(_) => queue.push_back(job),
            }
        }
        self.trials.wake.notify_one();
    }

    /// Stops batch `id`: its queued runs never start, and the one going on
    /// stops.
    pub(super) async fn stop_trials_of(&self, id: &str) {
        lock(&self.trials.stopped).insert(id.to_owned());
        let keys: Vec<BotId> = lock(&self.trials.active)
            .iter()
            .filter(|(_, a)| a.run.batch.as_deref() == Some(id))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(a) = lock(&self.trials.active).get_mut(&key) {
                "stopped".clone_into(&mut a.run.status);
            }
            self.interrupt_trial(&key).await;
        }
    }

    fn batch_stopped(&self, id: &str) -> bool {
        lock(&self.trials.stopped).contains(id)
    }

    /// Runs every queued run of batch `id`, one after another.
    async fn run_batch(self: &Arc<Self>, id: &str) {
        let Ok(Some(batch)) = self.store.batch(id) else {
            return;
        };
        if self.batch_stopped(id) {
            return;
        }
        let _ = self.store.set_batch_status(id, "running");
        let runs = self.store.batch_runs(id).unwrap_or_default();
        let total = u32::try_from(runs.len()).unwrap_or(0);
        let mut done = 0;
        self.announce_batch(&batch.bot, id, done, total, "running", None);
        for mut run in runs {
            if run.status != "queued" {
                done += 1;
                continue;
            }
            if self.batch_stopped(id) {
                "stopped".clone_into(&mut run.status);
                let _ = self.store.save_run(&run);
                self.announce_run(&run);
                done += 1;
                continue;
            }
            // Queued before Claude left oxbot: it can't run.
            let Some(engine) = compare::engine_of(&run.engine) else {
                "stopped".clone_into(&mut run.status);
                "Not run: Claude is no longer one of oxbot's AI models."
                    .clone_into(&mut run.answer);
                let _ = self.store.save_run(&run);
                done += 1;
                continue;
            };
            // The rest of today's free requests are kept for live bots.
            if let (Some(left), reserve) = self.openrouter_room()
                && left <= reserve
            {
                "rate_limited".clone_into(&mut run.status);
                "Not run: the rest of today's free AI questions are kept for live bots."
                    .clone_into(&mut run.answer);
                let _ = self.store.save_run(&run);
                self.announce_run(&run);
                done += 1;
                continue;
            }
            let case = run
                .case_id
                .as_deref()
                .and_then(|c| self.store.test_case(c).ok().flatten());
            if let Some(case) = case {
                let input = RunInput::of_case(&case, &cases_dir(&self.config.data_dir));
                let target = Target {
                    engine,
                    model: run.model.clone(),
                };
                self.run_one(&case.bot, &mut run, &target, input).await;
            } else {
                "failed".clone_into(&mut run.status);
                "The test case was deleted.".clone_into(&mut run.answer);
                let _ = self.store.save_run(&run);
                self.announce_run(&run);
            }
            done += 1;
            self.announce_batch(&batch.bot, id, done, total, "running", None);
        }
        let status = if self.batch_stopped(id) {
            "stopped"
        } else {
            "done"
        };
        let _ = self.store.set_batch_status(id, status);
        lock(&self.trials.stopped).remove(id);
        self.announce_batch(&batch.bot, id, done, total, status, None);
    }

    pub(super) fn announce_batch(
        &self,
        bot: &str,
        id: &str,
        done: u32,
        total: u32,
        status: &str,
        paused_reason: Option<String>,
    ) {
        let _ = self.updates.send(ServerMsg::BatchProgress {
            batch: id.to_owned(),
            bot: bot.to_owned(),
            done,
            total,
            status: status.to_owned(),
            paused_reason,
        });
    }

    pub(super) fn announce_run(&self, run: &TrialRunRecord) {
        let title = run
            .case_id
            .as_deref()
            .and_then(|c| self.store.test_case(c).ok().flatten())
            .map(|c| c.title)
            .unwrap_or_default();
        if let Some(info) = compare::run_info(run, &title) {
            let _ = self.updates.send(ServerMsg::TrialRun(Box::new(info)));
        }
    }

    /// Runs one practice run of `source`'s case on `target`, start to end,
    /// and saves it.
    pub(super) async fn run_one(
        self: &Arc<Self>,
        source: &str,
        run: &mut TrialRunRecord,
        target: &Target,
        input: RunInput,
    ) {
        let _one = self.trials.one.lock().await;
        "running".clone_into(&mut run.status);
        run.started_ms = Some(now_ms());
        let _ = self.store.save_run(run);
        self.announce_run(run);
        let key = BotId::new(format!(
            "{TRIAL_PREFIX}{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        ))
        .unwrap_or_else(|_| unreachable!("a t-- key of hex digits is a valid id"));
        let dir = trials_dir(&self.config.data_dir).join(&run.id);
        let outcome = self.drive(source, run, target, input, &key, &dir).await;
        // Whatever happened: nothing of the run stays but its row.
        self.stop_session(&key).await;
        let active = lock(&self.trials.active).remove(&key);
        if let Some(shared) = active.as_ref().and_then(|a| a.shared.clone()) {
            shared.browser.shutdown().await;
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
        if let Err(err) = self.store.forget_trial_key(&key) {
            tracing::warn!(%key, %err, "couldn't clear a practice run's key");
        }
        if let Some(a) = active {
            // The run's numbers as the session counted them.
            let status = run.status.clone();
            *run = TrialRunRecord { status, ..a.run };
            run.intent_list = serde_json::to_value(&a.intents).unwrap_or(Value::Null);
            run.report = a.report;
            if a.asked_to_sign_in && run.status == "done" {
                "needs_sign_in".clone_into(&mut run.status);
            }
            if a.over_budget && run.status != "stopped" {
                "over_budget".clone_into(&mut run.status);
            }
        }
        if let Err(why) = outcome {
            if run.status == "running" || run.status == "done" {
                "failed".clone_into(&mut run.status);
            }
            if run.answer.is_empty() {
                run.answer = why;
            }
        }
        run.ended_ms = Some(now_ms());
        let _ = self.store.save_run(run);
        self.announce_run(run);
    }

    /// Sets the run up, sends the task, and waits for it to end.
    async fn drive(
        self: &Arc<Self>,
        source: &str,
        run: &mut TrialRunRecord,
        target: &Target,
        input: RunInput,
        key: &BotId,
        dir: &Path,
    ) -> Result<(), String> {
        let real = self
            .bot(source)
            .map_err(|_| "The bot is gone.".to_owned())?;
        let kind = from_wire(target.engine);
        let engine_ready = self.engine(kind).map_err(|e| e.to_string());
        engine_ready?;
        // Its folder, with the case's files where the original had them.
        let work = dir.join("work");
        tokio::fs::create_dir_all(&work)
            .await
            .map_err(|e| format!("couldn't make the run's folder: {e}"))?;
        if let Some(from) = &input.files_dir {
            copy_files(from, &work, &input.files).await?;
        }
        let mut bot = real.clone();
        bot.id = key.clone();
        bot.dir = dir.to_owned();
        bot.instructions.clone_from(&input.instructions);
        let shared = self.trial_shared(&real, dir, &input).await?;
        let (ended, ended_rx) = oneshot::channel();
        lock(&self.trials.active).insert(
            key.clone(),
            Active {
                run: run.clone(),
                bot: bot.clone(),
                shared,
                mode: input.mode,
                intents: Vec::new(),
                fixtures: input.fixtures.clone(),
                step_cap: input.step_cap,
                over_budget: false,
                asked_to_sign_in: false,
                limited: false,
                report: None,
                ended: Some(ended),
            },
        );
        let overrides = SessionOverrides {
            engine: Some(kind),
            model: Some(ModelChoice::Only(target.model.clone())),
            gate_mode: GateMode::DryRun,
            ephemeral: true,
            no_browser: self.trial_browser(key).is_none(),
            ..SessionOverrides::default()
        };
        let runtime = self
            .start_session_with(&bot, overrides)
            .await
            .map_err(|e| e.to_string())?;
        self.runtimes.lock().await.insert(key.clone(), runtime);
        self.send_trial_turn(key, &input, &work).await?;
        let ok = match tokio::time::timeout(self.config.task_timeout, ended_rx).await {
            Ok(Ok(ok)) => ok,
            Ok(Err(_)) => false,
            Err(_) => {
                self.interrupt_trial(key).await;
                if let Some(a) = lock(&self.trials.active).get_mut(key) {
                    a.run.errors += 1;
                }
                return Err("The run took longer than the task time limit.".to_owned());
            }
        };
        self.finish_trial(key, ok, &input, run).await;
        Ok(())
    }

    /// Sends the case's task to a practice run's session, as the owner
    /// sends it.
    async fn send_trial_turn(
        &self,
        key: &BotId,
        input: &RunInput,
        work: &Path,
    ) -> Result<(), String> {
        let permit = Arc::clone(&self.turns)
            .acquire_owned()
            .await
            .map_err(|_| "oxbot is stopping".to_owned())?;

        // The task, as the owner sends it.
        let owner = self.owner_name();
        let user_message = self
            .record(
                key,
                EventKind::UserMessage {
                    text: input.prompt.clone(),
                    from: Some(owner.clone()),
                    files: input.files.clone(),
                },
            )
            .map_err(|e| e.to_string())?;
        let origin = ChainOrigin {
            person: self.farm_id(),
            on_farm: true,
            entry: Entry::Farm,
            root: None,
            via: Vec::new(),
            tainted_by: input.tainted_by.clone(),
        };
        if let Some(why) = &input.tainted_by {
            self.mark_untrusted(key, format!("a task passed on after reading {why}"))
                .await;
        }
        let mut message = sender_line(Some(&owner), &input.prompt);
        if !input.files.is_empty() {
            if !input.prompt.is_empty() {
                message.push_str("\n\n");
            }
            message.push_str(&crate::attach::note(Some(&owner), &input.files, work));
        }
        let turn = TurnInput {
            text: message,
            user_seq: user_message.seq,
            person: origin.asker().map(ToString::to_string),
            origin: serde_json::to_value(&origin).unwrap_or_default(),
        };
        {
            let mut runtimes = self.runtimes.lock().await;
            let runtime = runtimes
                .get_mut(key)
                .ok_or_else(|| "the run's session stopped".to_owned())?;
            runtime
                .session
                .send(turn)
                .await
                .map_err(|e| e.to_string())?;
            runtime.busy = true;
            runtime.permit = Some(permit);
            runtime.last_active = Instant::now();
            runtime.origin = origin;
            runtime.question = Some(
                input
                    .prompt
                    .chars()
                    .take(crate::report::MAX_QUESTION)
                    .collect(),
            );
        }

        Ok(())
    }

    /// After a practice run's turn ended (`ok`: without error): what its
    /// network guard stopped, its status, and how its steps matched.
    async fn finish_trial(
        &self,
        key: &BotId,
        ok: bool,
        input: &RunInput,
        run: &mut TrialRunRecord,
    ) {
        // What the network guard stopped, into the run's transcript.
        if let Some(shared) = self.trial_browser(key) {
            let (blocked, count) = shared.browser.blocked_requests().await;
            for b in blocked.iter().take(50) {
                let _ = self.record(
                    key,
                    EventKind::NetBlocked {
                        method: b.method.clone(),
                        host: b.host.clone(),
                    },
                );
            }
            if let Some(a) = lock(&self.trials.active).get_mut(key) {
                a.run.net_blocked = u32::try_from(count).unwrap_or(u32::MAX);
            }
        }
        let mut active = lock(&self.trials.active);
        if let Some(a) = active.get_mut(key) {
            let stopped = a.run.status == "stopped";
            // Ended on a limit: inconclusive, not a fail.
            let limited = !ok && a.limited;
            let status = if stopped {
                "stopped"
            } else if limited {
                "rate_limited"
            } else if ok {
                "done"
            } else {
                "failed"
            };
            status.clone_into(&mut a.run.status);
            let recorded = a.intents.clone();
            a.run.intent_match = Some(
                serde_json::to_value(compare::intent_match(&input.expected, &recorded))
                    .unwrap_or(Value::Null),
            );
            a.run.web_age_days = input
                .original_ms
                .map(|then| (now_ms() - then).max(0) / 86_400_000);
            run.status.clone_from(&a.run.status);
        }
    }

    /// The run's own Chrome: a copy of the source browser's profile without
    /// history (when the case needs its sign-ins) or an empty one, behind
    /// the network guard. `None` when this oxbot has no browser.
    async fn trial_shared(
        &self,
        real: &Bot,
        dir: &Path,
        input: &RunInput,
    ) -> Result<Option<Arc<Shared>>, String> {
        let Some(tools) = self.config.browser.clone() else {
            return Ok(None);
        };
        let owner = real.browser_owner().unwrap_or(BrowserOwner::Home);
        let profile = dir.join("profile");
        if input.uses_sign_ins {
            let source = self
                .browser_for(&owner)
                .ok_or_else(|| "the bot's browser isn't available".to_owned())?;
            oxbot_browser::trial::clone_profile(source.browser.profile_dir(), &profile)
                .await
                .map_err(|e| e.to_string())?;
        } else {
            oxbot_browser::trial::clone_profile(&dir.join("no-profile"), &profile)
                .await
                .map_err(|e| e.to_string())?;
        }
        let spec = oxbot_browser::BrowserSpec {
            label: "practice run".to_owned(),
            profile_dir: profile,
            downloads: dir.join("work").join("downloads"),
        };
        let browser = oxbot_browser::Browser::new_trial(tools, spec, net_mode(input.mode))
            .map_err(|e| format!("no local port for the run's Chrome: {e}"))?;
        Ok(Some(Arc::new(Shared {
            owner,
            browser,
            lease: Arc::new(Mutex::new(())),
            holder: std::sync::Mutex::new(None),
            last: std::sync::Mutex::new(None),
            last_used: std::sync::Mutex::new(Instant::now()),
            adding_site: std::sync::Mutex::new(None),
            // Its sign-ins are a copy: never checked against the real list.
            sites_checked: std::sync::atomic::AtomicBool::new(true),
        })))
    }
}

/// Copies a case's files into a run's folder, where the original had them.
async fn copy_files(from: &Path, work: &Path, files: &[Attachment]) -> Result<(), String> {
    let (from, work, files) = (from.to_owned(), work.to_owned(), files.to_vec());
    tokio::task::spawn_blocking(move || {
        for f in &files {
            for rel in std::iter::once(&f.path).chain(f.readable.as_ref()) {
                if !safe_relative(rel) {
                    continue;
                }
                let src = from.join(rel);
                let dst = work.join(rel);
                copy_tree(&src, &dst).map_err(|e| format!("couldn't copy {}: {e}", f.name))?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether `rel` stays inside the folder it's joined to.
pub(super) fn safe_relative(rel: &str) -> bool {
    let path = Path::new(rel);
    !rel.is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Copies a file, or a folder with what's in it (no links followed).
pub(super) fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// The worker: one job at a time, until the hub goes.
async fn run_queue(hub: Weak<Hub>) {
    loop {
        let Some(strong) = hub.upgrade() else { return };
        let job = lock(&strong.trials.queue).pop_front();
        let Some(job) = job else {
            // Woken by a new job, or once a minute to see the hub is still there.
            let woken = {
                let trials = &strong.trials;
                tokio::time::timeout(Duration::from_mins(1), trials.wake.notified()).await
            };
            drop(woken);
            continue;
        };
        let hub = strong;
        match job {
            Job::Batch(id) => hub.run_batch(&id).await,
            Job::Shadow(shadow) => {
                // A shadow runs next to the live turn, not after the batch.
                let hub = Arc::clone(&hub);
                tokio::spawn(async move { hub.run_shadow(shadow).await });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_keys_are_their_own_shape() {
        assert!(is_trial(&BotId::new("t--0123456789abcdef").unwrap()));
        assert!(!is_trial(&BotId::new("t-shirts").unwrap()));
        assert!(!is_trial(&BotId::new("assistant--sam-0a1b2c").unwrap()));
        for name in ["t--x", "T -- x", "t  --  x"] {
            let id = BotId::from_name(name, |_| false);
            assert!(!id.as_str().contains("--"), "{id}");
        }
    }

    #[test]
    fn case_paths_stay_inside() {
        assert!(safe_relative("files/12/cv.pdf"));
        for bad in ["", "/etc/passwd", "../x", "files/../../x", "./x"] {
            assert!(!safe_relative(bad), "{bad}");
        }
    }
}
