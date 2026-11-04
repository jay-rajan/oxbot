//! The tools a loop session offers, in a fixed order (oxbot's own tools,
//! browser tools sorted by name, file tools, then the loop's own), with
//! the names the hub's rules use. [`ToolRegistry::run`] needs a [`Permit`]:
//! only the gate makes one.
//!
//! Results the model gets are kept small: a page snapshot is stored whole
//! (the hub still sees all of it) and the model is shown the trimmed page,
//! a map of a big one, or what changed since its last look; any other big
//! result is stored and shown in part. Stored results are read back by
//! handle (`r12`) with `read_result` and the page tools.

pub mod browser;
pub mod control;
pub mod files;
pub mod mcp;
pub mod page;
pub mod pages;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use oxbot_engine::text::data_block;
use oxbot_engine::trust::{HOST_TOOL_PREFIX, is_trusted_tool};
use oxbot_engine::{EngineEvent, HostToolCall, Reply, ToolReply};
use oxbot_llm::{Part, ToolCall, ToolSpec};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::budget::{TokenBudget, est};
use crate::context::state::{EntryKind, EntryMeta};
use crate::context::{short_tool, short_url, tokens_in_words};
use crate::gate::Permit;
use crate::page::{self as snap, Baseline, Snapshot};
use crate::results::{NewResult, ResultStore};
use crate::store::ResultKind;
use browser::{BROWSER_PREFIX, BrowserTools, SIDE_EFFECT_TOOLS};
use pages::PageTracker;

/// Results smaller than this (tokens) are never stored: a stub would be
/// about as long.
pub const STORE_OVER: u64 = 200;
/// Longest tool error kept, in characters.
pub const MAX_ERROR: usize = 600;

/// oxbot's own tools that change something (written ahead, not re-run
/// after a restart).
const HOST_SIDE_EFFECTS: [&str; 2] = ["create_bot", "give_task"];

/// What kind of tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// oxbot's own, run by the hub.
    Host,
    /// The browser tool.
    Browser,
    /// Files in `work/`.
    File,
    /// The loop's own (`ask_user`, `update_todo`, `note_progress`,
    /// `read_result`).
    Control,
    /// The page tools (over stored snapshots).
    Page,
}

/// One tool of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolEntry {
    /// As the model sees it.
    pub spec: ToolSpec,
    /// What kind.
    pub kind: ToolKind,
    /// Whether it changes something outside oxbot.
    pub side_effect: bool,
}

/// A tool's result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// The full text, for the hub (pages, element names, taint).
    pub text_for_hub: String,
    /// What the model gets (outside text wrapped as data, big results cut).
    pub for_model: Vec<Part>,
    /// Outside text (web pages, files, other bots).
    pub untrusted: bool,
    /// Where the whole result is stored (`r12`), if it is.
    pub handle: Option<String>,
    /// The tool failed.
    pub error: bool,
    /// The context builder's notes on it.
    pub meta: EntryMeta,
}

impl ToolOutcome {
    /// A result oxbot wrote itself (refusals, practice runs, errors before
    /// running).
    #[must_use]
    pub fn note(text: impl Into<String>, error: bool) -> Self {
        let text = text.into();
        Self {
            text_for_hub: text.clone(),
            for_model: vec![Part::Text(text)],
            untrusted: false,
            handle: None,
            error,
            meta: EntryMeta {
                kind: if error {
                    EntryKind::Error
                } else {
                    EntryKind::Normal
                },
                ..EntryMeta::default()
            },
        }
    }

    /// The same, as `kind` (refusals and the to-do list are never cleared).
    #[must_use]
    pub fn kind(mut self, kind: EntryKind) -> Self {
        self.meta.kind = kind;
        self
    }
}

/// What running a tool needs from the session.
pub struct ToolEnv<'a> {
    /// The bot's `work/` folder.
    pub work_dir: &'a Path,
    /// Where host tool calls go.
    pub events: &'a mpsc::Sender<EngineEvent>,
    /// The browser tool, if the session has one.
    pub browser: Option<&'a Arc<dyn BrowserTools>>,
    /// Stops the call.
    pub cancel: &'a CancellationToken,
    /// Where big results are stored.
    pub results: &'a ResultStore,
    /// The browser's tabs.
    pub pages: &'a Mutex<PageTracker>,
    /// Strictness of an element label (the gate's).
    pub strictness: Arc<crate::Strictness>,
    /// The turn's budget (caps).
    pub budget: TokenBudget,
    /// The turn and step, for stored results.
    pub turn: u32,
    /// The step.
    pub step: u32,
    /// Whether the model can still see stored result `handle` in full
    /// (in the window and not cleared): only then is a diff against it
    /// shown.
    pub visible: &'a (dyn Fn(&str) -> bool + Send + Sync),
}

impl std::fmt::Debug for ToolEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEnv")
            .field("work_dir", &self.work_dir)
            .field("turn", &self.turn)
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

/// A session's tools.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    entries: Vec<ToolEntry>,
}

impl ToolRegistry {
    /// The tools: `host` (oxbot's own, as given), `browser` (already
    /// prefixed and sorted), the file tools, then the loop's own (the page
    /// tools only with a browser).
    #[must_use]
    pub fn new(host: Vec<ToolSpec>, browser: Vec<ToolSpec>) -> Self {
        let mut entries = Vec::new();
        for spec in host {
            let bare = spec.name.strip_prefix(HOST_TOOL_PREFIX).unwrap_or_default();
            let side_effect = HOST_SIDE_EFFECTS.contains(&bare);
            entries.push(ToolEntry {
                spec,
                kind: ToolKind::Host,
                side_effect,
            });
        }
        let has_browser = !browser.is_empty();
        for spec in browser {
            let bare = spec.name.strip_prefix(BROWSER_PREFIX).unwrap_or_default();
            let side_effect = SIDE_EFFECT_TOOLS.contains(&bare);
            entries.push(ToolEntry {
                spec,
                kind: ToolKind::Browser,
                side_effect,
            });
        }
        for spec in files::specs() {
            let side_effect = files::changes_files(&spec.name);
            entries.push(ToolEntry {
                spec,
                kind: ToolKind::File,
                side_effect,
            });
        }
        for spec in [
            control::ask_user_spec(),
            control::update_todo_spec(),
            control::note_progress_spec(),
            control::read_result_spec(),
        ] {
            entries.push(ToolEntry {
                spec,
                kind: ToolKind::Control,
                side_effect: false,
            });
        }
        if has_browser {
            for spec in page::specs() {
                entries.push(ToolEntry {
                    spec,
                    kind: ToolKind::Page,
                    side_effect: false,
                });
            }
        }
        Self { entries }
    }

    /// The same tools without those called `names`.
    #[must_use]
    pub fn without(mut self, names: &[&str]) -> Self {
        self.entries
            .retain(|e| !names.contains(&e.spec.name.as_str()));
        self
    }

    /// The tools as the model sees them, in order.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.entries.iter().map(|e| e.spec.clone()).collect()
    }

    /// A tool by the name the model uses.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.entries.iter().find(|e| e.spec.name == name)
    }

    /// Checks a call before asking the gate: a known tool, arguments that
    /// are a JSON object, and every required argument there.
    ///
    /// # Errors
    /// What to tell the model.
    pub fn validate(&self, call: &ToolCall) -> Result<&ToolEntry, String> {
        let Some(entry) = self.get(&call.name) else {
            // The right tool under another prefix: name it.
            let bare = call.name.rsplit("__").next().unwrap_or(&call.name);
            if let Some(right) = self
                .entries
                .iter()
                .find(|e| e.spec.name.rsplit("__").next() == Some(bare))
            {
                return Err(format!(
                    "There is no tool called \"{}\"; it's {}.",
                    shorten(&call.name, 80),
                    right.spec.name
                ));
            }
            return Err(format!(
                "There is no tool called \"{}\". Use one of the tools you were given.",
                shorten(&call.name, 80)
            ));
        };
        match argument_problem(entry, call) {
            Some(ArgProblem::Unreadable(why)) => Err(format!(
                "Your call to {} couldn't be read ({why}). You sent: {}",
                call.name,
                shorten(&call.raw, 500)
            )),
            Some(ArgProblem::Missing(field)) => Err(format!("{} needs {field}.", call.name)),
            None => Ok(entry),
        }
    }

    /// For calls to known tools whose arguments can't be used (not a JSON
    /// object, or a required field missing): a short note for asking the
    /// model again, naming each tool, what was wrong and its required
    /// fields. `None` when every call can be used (or names an unknown
    /// tool: [`Self::validate`] says which one is meant).
    #[must_use]
    pub fn reask_note(&self, calls: &[ToolCall]) -> Option<String> {
        let lines: Vec<String> = calls
            .iter()
            .filter_map(|call| {
                let entry = self.get(&call.name)?;
                let why = match argument_problem(entry, call)? {
                    ArgProblem::Unreadable(why) => why,
                    ArgProblem::Missing(field) => format!("{field} is missing"),
                };
                let required = required_fields(entry);
                let needs = if required.is_empty() {
                    "It has no required fields".to_owned()
                } else {
                    format!("It needs {}", required.join(", "))
                };
                Some(format!(
                    "Your call to {} couldn't be used: {why}. {needs}; send its arguments as one \
                     JSON object.",
                    call.name
                ))
            })
            .collect();
        if lines.is_empty() {
            return None;
        }
        Some(format!("[oxbot] {} Call it again.", lines.join(" ")))
    }

    /// Runs a host, browser or file call the gate allowed (the loop's own
    /// tools run in the turn, which holds their state).
    pub async fn run(&self, call: &ToolCall, permit: Permit, env: &ToolEnv<'_>) -> ToolOutcome {
        debug_assert_eq!(permit.tool_use_id(), call.id);
        let Some(entry) = self.get(&call.name) else {
            return ToolOutcome::note(format!("[tool error] unknown tool {}", call.name), true);
        };
        let untrusted = !is_trusted_tool(&call.name);
        let raw = match entry.kind {
            ToolKind::Host => host(call, env).await,
            ToolKind::Browser => browser(call, env).await,
            ToolKind::File => file(call, env).await,
            ToolKind::Control | ToolKind::Page => {
                Raw::error(format!("{} runs in the turn", call.name))
            }
        };
        let untrusted = untrusted || raw.taints;
        finish(raw, &call.name, untrusted, env)
    }
}

/// What is wrong with a known tool's arguments.
enum ArgProblem {
    /// Not a JSON object.
    Unreadable(String),
    /// A required field isn't there.
    Missing(String),
}

/// The tool's required fields, in its schema's order.
fn required_fields(entry: &ToolEntry) -> Vec<&str> {
    entry
        .spec
        .parameters
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn argument_problem(entry: &ToolEntry, call: &ToolCall) -> Option<ArgProblem> {
    if let Some(why) = call.arguments_error() {
        return Some(ArgProblem::Unreadable(why));
    }
    required_fields(entry)
        .into_iter()
        .find(|field| call.arguments.get(*field).is_none_or(Value::is_null))
        .map(|field| ArgProblem::Missing(field.to_owned()))
}

/// A result before it's wrapped and cut for the model.
struct Raw {
    text: String,
    /// Wrap as data: (tag, attributes).
    wrap: Option<(&'static str, String)>,
    error: bool,
    /// Why it can't be trusted, in the hub's words.
    why: Option<String>,
    /// A browser result: the page it's about.
    page: Option<pages::TabPage>,
    /// Outside text even from a trusted tool (another bot's words).
    taints: bool,
    /// A browser result's page snapshot, parsed.
    snapshot: Option<Snapshot>,
}

impl Raw {
    fn error(text: impl std::fmt::Display) -> Self {
        Self {
            text: format!("[tool error] {}", short_error(&text.to_string())),
            wrap: None,
            error: true,
            why: None,
            page: None,
            taints: false,
            snapshot: None,
        }
    }

    fn plain(text: String) -> Self {
        Self {
            text,
            wrap: None,
            error: false,
            why: None,
            page: None,
            taints: false,
            snapshot: None,
        }
    }
}

/// A tool error kept short: the first line, `…`, and the last 3 lines
/// (a stack trace keeps its message and loses its frames).
#[must_use]
pub fn short_error(text: &str) -> String {
    if text.chars().count() <= MAX_ERROR {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    let first: String = lines
        .first()
        .copied()
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    let tail: Vec<String> = lines
        .iter()
        .skip(1)
        .rev()
        .take(3)
        .rev()
        .map(|l| l.chars().take(90).collect())
        .collect();
    let mut out = first;
    out.push_str("\n…");
    for t in tail {
        out.push('\n');
        out.push_str(&t);
    }
    out.chars().take(MAX_ERROR).collect()
}

async fn host(call: &ToolCall, env: &ToolEnv<'_>) -> Raw {
    let name = call
        .name
        .strip_prefix(HOST_TOOL_PREFIX)
        .unwrap_or(&call.name)
        .to_owned();
    let (reply, wait) = Reply::<ToolReply>::channel();
    let _ = env
        .events
        .send(EngineEvent::HostTool(HostToolCall {
            name,
            args: call.arguments.clone(),
            reply,
        }))
        .await;
    let reply = tokio::select! {
        r = wait => r,
        () = env.cancel.cancelled() => return Raw::error("Stopped."),
    };
    if !reply.ok {
        // Marked like every other failed tool, so models can tell.
        return Raw::error(reply.text);
    }
    // The hub writes these results, but it wraps other bots' words in
    // data blocks (escaped, so only the hub can open one): those are
    // outside text for this window.
    let other_bot = reply.text.contains("<bot_reply ") || reply.text.contains("<bot_purpose>");
    let mut raw = Raw::plain(reply.text);
    raw.taints = other_bot;
    raw.why = Some(if other_bot {
        "another bot's reply".to_owned()
    } else {
        format!("a tool result ({})", short_tool(&call.name))
    });
    raw
}

async fn browser(call: &ToolCall, env: &ToolEnv<'_>) -> Raw {
    let Some(tools) = env.browser else {
        return Raw::error("This bot has no browser right now.");
    };
    let name = call.name.strip_prefix(BROWSER_PREFIX).unwrap_or(&call.name);
    let out = tokio::select! {
        r = tools.call(name, call.arguments.clone()) => r,
        () = env.cancel.cancelled() => return Raw::error("Stopped."),
    };
    match out {
        Ok(out) if out.is_error => Raw::error(out.text),
        Ok(out) => {
            let mut text = out.text;
            if out.images > 0 {
                text.push_str(
                    "\n(The image isn't shown here. Use take_snapshot to read the page.)",
                );
            }
            let page = {
                let mut tabs = env.pages.lock().unwrap_or_else(PoisonError::into_inner);
                tabs.learn(&text);
                tabs.current()
            };
            let url = page.as_ref().map(|p| p.url.clone()).unwrap_or_default();
            // A page can be megabytes: parsed off the async threads.
            let strict = Arc::clone(&env.strictness);
            let (text, snapshot) = tokio::task::spawn_blocking(move || {
                let snapshot = Snapshot::parse(&text, strict.as_ref());
                (text, snapshot)
            })
            .await
            .unwrap_or_else(|_| (String::new(), None));
            Raw {
                text,
                snapshot,
                wrap: Some(("web_page", format!(" url=\"{}\"", attr(&url)))),
                error: false,
                why: Some(if url.is_empty() {
                    "a web page".to_owned()
                } else {
                    format!("a web page ({url})")
                }),
                page,
                taints: false,
            }
        }
        Err(err) => Raw::error(err),
    }
}

async fn file(call: &ToolCall, env: &ToolEnv<'_>) -> Raw {
    let name = call.name.clone();
    let args = call.arguments.clone();
    let work = env.work_dir.to_path_buf();
    let run = tokio::task::spawn_blocking(move || files::run(&name, &args, &work));
    let done = tokio::select! {
        r = run => r,
        () = env.cancel.cancelled() => return Raw::error("Stopped."),
    };
    match done {
        Ok(Ok(result)) => {
            let (wrap, why) = match (call.name.as_str(), result.read_path) {
                ("Read", Some(path)) => (
                    Some(("file", format!(" path=\"{}\"", attr(&path)))),
                    Some(format!("a file ({path})")),
                ),
                ("Grep" | "Glob", _) => (
                    Some(("file", " search=\"results\"".to_owned())),
                    Some("files in the work folder".to_owned()),
                ),
                _ => (None, Some(format!("a file tool's result ({})", call.name))),
            };
            Raw {
                text: result.text,
                wrap,
                error: false,
                why,
                page: None,
                taints: false,
                snapshot: None,
            }
        }
        Ok(Err(err)) => Raw::error(err),
        Err(err) => Raw::error(format!("the file tool failed: {err}")),
    }
}

/// Wraps a result for the model: a page snapshot is stored and shown as
/// the page, a map or a diff; any other big result is stored and shown in
/// part; the rest as it is.
fn finish(mut raw: Raw, tool: &str, untrusted: bool, env: &ToolEnv<'_>) -> ToolOutcome {
    let trust_why = if untrusted { raw.why.clone() } else { None };
    let mut meta = EntryMeta {
        kind: if raw.error {
            EntryKind::Error
        } else {
            EntryKind::Normal
        },
        tool: Some(tool.to_owned()),
        trust_why: trust_why.clone(),
        page_url: raw
            .page
            .as_ref()
            .map(|p| p.url.clone())
            .filter(|u| !u.is_empty()),
        ..EntryMeta::default()
    };
    if raw.error {
        return ToolOutcome {
            text_for_hub: raw.text.clone(),
            for_model: vec![Part::Text(raw.text)],
            untrusted,
            handle: None,
            error: true,
            meta,
        };
    }
    let (shown, handle) = match raw.snapshot.take() {
        Some(snapshot) => page_view(&raw, tool, &snapshot, trust_why.clone(), env, &mut meta),
        None => big_text(&raw, tool, trust_why, env, &mut meta),
    };
    let for_model = match &raw.wrap {
        Some((tag, attrs)) => {
            let attrs = match &handle {
                Some(h) => format!("{attrs} saved=\"{h}\""),
                None => attrs.clone(),
            };
            let (head, body) = shown;
            format!("{head}{}", data_block(tag, &attrs, &body))
        }
        None => format!("{}{}", shown.0, shown.1),
    };
    ToolOutcome {
        text_for_hub: raw.text,
        for_model: vec![Part::Text(for_model)],
        untrusted,
        handle,
        error: false,
        meta,
    }
}

/// A snapshot: stored whole, shown as the trimmed page (small pages), what
/// changed since the model's last look at this tab, or a map.
fn page_view(
    raw: &Raw,
    tool: &str,
    snapshot: &Snapshot,
    tainted_by: Option<String>,
    env: &ToolEnv<'_>,
    meta: &mut EntryMeta,
) -> ((String, String), Option<String>) {
    let tab = raw.page.clone().unwrap_or_default();
    let url = if snapshot.url().is_empty() {
        tab.url.clone()
    } else {
        snapshot.url().to_owned()
    };
    let title = if snapshot.title().is_empty() {
        tab.title.clone()
    } else {
        snapshot.title().to_owned()
    };
    let stored = env.results.put(&NewResult {
        turn: env.turn,
        step: env.step,
        tool,
        kind: ResultKind::Page,
        page_url: Some(url.clone()).filter(|u| !u.is_empty()),
        page_title: Some(title.clone()).filter(|t| !t.is_empty()),
        tainted_by,
        text: &raw.text,
    });
    let cap = env.budget.inline_cap();
    let Ok(row) = stored else {
        // Not stored: the trimmed page within the cap, and no handle.
        tracing::warn!("couldn't store a page snapshot");
        let (body, _) = snapshot.trim().render_within(cap, "the page");
        return ((String::new(), with_pages(snapshot, &body)), None);
    };
    let handle = row.handle.clone();
    let trimmed = snapshot.trim().with_handle(&handle);
    let (key, baseline) = {
        let tabs = env.pages.lock().unwrap_or_else(PoisonError::into_inner);
        let key = tabs.tab_key(&url);
        let baseline = tabs
            .baseline(&key)
            .filter(|b| (env.visible)(&b.handle))
            .map(|b| Baseline {
                handle: b.handle.clone(),
                origin_path: b.origin_path.clone(),
                lines: b.lines.clone(),
            });
        (key, baseline)
    };
    let place = short_url(&url);
    let size = tokens_in_words(row.est_tokens);
    let what = short_tool(tool);
    let diff = baseline
        .as_ref()
        .and_then(|b| snap::diff(b, &trimmed, snapshot, &handle, cap));
    let is_diff = diff.is_some();
    let (head, body) = if let Some(diff) = diff {
        (
            format!(
                "[stored as {handle}: {what} of {place}, {size}. Showing what changed since the \
                 last look; find_in_page(page: \"{handle}\", …) or read_section for the rest.]\n"
            ),
            diff,
        )
    } else if trimmed.est_tokens() <= cap {
        (
            format!("[stored as {handle}: {what} of {place}, {size}. Showing the whole page.]\n"),
            with_pages(snapshot, &trimmed.render()),
        )
    } else {
        (
            format!(
                "[stored as {handle}: {what} of {place}, {size}. It's big, so here is a map: look \
                 inside with find_in_page, read_section or ask_page.]\n"
            ),
            trimmed.map(snapshot, &handle),
        )
    };
    {
        let mut tabs = env.pages.lock().unwrap_or_else(PoisonError::into_inner);
        tabs.latest_is(&handle);
        // A diff isn't a page the model saw: the next one compares with
        // the last page it was shown whole (or as a map).
        if !is_diff {
            tabs.shown(
                &key,
                Baseline {
                    handle: handle.clone(),
                    origin_path: snap::origin_path(&url),
                    lines: trimmed.lines(),
                },
            );
        }
    }
    meta.diff = is_diff;
    meta.page_key = Some(key);
    meta.page_url = Some(url).filter(|u| !u.is_empty());
    meta.page_title = Some(title).filter(|t| !t.is_empty());
    meta.full_tokens = Some(row.est_tokens);
    ((head, body), Some(handle))
}

/// The page list (tab ids are needed by the browser tools), then `body`.
fn with_pages(snapshot: &Snapshot, body: &str) -> String {
    if snapshot.pages().is_empty() {
        return body.to_owned();
    }
    let mut out = String::from("## Pages\n");
    for p in snapshot.pages() {
        let label = if p.title.is_empty() {
            p.url.clone()
        } else {
            format!("{} ({})", p.title, p.url)
        };
        let _ = writeln!(
            out,
            "{}: {label}{}",
            p.id,
            if p.selected { " [selected]" } else { "" }
        );
    }
    out.push_str("## Page\n");
    out.push_str(body);
    out
}

/// Any other result: stored when it's big enough to be cleared later, and
/// shown in part when over the inline cap.
fn big_text(
    raw: &Raw,
    tool: &str,
    tainted_by: Option<String>,
    env: &ToolEnv<'_>,
    meta: &mut EntryMeta,
) -> ((String, String), Option<String>) {
    let tokens = est(&raw.text);
    if tokens <= STORE_OVER {
        return ((String::new(), raw.text.clone()), None);
    }
    let stored = env.results.put(&NewResult {
        turn: env.turn,
        step: env.step,
        tool,
        kind: ResultKind::Text,
        page_url: meta.page_url.clone(),
        page_title: None,
        tainted_by,
        text: &raw.text,
    });
    let cap = env.budget.inline_cap();
    let Ok(row) = stored else {
        tracing::warn!("couldn't store a big tool result");
        return ((String::new(), control::cut_lines(&raw.text, cap)), None);
    };
    meta.full_tokens = Some(row.est_tokens);
    if tokens <= cap {
        return ((String::new(), raw.text.clone()), Some(row.handle));
    }
    let head = head_of(&raw.text, cap / 2);
    let note = format!(
        "[stored as {}: {}, {}. Showing the start; read_result(\"{}\", find: \"…\") or from_line \
         for the rest.]\n",
        row.handle,
        short_tool(tool),
        tokens_in_words(row.est_tokens),
        row.handle
    );
    ((note, head), Some(row.handle))
}

/// The start of `text` up to `tokens`, cut at a line break.
fn head_of(text: &str, tokens: u64) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if est(&out) + est(line) + 1 > tokens {
            if out.is_empty() {
                out = line
                    .chars()
                    .take(usize::try_from(tokens * 3).unwrap_or(0))
                    .collect();
            }
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push('…');
    out
}

/// Text safe inside a double-quoted attribute.
pub(crate) fn attr(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
