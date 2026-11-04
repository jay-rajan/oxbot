//! One session's tools, as every engine that runs oxbot's tools uses them:
//! the loop's own turns, and Codex's dynamic tools. There is one way from
//! "the model asked for a tool" to "it ran", here:
//!
//! 1. the call is checked: a known tool, arguments it can use;
//! 2. the gate is asked ([`crate::gate`]): the hub's verdict, cards and
//!    Always allow; a practice run records instead;
//! 3. it runs: oxbot's own tools in the hub, the browser, files in
//!    `work/`, stored results read back and the page tools, with the
//!    results wrapped and stored as [`crate::tools`] does it.
//!
//! One call runs at a time per session, in the order asked (Codex may ask
//! for several at once): the page a gate verdict was read from is the page
//! the call acts on, and the browser's tabs stay in step with what was
//! shown. What only an engine knows (the loop's to-do list and progress
//! notes, its write-ahead of calls that change something, which results
//! the model still sees) comes through [`TurnHooks`].
//!
//! Two guards keep a model from going round in circles: a read-only tool
//! asked the same thing twice in a turn gets a pointer to the first answer
//! ([`RepeatGuard`]), and a call that failed the same way three times gets
//! a note to try something else ([`Failures`]).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use oxbot_engine::{Activity, EngineEvent, ModelChoice, Privacy, SessionSpec};
use oxbot_llm::{Part, ToolCall, ToolSpec};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Shared;
use crate::budget::TokenBudget;
use crate::context::state::{EntryKind, EntryMeta};
use crate::gate::{GateCheck, Gated, Permit};
use crate::pacer::Lane;
use crate::results::ResultStore;
use crate::runner::{HelperCaller, Stop};
use crate::tools::browser::{BrowserTools, model_specs};
use crate::tools::control::{ASK_USER, READ_RESULT};
use crate::tools::page::{self, PageEnv};
use crate::tools::pages::PageTracker;
use crate::tools::{ToolEnv, ToolKind, ToolOutcome, ToolRegistry};

/// The same call failing this often gets a nudge.
const SAME_FAILURES: u32 = 3;
/// The nudge.
const TRY_ELSE: &str = "This failed 3 times the same way; try something else or ask the user.";

/// What one call came to.
#[derive(Debug)]
pub enum Dispatched {
    /// It ran (or a practice run recorded it): its result.
    Ran(ToolOutcome),
    /// Not a tool, or arguments it can't use: what to tell the model.
    Invalid(ToolOutcome),
    /// oxbot or the person said no: its result. The rest of the step
    /// shouldn't run.
    Refused(ToolOutcome),
    /// The model asked the person this (`ask_user`): the turn waits.
    Asked(String),
}

/// Where a call stands: its turn and step, and who the turn acts for.
#[derive(Debug, Clone, Copy)]
pub struct CallCtx<'a> {
    /// The turn.
    pub turn: u32,
    /// The step in it.
    pub step: u32,
    /// The window's budget: results are cut to it.
    pub budget: TokenBudget,
    /// The person the turn acts for (helper requests and the meter).
    pub person: Option<&'a str>,
    /// The pacer queue helper requests wait in.
    pub lane: Lane,
    /// Stops the call.
    pub cancel: &'a CancellationToken,
}

/// What only the engine running the turn knows.
pub trait TurnHooks: Send {
    /// Runs one of the tools whose state the engine keeps (`update_todo`,
    /// `note_progress`); the gate already said yes.
    fn own_tool(&mut self, call: &ToolCall, turn: u32) -> ToolOutcome;

    /// A call that changes something outside oxbot is about to run (the
    /// loop writes it down first, so a restart doesn't run it again).
    ///
    /// # Errors
    /// The turn has to stop.
    fn before_side_effect(&mut self, call: &ToolCall) -> Result<(), Stop>;

    /// Stored results the model still sees in full (a page diff is shown
    /// only against one of these).
    fn visible_handles(&self) -> BTreeSet<String>;

    /// Where the next result is kept, for the repeat guard (the loop: its
    /// message number).
    fn next_mark(&self) -> u32;

    /// Whether what was kept at `mark` is still in view.
    fn in_view(&self, mark: u32) -> bool;

    /// The turn's repeat guard.
    fn repeats(&mut self) -> &mut RepeatGuard;
}

/// Read-only calls already answered this turn.
#[derive(Debug, Default, Clone)]
pub struct RepeatGuard {
    /// key → (step, mark, first and last line shown).
    seen: HashMap<String, (u32, u32, usize, usize)>,
}

impl RepeatGuard {
    /// Forgets everything (a new turn, or the model's view was rebuilt).
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

/// Calls that failed this turn, by call.
#[derive(Debug, Default, Clone)]
pub struct Failures {
    counts: HashMap<String, u32>,
}

impl Failures {
    /// Counts a failed `call`; the third time it failed the same way, the
    /// model is told to try something else (added to `outcome`).
    pub fn count(&mut self, call: &ToolCall, outcome: &mut ToolOutcome) {
        let key = call_key(&call.name, &call.arguments);
        let n = self.counts.entry(key).or_insert(0);
        *n += 1;
        if *n >= SAME_FAILURES
            && let Some(Part::Text(t)) = outcome.for_model.last_mut()
        {
            t.push('\n');
            t.push_str(TRY_ELSE);
        }
    }
}

/// Which of the loop's own tools a session offers besides the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnTools {
    /// All of them (the loop).
    All,
    /// All but `note_progress`, whose notes only the loop shows again
    /// after a conversation starts over.
    NoProgressNotes,
}

/// One session's tools.
pub struct ToolService {
    shared: Arc<Shared>,
    bot: String,
    session: String,
    work_dir: PathBuf,
    tools: ToolRegistry,
    gate: GateCheck,
    events: mpsc::Sender<EngineEvent>,
    browser: Option<Arc<dyn BrowserTools>>,
    pages: Mutex<PageTracker>,
    results: ResultStore,
    privacy: Privacy,
    choice: ModelChoice,
    /// One call at a time, in the order asked.
    queue: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for ToolService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolService")
            .field("bot", &self.bot)
            .field("session", &self.session)
            .field("tools", &self.tools.specs().len())
            .finish_non_exhaustive()
    }
}

impl ToolService {
    /// The tools for `spec`'s session, asking the gate and sending results
    /// through `events`. Opens the browser tool if the session has one.
    pub(crate) async fn open(
        shared: Arc<Shared>,
        spec: &SessionSpec,
        events: mpsc::Sender<EngineEvent>,
        own: OwnTools,
    ) -> Self {
        let (browser, browser_specs) = open_browser(&shared, spec).await;
        let host = spec
            .host_tool_specs
            .iter()
            .map(|t| ToolSpec {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect();
        let mut tools = ToolRegistry::new(host, browser_specs);
        if own == OwnTools::NoProgressNotes {
            tools = tools.without(&[crate::tools::control::NOTE_PROGRESS]);
        }
        let id = spec.resume.id().to_owned();
        Self {
            results: ResultStore::new(
                Arc::clone(&shared.setup.store),
                &spec.bot,
                &id,
                &spec.context_dir,
            ),
            bot: spec.bot.clone(),
            session: id,
            work_dir: spec.work_dir.clone(),
            tools,
            gate: GateCheck::new(events.clone()),
            events,
            browser,
            pages: Mutex::new(PageTracker::default()),
            privacy: spec.privacy,
            choice: spec.model.clone(),
            queue: tokio::sync::Mutex::new(()),
            shared,
        }
    }

    /// The tools as the model sees them, in order.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.specs()
    }

    /// The session's tools.
    #[must_use]
    pub fn registry(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Whether the session has a browser.
    #[must_use]
    pub fn has_browser(&self) -> bool {
        self.browser.is_some()
    }

    /// Stored results.
    #[must_use]
    pub fn results(&self) -> &ResultStore {
        &self.results
    }

    /// The bot's `work/` folder.
    #[must_use]
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// The selected tab's address, if known.
    #[must_use]
    pub fn current_url(&self) -> Option<String> {
        self.pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current()
            .map(|p| p.url)
            .filter(|u| !u.is_empty())
    }

    /// Stops the browser tool (its process).
    pub async fn shutdown(&self) {
        if let Some(browser) = &self.browser {
            browser.shutdown().await;
        }
    }

    /// The browser tool, if any (to stop it from a `Drop`).
    #[must_use]
    pub fn browser(&self) -> Option<Arc<dyn BrowserTools>> {
        self.browser.clone()
    }

    /// One call: checked, asked of the gate, run. Waits for the calls
    /// asked before it.
    ///
    /// # Errors
    /// The turn has to stop: it was stopped (`Stop::Cancelled`), or the
    /// engine's write-ahead failed.
    pub async fn dispatch(
        &self,
        call: &ToolCall,
        ctx: &CallCtx<'_>,
        hooks: &mut dyn TurnHooks,
    ) -> Result<Dispatched, Stop> {
        let _in_turn = tokio::select! {
            guard = self.queue.lock() => guard,
            () = ctx.cancel.cancelled() => return Err(Stop::Cancelled),
        };
        let entry = match self.tools.validate(call) {
            Ok(entry) => entry,
            Err(why) => {
                return Ok(Dispatched::Invalid(ToolOutcome::note(
                    format!("[tool error] {why}"),
                    true,
                )));
            }
        };
        let (kind, side_effect) = (entry.kind, entry.side_effect);
        let gated = self
            .gate
            .check(&call.name, &call.arguments, &call.id, ctx.cancel)
            .await
            .ok_or(Stop::Cancelled)?;
        let permit = match gated {
            Gated::Denied(why) => {
                return Ok(Dispatched::Refused(
                    ToolOutcome::note(format!("[oxbot said no] {why}"), true)
                        .kind(EntryKind::Refusal),
                ));
            }
            Gated::Simulated(text) => return Ok(Dispatched::Ran(ToolOutcome::note(text, false))),
            Gated::Run(permit) => permit,
        };
        if kind == ToolKind::Control && call.name == ASK_USER {
            let question = call
                .arguments
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return Ok(Dispatched::Asked(question));
        }
        let _ = self
            .events
            .send(EngineEvent::Activity(Activity::UsingTool(
                call.name.clone(),
            )))
            .await;
        if kind == ToolKind::Page || (kind == ToolKind::Control && call.name == READ_RESULT) {
            return Ok(Dispatched::Ran(
                self.read_only(call, kind, permit, ctx, hooks).await,
            ));
        }
        if kind == ToolKind::Control {
            debug_assert_eq!(permit.tool_use_id(), call.id);
            return Ok(Dispatched::Ran(hooks.own_tool(call, ctx.turn)));
        }
        if side_effect {
            hooks.before_side_effect(call)?;
        }
        let visible = hooks.visible_handles();
        let is_visible = |h: &str| visible.contains(h);
        let env = ToolEnv {
            work_dir: &self.work_dir,
            events: &self.events,
            browser: self.browser.as_ref(),
            cancel: ctx.cancel,
            results: &self.results,
            pages: &self.pages,
            strictness: Arc::clone(&self.shared.setup.strictness),
            budget: ctx.budget,
            turn: ctx.turn,
            step: ctx.step,
            visible: &is_visible,
        };
        Ok(Dispatched::Ran(self.tools.run(call, permit, &env).await))
    }

    /// A caller for helper requests within the turn (`ask_page`): the
    /// session's bot, privacy and model, the turn's person, lane and stop.
    fn helper<'a>(&'a self, ctx: &CallCtx<'a>) -> HelperCaller<'a> {
        HelperCaller {
            shared: &self.shared,
            bot: &self.bot,
            person: ctx.person,
            session: Some(&self.session),
            turn: Some(ctx.turn),
            step: ctx.step,
            privacy: self.privacy,
            lane: ctx.lane,
            choice: self.choice.clone(),
            cancel: ctx.cancel,
            meter: Some(&self.events),
        }
    }

    /// `read_result` and the page tools: read-only, so a repeat within the
    /// turn gets a pointer to the first answer instead.
    async fn read_only(
        &self,
        call: &ToolCall,
        kind: ToolKind,
        permit: Permit,
        ctx: &CallCtx<'_>,
        hooks: &mut dyn TurnHooks,
    ) -> ToolOutcome {
        debug_assert_eq!(permit.tool_use_id(), call.id);
        let args = self.with_default_page(call, kind);
        let (key, wanted) = repeat_key(&call.name, &args);
        // Only while the first answer is still there to see: not once it's
        // a stub or out of the window (its stub says to ask again).
        if let Some((step, ..)) =
            hooks
                .repeats()
                .seen
                .get(&key)
                .copied()
                .filter(|(_, mark, from, to)| {
                    hooks.in_view(*mark) && wanted.0 >= *from && wanted.1 <= *to
                })
        {
            let mut o = ToolOutcome::note(
                format!("You already did this at step {step}; the result is above."),
                false,
            );
            o.meta.tool = Some(call.name.clone());
            return o;
        }
        let mut meta = EntryMeta {
            tool: Some(call.name.clone()),
            ..EntryMeta::default()
        };
        let cap = ctx.budget.inline_cap();
        let outcome = if kind == ToolKind::Page {
            let helper = self.helper(ctx);
            let env = PageEnv {
                results: &self.results,
                pages: &self.pages,
                strictness: self.shared.setup.strictness.as_ref(),
                budget: ctx.budget,
                helper: &helper,
                turn: ctx.turn,
                step: ctx.step,
            };
            match page::run(&call.name, &args, &env).await {
                Ok(answer) => {
                    let why = if answer.url.is_empty() {
                        "a web page".to_owned()
                    } else {
                        format!("a web page ({})", answer.url)
                    };
                    meta.trust_why = Some(why);
                    meta.page_url = Some(answer.url.clone()).filter(|u| !u.is_empty());
                    ToolOutcome {
                        text_for_hub: answer.text.clone(),
                        for_model: vec![Part::Text(answer.text)],
                        untrusted: true,
                        // Cleared, it points back to the page it came from.
                        handle: Some(answer.handle),
                        error: false,
                        meta,
                    }
                }
                Err(why) => {
                    let mut o = ToolOutcome::note(format!("[tool error] {why}"), true);
                    o.meta.tool = Some(call.name.clone());
                    o
                }
            }
        } else {
            match crate::tools::control::read_result(&self.results, &args, cap) {
                Ok((text, tainted_by)) => {
                    let handle = args
                        .get("handle")
                        .and_then(Value::as_str)
                        .map(|h| h.trim().to_owned());
                    meta.trust_why.clone_from(&tainted_by);
                    ToolOutcome {
                        text_for_hub: text.clone(),
                        for_model: vec![Part::Text(text)],
                        // A stored result keeps its trust.
                        untrusted: tainted_by.is_some(),
                        // Never stored again: cleared, it points back.
                        handle,
                        error: false,
                        meta,
                    }
                }
                Err(why) => {
                    let mut o = ToolOutcome::note(format!("[tool error] {why}"), true);
                    o.meta.tool = Some(call.name.clone());
                    o
                }
            }
        };
        if !outcome.error {
            // Kept next, with the lines it showed (a section may stop
            // early).
            let shown_to = shown_to(&outcome.text_for_hub).unwrap_or(usize::MAX);
            let mark = hooks.next_mark();
            hooks
                .repeats()
                .seen
                .insert(key, (ctx.step, mark, wanted.0, shown_to));
        }
        outcome
    }

    /// A page tool's arguments with the latest page filled in when none is
    /// named (a dataset names itself).
    fn with_default_page(&self, call: &ToolCall, kind: ToolKind) -> Value {
        let mut args = call.arguments.clone();
        if kind == ToolKind::Page
            && call.name != page::QUERY_ROWS
            && args.get("page").and_then(Value::as_str).is_none()
            && let (Some(map), Some(latest)) = (
                args.as_object_mut(),
                self.pages
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .latest()
                    .map(str::to_owned),
            )
        {
            map.insert("page".to_owned(), Value::String(latest));
        }
        args
    }
}

/// The session's browser tool and its tools as the model sees them.
async fn open_browser(
    shared: &Shared,
    spec: &SessionSpec,
) -> (Option<Arc<dyn BrowserTools>>, Vec<ToolSpec>) {
    let setup = &shared.setup;
    let browser = match (&spec.browser, &setup.browser) {
        (Some(b), Some(factory)) => Some(factory.open(b, &spec.env)),
        _ => None,
    };
    let specs = match &browser {
        Some(tools) => match tools.specs().await {
            Ok(list) => model_specs(list, &setup.browser_allow),
            Err(err) => {
                tracing::warn!(bot = %spec.bot, %err, "the browser tool couldn't list its tools");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    (browser, specs)
}

/// What a read-only call asks for, to spot repeats: a key and the lines
/// it wants. A section is the same ask whatever its line count when it
/// wants lines already shown; a search the same whatever its limit.
fn repeat_key(name: &str, args: &Value) -> (String, (usize, usize)) {
    let mut args = args.clone();
    if name == page::READ_SECTION {
        let n = |k: &str, d: usize| {
            args.get(k)
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(d)
        };
        let from = n("from_line", 1).max(1);
        let to = from + n("max_lines", 150).clamp(1, 150) - 1;
        let page = args.get("page").and_then(Value::as_str).unwrap_or("");
        let uid = args.get("uid").and_then(Value::as_str).unwrap_or("");
        let uid = uid.trim().trim_start_matches("uid=");
        return (format!("{name} {page} {uid}"), (from, to));
    }
    if let Some(map) = args.as_object_mut() {
        map.remove("limit");
    }
    (call_key(name, &args), (0, 0))
}

/// The last line a section showed (`[lines 1–40 of 90; …]`), if it stopped
/// early.
fn shown_to(text: &str) -> Option<usize> {
    let at = text.rfind("[lines ")?;
    let rest = &text[at + 7..];
    let (_, after) = rest.split_once('–')?;
    after.split(' ').next()?.parse().ok()
}

/// A call's name and arguments (sorted keys), to spot repeats.
fn call_key(name: &str, args: &Value) -> String {
    format!("{name} {args}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sections_repeat_by_page_and_lines() {
        let (key, lines) = repeat_key(
            page::READ_SECTION,
            &json!({"page": "r3", "uid": "uid=1_2", "from_line": 10, "max_lines": 20}),
        );
        assert_eq!(key, format!("{} r3 1_2", page::READ_SECTION));
        assert_eq!(lines, (10, 29));
        let (a, _) = repeat_key(
            page::FIND_IN_PAGE,
            &json!({"page": "r3", "query": "x", "limit": 5}),
        );
        let (b, _) = repeat_key(page::FIND_IN_PAGE, &json!({"page": "r3", "query": "x"}));
        assert_eq!(a, b);
        assert_eq!(shown_to("…\n[lines 1–40 of 90; more]"), Some(40));
        assert_eq!(shown_to("no lines"), None);
    }

    #[test]
    fn the_third_same_failure_gets_a_nudge() {
        let call = ToolCall::new("c1", "mcp__chrome-devtools__click", json!({"uid": "1_2"}));
        let mut failures = Failures::default();
        let mut outcome = ToolOutcome::note("[tool error] no", true);
        failures.count(&call, &mut outcome);
        failures.count(&call, &mut outcome);
        assert!(!outcome.text_for_hub.contains(TRY_ELSE));
        failures.count(&call, &mut outcome);
        assert!(matches!(&outcome.for_model[0], Part::Text(t) if t.ends_with(TRY_ELSE)));
    }
}
