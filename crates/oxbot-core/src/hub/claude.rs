//! A bot's `claude` child: starting it and turning its output into stored
//! events, cards and tool results.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use oxbot_browser::{MCP_SERVER_NAME, normalize_web_url};
use oxbot_claude::{Activity, ClaudeConfig, ClaudeEvent, ClaudeProcess, Session, ToolUse};
use oxbot_protocol::{EventKind, ServerMsg};
use serde_json::json;
use tokio::sync::mpsc;

use super::taint::{UNKNOWN_RESULT, untrusted_tool};
use super::team::team_label;
use super::{Hub, HubError, Runtime, Shared, elements};
use crate::bot::Bot;
use crate::id::BotId;
use crate::prompt::{Capabilities, system_prompt};
use crate::report::Visit;
use crate::store::{MAX_SESSION_PAGES, SavedSession};
use crate::tools::{self, SAVE_REPORT_TOOL, SIGN_IN_TOOL, TOOL_SERVER_NAME};

impl Hub {
    /// Starts `bot`'s Claude and a task that turns its output into events.
    /// Resumes the saved session unless the system prompt has changed.
    pub(super) async fn start_claude(self: &Arc<Self>, bot: &Bot) -> Result<Runtime, HubError> {
        let bot = &self.as_herder(bot);
        let shared = self.browser_of(bot);
        let caps = Capabilities {
            browser: shared.is_some(),
        };
        let prompt = system_prompt(bot, caps);
        let session = match self.store.session(&bot.id)? {
            Some(saved) if saved.prompt == prompt => Session::Resume(saved.id),
            saved => {
                if saved.is_some() {
                    tracing::info!(bot = %bot.id, "system prompt changed; starting a new Claude session");
                }
                Session::New(uuid::Uuid::new_v4().to_string())
            }
        };
        // A resumed session still remembers what it read and opened.
        let (taint, visited) = match &session {
            Session::Resume(_) => (
                self.store.session_taint(&bot.id)?,
                self.store.session_pages(&bot.id)?.into_iter().collect(),
            ),
            Session::New(_) => (None, BTreeMap::new()),
        };
        let mut config = ClaudeConfig::new(bot.work_dir(), session.clone());
        config.program.clone_from(&self.config.claude_program);
        config.model.clone_from(&bot.model);
        config.extra_env.clone_from(&self.config.extra_env);
        config.instructions = Some(prompt.clone());
        // Every tool that isn't pre-allowed is checked by oxbot.
        config.ask_oxbot = true;

        let mut servers = serde_json::Map::new();
        // The bot takes turns on this browser even if Chrome didn't start now.
        let browser = shared.clone();
        if let (Some(tools), Some(shared)) = (self.config.browser.clone(), &shared) {
            if !tools.is_installed() {
                self.notice("Setting up the browser for bots (first time only)…");
            }
            let started = async {
                tools.ensure_installed().await?;
                shared.browser.ensure_running().await
            };
            match started.await {
                Ok(()) => {
                    servers.insert(
                        MCP_SERVER_NAME.to_owned(),
                        tools.mcp_server(shared.browser.port(), &bot.work_dir()),
                    );
                    servers.insert(TOOL_SERVER_NAME.to_owned(), tools::mcp_entry());
                    // Once per run, and after an upgrade renamed its sites (until
                    // a check works), drop remembered sites the browser has no
                    // cookies for.
                    let due = self.store.site_check_due(&shared.owner).unwrap_or(false);
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
                }
                Err(err) => {
                    tracing::warn!(%err, "starting without the browser");
                    self.notice(&format!(
                        "{} can't use the browser right now: {err}",
                        bot.name
                    ));
                }
            }
        }
        // Herder always gets its team tools, browser or not.
        if bot.is_central() {
            servers.insert(TOOL_SERVER_NAME.to_owned(), tools::mcp_entry());
        }
        if !servers.is_empty() {
            config.mcp_config = Some(json!({ "mcpServers": servers }).to_string());
            // A sign-in or a task can take minutes, and a task for a private
            // bot may wait for a card first: let a tool call wait that long.
            let wait = self.config.sign_in_timeout.max(self.config.task_timeout)
                + self.config.approval_timeout
                + Duration::from_mins(1);
            config
                .extra_env
                .push(("MCP_TOOL_TIMEOUT".to_owned(), wait.as_millis().to_string()));
        }

        let (process, events) = ClaudeProcess::spawn(&config)?;
        self.store.set_session(
            &bot.id,
            Some(&SavedSession {
                id: session.id().to_owned(),
                prompt,
            }),
        )?;
        tokio::spawn(Arc::clone(self).pump(bot.id.clone(), events));
        Ok(new_runtime(process, browser, &session, taint, visited))
    }

    /// Turns one bot's Claude output into stored events until the child exits.
    async fn pump(self: Arc<Self>, bot: BotId, mut events: mpsc::Receiver<ClaudeEvent>) {
        let mut started = false;
        while let Some(event) = events.recv().await {
            if let Err(err) = self.handle(&bot, event, &mut started).await {
                tracing::error!(%bot, %err, "couldn't record claude output");
            }
        }

        // The child exited.
        self.deny_open_approvals(&bot);
        self.cancel_sign_ins(&bot);
        let removed = self.runtimes.lock().await.remove(&bot);
        let was_busy = removed.is_some_and(|mut r| {
            Self::release_browser(&bot, &mut r);
            r.busy
        });
        if !started && was_busy {
            // It never got going (e.g. its saved session is gone): start fresh next time.
            tracing::warn!(%bot, "claude exited before starting; forgetting its session");
            let _ = self.store.set_session(&bot, None);
        }
        if was_busy {
            let _ = self.turn_failed(&bot, "Claude stopped unexpectedly.");
        }
    }

    async fn handle(
        self: &Arc<Self>,
        bot: &BotId,
        event: ClaudeEvent,
        started: &mut bool,
    ) -> Result<(), HubError> {
        match event {
            ClaudeEvent::Init { model, .. } => {
                *started = true;
                self.note_model(bot, &model);
            }
            ClaudeEvent::TextDelta(text) => {
                let _ = self.updates.send(ServerMsg::Delta {
                    bot: bot.to_string(),
                    text,
                });
            }
            ClaudeEvent::Assistant { text, tool_uses } => {
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
            ClaudeEvent::ToolResult { tool_use_id, text } => {
                self.tool_result(bot, &tool_use_id, &text).await;
                let _ = self.updates.send(ServerMsg::Activity {
                    bot: bot.to_string(),
                    text: activity_label(&Activity::Requesting),
                });
            }
            ClaudeEvent::Permission(request) => {
                tokio::spawn(Arc::clone(self).permission(bot.clone(), request));
            }
            ClaudeEvent::McpMessage {
                request_id,
                server,
                message,
            } => {
                if server == TOOL_SERVER_NAME {
                    tokio::spawn(Arc::clone(self).tool_message(bot.clone(), request_id, message));
                } else {
                    tracing::warn!(%bot, server, "message for a tool server oxbot doesn't host");
                }
            }
            ClaudeEvent::Limits(windows) => {
                self.store.set_limits(&windows)?;
                self.check_limit_alerts(&windows);
                self.announce_usage();
            }
            ClaudeEvent::TurnResult {
                ok,
                text,
                duration_ms,
                usage,
            } => {
                self.deny_open_approvals(bot);
                self.cancel_sign_ins(bot);
                self.store.add_usage(bot, &usage)?;
                if !ok {
                    self.record(bot, EventKind::Error { message: text })?;
                }
                self.record(bot, EventKind::TurnDone { ok, duration_ms })?;
                if let Some(runtime) = self.runtimes.lock().await.get_mut(bot) {
                    runtime.busy = false;
                    runtime.permit = None;
                    runtime.last_active = Instant::now();
                    Self::release_browser(bot, runtime);
                }
                self.announce_bots().await;
                self.announce_usage();
            }
            ClaudeEvent::Activity(activity) => {
                let _ = self.updates.send(ServerMsg::Activity {
                    bot: bot.to_string(),
                    text: activity_label(&activity),
                });
            }
            ClaudeEvent::ControlAck { .. } => {}
        }
        Ok(())
    }
}

impl Hub {
    /// Notes the tools Claude just called, so their results can be read,
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

    /// Reads the snapshot files a browser result points to (off the async
    /// threads) into `bot`'s element book.
    async fn learn_files(&self, bot: &BotId, files: Vec<elements::SnapshotFile>) {
        let Ok(work_dir) = self.bot(bot.as_str()).map(|b| b.work_dir()) else {
            return;
        };
        let Some(claude_home) = elements::claude_home() else {
            return;
        };
        let read = tokio::task::spawn_blocking(move || {
            files
                .into_iter()
                .filter_map(|file| {
                    let path = elements::allowed_path(&file, &claude_home, &work_dir);
                    if path.is_none() {
                        tracing::debug!(path = %file.path.display(), "not reading a snapshot file outside the allowed folders");
                    }
                    Some((elements::read_file(&path?)?, file.saved_snapshot))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
            for (contents, saved_snapshot) in read {
                let learned = r.elements.learn_file(&contents, saved_snapshot);
                tracing::debug!(%bot, learned, "read elements from a snapshot file");
            }
        }
    }
}

/// A just-started `claude` child's runtime. A resumed session keeps what
/// it had read (`taint`) and the pages it opened (`visited`); a new one
/// starts trusted, with none.
fn new_runtime(
    process: ClaudeProcess,
    browser: Option<Arc<Shared>>,
    session: &Session,
    taint: Option<String>,
    visited: BTreeMap<String, Visit>,
) -> Runtime {
    let mut runtime = Runtime {
        process,
        busy: false,
        permit: None,
        browser,
        lease: None,
        last_active: Instant::now(),
        tool_names: HashMap::new(),
        page: None,
        elements: elements::ElementBook::default(),
        asker: None,
        taint: None,
        question: None,
        visited,
    };
    match (session, taint) {
        (Session::Resume(_), Some(why)) => {
            runtime.mark_untrusted(why);
        }
        (Session::Resume(_), None) => {}
        (Session::New(_), _) => runtime.reset_for_new_session(),
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

/// The status line for an activity, in the terminal's spirit.
pub(super) fn activity_label(activity: &Activity) -> String {
    match activity {
        Activity::Requesting => "Clauding…".to_owned(),
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
        Activity::UsingTool(name) => {
            match name.strip_prefix(&format!("mcp__{MCP_SERVER_NAME}__")) {
                Some(action) => format!("Using browser: {action}…"),
                None => format!("Using {name}…"),
            }
        }
        Activity::Other(status) => format!("{status}…"),
    }
}
