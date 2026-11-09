//! A bot's engine session: starting it on its engine and turning its
//! events into stored events, cards and tool results.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use oxbot_browser::{MCP_SERVER_NAME, normalize_web_url};
use oxbot_protocol::{EventKind, ServerMsg};
use tokio::sync::mpsc;

use super::taint::{UNKNOWN_RESULT, untrusted_tool};
use super::team::team_label;
use super::{Hub, HubError, Runtime, Shared, elements};
use crate::bot::Bot;
use crate::engine::{
    Activity, BrowserSpec, EngineEvent, EngineKind, GateMode, HostTools, ModelChoice, Privacy,
    Resume, Session, SessionSpec, Started, ToolUse,
};
use crate::id::BotId;
use crate::prompt::{Capabilities, system_prompt};
use crate::report::Visit;
use crate::store::{MAX_SESSION_PAGES, SavedSession};
use crate::tools::{self, SAVE_REPORT_TOOL, SIGN_IN_TOOL, TOOL_SERVER_NAME};

/// What a session may be started with instead of the bot's own settings
/// (practice runs, #21). The default is the bot as it is.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionOverrides {
    /// Another engine than the bot's.
    pub engine: Option<EngineKind>,
    /// Another model than the bot's.
    pub model: Option<ModelChoice>,
    /// Another working folder than the bot's `work/`.
    pub work_dir: Option<PathBuf>,
    /// Another folder for what the engine saves about the session.
    pub context_dir: Option<PathBuf>,
    /// The knowledge folder to give the engine.
    pub knowledge_dir: Option<PathBuf>,
    /// No browser at all.
    pub no_browser: bool,
    /// Live, or a practice run.
    pub gate_mode: GateMode,
    /// A throwaway session: always new, never saved as the bot's.
    pub ephemeral: bool,
}

impl Hub {
    /// Starts `bot`'s engine session and a task that turns its events into
    /// stored events. Resumes the saved session unless the system prompt
    /// or the engine has changed.
    pub(super) async fn start_session(self: &Arc<Self>, bot: &Bot) -> Result<Runtime, HubError> {
        self.start_session_with(bot, SessionOverrides::default())
            .await
    }

    /// Like [`Hub::start_session`], with `overrides` in place of the
    /// bot's own settings.
    pub(super) async fn start_session_with(
        self: &Arc<Self>,
        bot: &Bot,
        overrides: SessionOverrides,
    ) -> Result<Runtime, HubError> {
        let bot = &self.as_assistant(bot);
        let kind = overrides.engine.unwrap_or_else(|| self.engine_of(bot));
        let engine = self.engine(kind)?;
        let shared = if overrides.no_browser {
            None
        } else {
            self.browser_of(bot)
        };
        let caps = Capabilities {
            browser: shared.is_some(),
        };
        let mut prompt = system_prompt(bot, caps);
        // Codex runs oxbot's tools: oxbot's rules, not Codex's own.
        prompt.push_str(&crate::prompt::codex_rules(caps));
        let resume = self.resume_for(bot, kind, &prompt, overrides.ephemeral)?;
        // A resumed session still remembers what it read and opened.
        let (taint, visited) = match &resume {
            Resume::Resume(_) => (
                self.store.session_taint(&bot.id)?,
                self.store.session_pages(&bot.id)?.into_iter().collect(),
            ),
            Resume::New(_) => (None, BTreeMap::new()),
        };
        let work_dir = overrides.work_dir.clone().unwrap_or_else(|| bot.work_dir());

        // The bot takes turns on this browser even if Chrome didn't start now.
        let browser = shared.clone();
        let mut browser_spec = match &shared {
            Some(shared) => self.start_browser(bot, shared, &work_dir).await,
            None => None,
        };
        // A browser tool handed to the engines (tests) needs no Chrome.
        let injected = self
            .config
            .openrouter
            .as_ref()
            .is_some_and(|p| p.browser.is_some());
        if browser_spec.is_none() && shared.is_some() && injected {
            browser_spec = Some(BrowserSpec {
                mcp: crate::engine::McpCommand::default(),
            });
        }
        // As the prompt says: the Assistant gets its team tools, a bot given a
        // browser can save reports.
        let host_tools = HostTools {
            central: bot.is_central(),
            browser: shared.is_some(),
        };
        let gate_mode = overrides.gate_mode;
        let spec = SessionSpec {
            bot: bot.id.to_string(),
            work_dir,
            context_dir: overrides
                .context_dir
                .unwrap_or_else(|| bot.dir.join("context")),
            knowledge_dir: overrides.knowledge_dir,
            system_prompt: prompt.clone(),
            keeps_files: oxbot_loop::tools::files::asks_for_file(&bot.instructions),
            resume: resume.clone(),
            model: overrides.model.unwrap_or_else(|| model_of(bot)),
            browser: browser_spec,
            host_tools,
            host_tool_specs: tools::specs(host_tools.into()),
            privacy: if bot.private {
                Privacy::Private
            } else {
                Privacy::Standard
            },
            gate_mode,
            env: self.config.extra_env.clone(),
            // A sign-in or a task can take minutes, and a task for a private
            // bot may wait for a card first: let a tool call wait that long.
            tool_wait: self.config.sign_in_timeout.max(self.config.task_timeout)
                + self.config.approval_timeout
                + Duration::from_mins(1),
        };

        let Started { session, events } = engine.start(spec).await?;
        if !overrides.ephemeral {
            self.store.save_session(
                &bot.id,
                Some(&SavedSession {
                    id: resume.id().to_owned(),
                    prompt,
                }),
                kind,
            )?;
        }
        tokio::spawn(Arc::clone(self).pump(bot.id.clone(), events));
        let mut runtime = new_runtime(session, browser, &resume, taint, visited);
        runtime.engine = kind;
        runtime.gate_mode = gate_mode;
        Ok(runtime)
    }

    /// Starts `shared`'s Chrome (installing the browser tool the first
    /// time) for `bot`, saving files in `work_dir`. `None` if this oxbot
    /// has no browser or it couldn't start: the bot goes on without one.
    async fn start_browser(
        self: &Arc<Self>,
        bot: &Bot,
        shared: &Arc<Shared>,
        work_dir: &std::path::Path,
    ) -> Option<BrowserSpec> {
        let tools = self.config.browser.clone()?;
        if !tools.is_installed() {
            self.notice("Setting up the browser for bots (first time only)…");
        }
        let started = async {
            tools.ensure_installed().await?;
            shared.browser.ensure_running().await
        };
        if let Err(err) = started.await {
            tracing::warn!(%err, "starting without the browser");
            self.notice(&format!(
                "{} can't use the browser right now: {err}",
                bot.name
            ));
            return None;
        }
        // Once per run, and after an upgrade renamed its sites (until a
        // check works), drop remembered sites the browser has no cookies for.
        // A practice run's browser holds a copy: never checked against the
        // real list.
        let due = self.store.site_check_due(&shared.owner).unwrap_or(false)
            && !super::trials::is_trial(&bot.id);
        if !shared.sites_checked.swap(true, Ordering::Relaxed) || due {
            let hub = Arc::clone(self);
            let shared = Arc::clone(shared);
            tokio::spawn(async move {
                // Not checked after all: try again on the next start.
                if !hub.refresh_sites(&shared).await {
                    shared.sites_checked.store(false, Ordering::Relaxed);
                }
            });
        }
        Some(BrowserSpec {
            mcp: tools.mcp_command(shared.browser.port(), work_dir),
        })
    }

    /// Which session `bot` starts on `kind` with `prompt`: its saved one,
    /// unless the prompt or the engine changed (or it's `ephemeral`).
    fn resume_for(
        &self,
        bot: &Bot,
        kind: EngineKind,
        prompt: &str,
        ephemeral: bool,
    ) -> Result<Resume, HubError> {
        let saved = if ephemeral {
            None
        } else {
            self.store.session(&bot.id)?
        };
        let saved_engine = match &saved {
            Some(_) => self.store.session_engine(&bot.id)?,
            None => None,
        };
        Ok(match saved {
            Some(saved)
                if saved.prompt == prompt && saved_engine.as_deref() == Some(kind.as_str()) =>
            {
                Resume::Resume(saved.id)
            }
            Some(_) if saved_engine.as_deref() != Some(kind.as_str()) => {
                // The other engine's model has read none of it.
                tracing::info!(bot = %bot.id, engine = %kind, "engine changed; starting a new session");
                Resume::New(uuid::Uuid::new_v4().to_string())
            }
            saved => {
                if saved.is_some() {
                    tracing::info!(bot = %bot.id, "system prompt changed; starting a new session");
                }
                Resume::New(uuid::Uuid::new_v4().to_string())
            }
        })
    }

    /// Turns one bot's engine events into stored events until its session
    /// ends.
    async fn pump(self: Arc<Self>, bot: BotId, mut events: mpsc::Receiver<EngineEvent>) {
        let mut started = false;
        while let Some(event) = events.recv().await {
            if let Err(err) = self.handle(&bot, event, &mut started).await {
                tracing::error!(%bot, %err, "couldn't record the engine's output");
            }
        }

        // The session ended (its engine stopped it).
        self.deny_open_approvals(&bot);
        self.cancel_sign_ins(&bot);
        let removed = self.runtimes.lock().await.remove(&bot);
        let was_busy = removed.is_some_and(|mut r| {
            Self::release_browser(&bot, &mut r);
            r.busy
        });
        if !started && was_busy {
            // It never got going (e.g. its saved session is gone): start fresh next time.
            tracing::warn!(%bot, "the session ended before starting; forgetting it");
            let _ = self.store.set_session(&bot, None);
        }
        if was_busy {
            let _ = self.turn_failed(&bot, "The bot's AI helper stopped unexpectedly.");
        }
        self.run_queued(&bot);
    }

    async fn handle(
        self: &Arc<Self>,
        bot: &BotId,
        event: EngineEvent,
        started: &mut bool,
    ) -> Result<(), HubError> {
        match event {
            EngineEvent::Started { .. } => *started = true,
            EngineEvent::TextDelta(text) => {
                let _ = self.updates.send(ServerMsg::Delta {
                    bot: bot.to_string(),
                    text,
                });
            }
            EngineEvent::Assistant { text, tool_uses } => {
                if !text.trim().is_empty() {
                    self.record(bot, EventKind::AssistantMessage { text })?;
                }
                self.tools_called(bot, &tool_uses).await;
                for tool in tool_uses {
                    let summary = tool.summary();
                    let name = tool_label(&tool.name);
                    self.record(bot, EventKind::ToolUse { name, summary })?;
                }
            }
            EngineEvent::ToolResult { tool_use_id, text } => {
                self.tool_result(bot, &tool_use_id, &text).await;
                let _ = self.updates.send(ServerMsg::Activity {
                    bot: bot.to_string(),
                    text: activity_label(&Activity::Requesting),
                });
            }
            EngineEvent::Permission(request) => {
                tokio::spawn(Arc::clone(self).permission(bot.clone(), request));
            }
            EngineEvent::HostTool(call) => {
                tokio::spawn(Arc::clone(self).host_tool(bot.clone(), call));
            }
            EngineEvent::Sealed(call) => {
                tokio::spawn(Arc::clone(self).sealed_work(bot.clone(), call));
            }
            EngineEvent::ModelCall(record) if super::trials::is_trial(bot) => {
                self.trial_model_call(bot, &record);
            }
            EngineEvent::ModelCall(record) => self.note_model_call(bot, &record).await,
            EngineEvent::TurnDone {
                ok,
                text,
                duration_ms,
            } => {
                self.deny_open_approvals(bot);
                self.cancel_sign_ins(bot);
                if !ok {
                    self.record(bot, EventKind::Error { message: text })?;
                }
                self.record(bot, EventKind::TurnDone { ok, duration_ms })?;
                let meter = if let Some(runtime) = self.runtimes.lock().await.get_mut(bot) {
                    runtime.busy = false;
                    runtime.permit = None;
                    runtime.last_active = Instant::now();
                    // A sealed tab never outlives its turn.
                    if let Some(shared) = runtime.browser.clone() {
                        tokio::spawn(async move { shared.browser.close_sealed().await });
                    }
                    Self::release_browser(bot, runtime);
                    Some(std::mem::take(&mut runtime.meter))
                } else {
                    None
                };
                if let Some(meter) = meter {
                    self.announce_meter(bot, &meter.model, meter.requests, meter.cost_usd);
                }
                self.cue_data_policy();
                self.announce_bots().await;
                // A message sent while it worked that didn't go in goes now.
                self.run_queued(bot);
            }
            EngineEvent::Activity(activity) => {
                let _ = self.updates.send(ServerMsg::Activity {
                    bot: bot.to_string(),
                    text: activity_label(&activity),
                });
            }
        }
        Ok(())
    }
}

impl Hub {
    /// Notes the tools the bot just called, so their results can be read,
    /// and marks the session for an untrusted one as soon as it's called,
    /// so a team tool called meanwhile sees it.
    async fn tools_called(&self, bot: &BotId, tool_uses: &[ToolUse]) {
        let mut untrusted = None;
        if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
            for tool in tool_uses {
                r.tool_names.insert(tool.id.clone(), tool.name.clone());
                untrusted = untrusted_tool(&tool.name, r.page.as_ref()).or(untrusted);
            }
        }
        if let Some(why) = untrusted {
            self.mark_untrusted(bot, why).await;
        }
    }

    /// Reads a tool's result: what the browser reported, and whether the
    /// session may no longer trust what it has read.
    async fn tool_result(&self, bot: &BotId, tool_use_id: &str, text: &str) {
        let mut untrusted = None;
        let mut files = Vec::new();
        let mut visit = None;
        if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
            let name = r.tool_names.remove(tool_use_id);
            if let Some(action) = name
                .as_deref()
                .and_then(|n| n.strip_prefix(&format!("mcp__{MCP_SERVER_NAME}__")))
            {
                visit = learn_from_result(r, action, text);
                files = elements::files_in(text);
            }
            untrusted = match &name {
                Some(name) => untrusted_tool(name, r.page.as_ref()),
                None => Some(UNKNOWN_RESULT.to_owned()),
            };
            r.last_active = Instant::now();
        }
        if let Some(why) = untrusted {
            self.mark_untrusted(bot, why).await;
        }
        if let Some((url, title)) = visit
            && let Err(err) = self.store.add_session_page(bot, &url, &title)
        {
            tracing::warn!(%bot, %err, "couldn't save a page the session opened");
        }
        if !files.is_empty() {
            self.learn_files(bot, files).await;
        }
    }

    /// Reads the snapshot files the bot saved, as a browser result says
    /// (off the async threads), into `bot`'s element book.
    async fn learn_files(&self, bot: &BotId, files: Vec<PathBuf>) {
        let Ok(work_dir) = self.run_bot(bot.as_str()).map(|b| b.work_dir()) else {
            return;
        };
        let read = tokio::task::spawn_blocking(move || {
            files
                .into_iter()
                .filter_map(|file| {
                    let path = elements::allowed_path(&file, &work_dir);
                    if path.is_none() {
                        tracing::debug!(path = %file.display(), "not reading a snapshot file outside the bot's folder");
                    }
                    elements::read_file(&path?)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
            for contents in read {
                let learned = r.elements.learn_file(&contents);
                tracing::debug!(%bot, learned, "read elements from a snapshot file");
            }
        }
    }
}

/// The model a bot's settings name: its own list when it has one, else the
/// model pinned before bots had lists.
fn model_of(bot: &Bot) -> ModelChoice {
    if !bot.loop_settings.models.is_empty() {
        return ModelChoice::List(bot.loop_settings.models.clone());
    }
    match bot.loop_settings.model.as_deref() {
        None | Some("" | "auto") => ModelChoice::Auto,
        Some(id) => ModelChoice::Pin(id.to_owned()),
    }
}

/// A just-started session's runtime. A resumed session keeps what it had
/// read (`taint`) and the pages it opened (`visited`); a new one starts
/// trusted, with none.
fn new_runtime(
    session: Box<dyn Session>,
    browser: Option<Arc<Shared>>,
    resume: &Resume,
    taint: Option<String>,
    visited: BTreeMap<String, Visit>,
) -> Runtime {
    let mut runtime = Runtime {
        session,
        engine: EngineKind::Codex,
        gate_mode: GateMode::Live,
        busy: false,
        permit: None,
        browser,
        lease: None,
        last_active: Instant::now(),
        tool_names: HashMap::new(),
        page: None,
        elements: elements::ElementBook::default(),
        origin: super::ChainOrigin::default(),
        taint: None,
        question: None,
        visited,
        meter: super::looped::TurnMeter::default(),
    };
    match (resume, taint) {
        (Resume::Resume(_), Some(why)) => {
            runtime.mark_untrusted(why);
        }
        (Resume::Resume(_), None) => {}
        (Resume::New(_), _) => runtime.reset_for_new_session(),
    }
    runtime
}

/// Reads what the browser tool's `action` reported: the selected page, and
/// the element names from a page snapshot (added to what it knew). Returns
/// a web page this session hadn't opened before, (address, title), to keep
/// with the session.
fn learn_from_result(runtime: &mut Runtime, action: &str, text: &str) -> Option<(String, String)> {
    let mut visit = None;
    if let Some((title, url)) = listed_selected_page(action, text) {
        if let Some(key) = normalize_web_url(&url)
            && !runtime.visited.contains_key(&key)
        {
            note_visit(&mut runtime.visited, &key, &title);
            visit = Some((key, title.clone()));
        }
        runtime.page = Some((title, url));
    }
    runtime.elements.learn(text);
    visit
}

/// Adds a page to what the session opened, dropping the oldest past the
/// store's limit (as the store does).
pub(super) fn note_visit(visited: &mut BTreeMap<String, Visit>, url: &str, title: &str) {
    visited.insert(
        url.to_owned(),
        Visit {
            title: title.to_owned(),
            first_ms: crate::store::now_ms(),
        },
    );
    while visited.len() > MAX_SESSION_PAGES {
        let Some(oldest) = visited
            .iter()
            .min_by_key(|(_, v)| v.first_ms)
            .map(|(url, _)| url.clone())
        else {
            break;
        };
        visited.remove(&oldest);
    }
}

/// Browser tools whose results carry text a page chose ahead of their own
/// sections (network bodies, console messages, script results): their
/// page lists aren't read.
const PAGE_TEXT_FIRST: [&str; 5] = [
    "evaluate_script",
    "get_network_request",
    "list_network_requests",
    "get_console_message",
    "list_console_messages",
];

/// The selected page from a browser result's page list, as (title,
/// address). Text on a page can contain lines that look like a page list,
/// so only the real one counts: the result's first `## ` heading must be
/// `## Pages` with nothing a page wrote before it (an open dialog's text
/// comes first), only page lines are read, it must be the only `## Pages`
/// in the result, and exactly one page in it is selected.
pub(super) fn listed_selected_page(action: &str, text: &str) -> Option<(String, String)> {
    if PAGE_TEXT_FIRST.contains(&action) {
        return None;
    }
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if line.starts_with("# Open dialog") {
            return None;
        }
        if line.starts_with("## ") {
            if line.trim_end() != "## Pages" {
                return None;
            }
            break;
        }
    }
    let mut selected = Vec::new();
    for line in lines.by_ref() {
        if !is_page_line(line) {
            break;
        }
        selected.extend(selected_page(line.trim()));
    }
    // A second list later on is a page's text.
    if lines.any(|l| l.trim_end() == "## Pages") || selected.len() != 1 {
        return None;
    }
    selected.pop()
}

/// `3: …`: a line of the page list.
fn is_page_line(line: &str) -> bool {
    line.split_once(": ")
        .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// `3: Inbox (https://mail.google.com/…) [selected]` → (`Inbox`, address).
pub(super) fn selected_page(line: &str) -> Option<(String, String)> {
    let label = line.strip_suffix(" [selected]")?;
    let (number, label) = label.split_once(": ")?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if let Some(inner) = label.strip_suffix(')')
        && let Some((title, url)) = inner.rsplit_once(" (")
    {
        return Some((title.to_owned(), url.to_owned()));
    }
    Some((String::new(), label.to_owned()))
}

/// How a tool call shows in the chat: `browser navigate_page`,
/// `sign-in request`.
pub(super) fn tool_label(name: &str) -> String {
    if name == format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}") {
        return "sign-in request".to_owned();
    }
    if let Some(label) = team_label(name) {
        return label.to_owned();
    }
    if let Some((label, _)) = loop_label(name) {
        return label.to_owned();
    }
    match name.strip_prefix(&format!("mcp__{MCP_SERVER_NAME}__")) {
        Some(action) => format!("browser {action}"),
        None => name.to_owned(),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The chat's words for one of the loop's own tools: (the line for the
/// call, the status while it runs).
fn loop_label(name: &str) -> Option<(&'static str, &'static str)> {
    let tool = name.strip_prefix(&format!("mcp__{TOOL_SERVER_NAME}__"))?;
    Some(match tool {
        "find_in_page" => ("page search", "Searching the page…"),
        "read_section" => ("page section", "Reading part of the page…"),
        "ask_page" => ("page question", "Asking about the page…"),
        "extract_rows" => ("page tables", "Reading the page's tables…"),
        "query_rows" => ("table query", "Sorting and counting rows…"),
        "read_pages" => ("pages read with a script", "Reading pages with a script…"),
        "page_steps" => ("several steps at once", "Doing several steps on the page…"),
        "run_program" => ("small script", "Running a small script…"),
        "read_result" => ("saved result", "Reading a saved result…"),
        "update_todo" => ("to-do list", "Updating its to-do list…"),
        "note_progress" => ("progress note", "Noting where it is…"),
        _ => return None,
    })
}

/// The status line for an activity, in the terminal's spirit.
pub(super) fn activity_label(activity: &Activity) -> String {
    match activity {
        // The same word for every engine.
        Activity::Requesting => "Working…".to_owned(),
        Activity::Thinking => "Thinking…".to_owned(),
        Activity::Writing => "Writing…".to_owned(),
        Activity::Retrying => "Waiting to retry…".to_owned(),
        Activity::UsingTool(name)
            if name == &format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}") =>
        {
            "Asking you to sign in…".to_owned()
        }
        Activity::UsingTool(name) if name == &tools::full_name(SAVE_REPORT_TOOL) => {
            "Writing a report…".to_owned()
        }
        Activity::UsingTool(name) if team_label(name).is_some() => {
            format!("{}…", capitalized(team_label(name).unwrap_or_default()))
        }
        Activity::UsingTool(name) if loop_label(name).is_some() => loop_label(name)
            .map(|(_, status)| status.to_owned())
            .unwrap_or_default(),
        Activity::UsingTool(name) => {
            match name.strip_prefix(&format!("mcp__{MCP_SERVER_NAME}__")) {
                Some(action) => format!("Using browser: {action}…"),
                None => format!("Using {name}…"),
            }
        }
        Activity::Other(status) => format!("{status}…"),
    }
}
