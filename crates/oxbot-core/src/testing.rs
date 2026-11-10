//! Test helpers (feature `test-bots`): hubs whose bots run on Codex (the
//! real `codex` program, when this Mac has it: tests skip, saying why,
//! when it hasn't) against a stand-in model, with a scripted browser.
//!
//! [`FakeBots`] acts like a bot that asks before every tool and then says
//! what oxbot answered, so tests can read the outcome from the reply:
//! `behavior=allow` or `behavior=deny` for a browser or file tool,
//! `signed_in=true|false` for a sign-in, `tool_ok=true|false <text>` for
//! oxbot's other tools, `ok` with no tool. For each person's message it
//! reads a file (if [`FakeBots::read_file`]), takes a page snapshot (unless
//! [`FakeBots::no_page`]), then calls the message's tool (see [`Tool`]).
//! The plan answers Codex's Responses requests through oxbot's relay
//! ([`FakeBots::upstream`]), and the chat requests of oxbot's own helpers
//! (a page question: [`FakeBots::model`]).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use oxbot_browser::{Tools, ToolsConfig};
use oxbot_llm::{LlmError, RawAnswer, Role, ScriptedModel, Step, ToolCall};
use oxbot_loop::tools::browser::{SameBrowser, ScriptedBrowser};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{Bot, BotId, CodexParts, HubConfig, LoopParts};

/// The page every fake bot reads: a job form with an upload, a post
/// button and a password box.
pub const SNAPSHOT: &str = "## Pages\n1: Jobs (https://jobs.example.com/apply) [selected]\n## Latest page snapshot\nuid=1_1 RootWebArea \"Jobs\"\n  uid=1_2 button \"Upload CV\"\n  uid=1_3 button \"Post\"\n  uid=1_4 textbox \"Password\"";

/// The model every fake bot runs on.
pub const MODEL: &str = "m:free";

/// `OpenRouter`'s model list with [`MODEL`] alone, as the hub keeps it.
#[must_use]
pub fn catalog() -> String {
    json!({"models": [{"id": MODEL, "name": "M", "context_length": 128_000,
        "max_completion": null, "prompt_usd_per_token": 0.0, "completion_usd_per_token": 0.0,
        "tools": true, "images_in": false, "text_io": true, "free": true}]})
    .to_string()
}

/// Browser tools that look installed, with no Chrome: the rules run as
/// they would for real, and the scripted browser answers the calls.
///
/// # Panics
/// If the folders can't be made.
#[must_use]
pub fn tools(home: &Path) -> Arc<Tools> {
    let mut tools = ToolsConfig::in_home(home);
    tools.chrome = PathBuf::from("/no/chrome");
    let package = tools.tools_dir.join("node_modules/chrome-devtools-mcp");
    std::fs::create_dir_all(&package).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(package.join("package.json"), r#"{"version":"1.10.1"}"#)
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(tools.tools_dir.join("node_modules/.bin"))
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(tools.mcp_program(), "").unwrap_or_else(|e| panic!("{e}"));
    Tools::new(tools)
}

/// What a fake bot does with one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    /// Upload `cv.pdf` from its folder (the default).
    Upload,
    /// Click the page's "Post" button.
    Post,
    /// Click the page's "Password" box and type into it.
    Password,
    /// Ask the person to sign in.
    SignIn,
    /// `create_bot` ("Mail Checker").
    CreateBot,
    /// `give_task` (`say hi`) to [`FakeBots::target`].
    Task,
    /// `list_bots`.
    List,
    /// `read_reply` of [`FakeBots::target`].
    Read,
    /// `save_report` with [`FakeBots::report`].
    Report,
    /// Write `notes.md`, saying so first.
    Note,
    /// No tool: reply `ok`.
    None,
    /// Any other call, by its full name, with these arguments (oxbot's own
    /// tools end like the team tools: `tool_ok=true|false <text>`).
    Call(String, Value),
}

impl Tool {
    /// A tool by the word tests use for it (`upload`, `post`, `signin`, …).
    ///
    /// # Panics
    /// For a word no tool has.
    #[must_use]
    pub fn named(word: &str) -> Self {
        match word {
            "upload" => Self::Upload,
            "post" => Self::Post,
            "password" => Self::Password,
            "signin" => Self::SignIn,
            "createbot" => Self::CreateBot,
            "task" => Self::Task,
            "list" => Self::List,
            "read" => Self::Read,
            "report" => Self::Report,
            "note" => Self::Note,
            "none" => Self::None,
            other => panic!("no fake tool called {other}"),
        }
    }
}

/// One step a fake bot takes: a call (and what it says with it).
#[derive(Debug, Clone, PartialEq)]
struct Call {
    name: String,
    args: Value,
    say: String,
}

/// A fake bot's plan for one message, and how it ends.
#[derive(Debug, Clone)]
pub struct Plan {
    calls: Vec<Call>,
    tool: Tool,
}

/// A bot that asks first, then says what oxbot answered. See the module.
#[derive(Debug, Default)]
pub struct FakeBots {
    /// The tool for every message, unless [`FakeBots::tools`] names one.
    pub tool: Option<Tool>,
    /// The tool for each message in turn (the 1st message the 1st).
    pub tools: Vec<Tool>,
    /// The same, for the bot named `helper`.
    pub helper_tools: Vec<Tool>,
    /// The bot `give_task` and `read_reply` name (default `helper`).
    pub target: Option<String>,
    /// `create_bot` asks for a private bot.
    pub private: bool,
    /// No page snapshot first.
    pub no_page: bool,
    /// Only the first message (across hubs sharing this file) reads the page.
    pub page_once: Option<PathBuf>,
    /// Read `notes.txt` in its folder first.
    pub read_file: bool,
    /// `save_report`'s arguments (default: one source, the page it read).
    pub report: Option<Value>,
    /// Where the bots' folders are (for the file it uploads).
    pub bots_dir: PathBuf,
    /// The page the scripted browser shows (default [`SNAPSHOT`]).
    pub page: Option<String>,
    /// The browser tools that ran, in order.
    pub browser_calls: Calls,
    /// Each person's message as the bots got it, in order (see
    /// [`FakeBots::told`]).
    pub messages: Calls,
    /// Each tool result the bots read, in order (see [`FakeBots::heard`]).
    pub results: Calls,
    /// What a bot with no tool ([`Tool::None`]) answers, instead of `ok`.
    pub answer: Option<String>,
    /// What it answers when oxbot sends its answer back to be checked,
    /// instead of the same answer.
    pub checked: Option<String>,
    /// oxbot's notes the bots got (a check, say), in order.
    pub notes: Calls,
    /// The plan each (bot, message) got, so every request of a turn
    /// follows the same one (made as they're needed; leave it default).
    pub plans: Plans,
}

/// The browser tools a [`FakeBots`]'s browser ran (see
/// [`FakeBots::ran`]).
#[derive(Debug, Default)]
pub struct Calls(Mutex<Vec<String>>);

/// The plans a [`FakeBots`] has made so far.
#[derive(Debug, Default)]
pub struct Plans(Mutex<HashMap<(String, usize), Plan>>);

/// A conversation as a request carries it: the bot's instructions, then
/// each message with its role, text and calls.
#[derive(Debug, Default)]
struct Conversation {
    system: String,
    messages: Vec<Turn>,
}

#[derive(Debug)]
struct Turn {
    role: Role,
    text: String,
    calls: usize,
}

/// What the fake answers: text, and maybe one call (its full name).
#[derive(Debug, Clone, PartialEq)]
struct Answer {
    text: String,
    call: Option<(String, Value)>,
}

/// How oxbot's own notes in a person's place start.
const NOTE: &str = "[oxbot]";
impl FakeBots {
    /// A fake for bots in `bots_dir` that takes `tool` for every message.
    #[must_use]
    pub fn new(bots_dir: impl Into<PathBuf>, tool: Tool) -> Self {
        Self {
            tool: Some(tool),
            bots_dir: bots_dir.into(),
            ..Self::default()
        }
    }

    /// The browser tools that ran so far, by their own names (`click`).
    #[must_use]
    pub fn ran(&self) -> Vec<String> {
        self.browser_calls
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The last person's message a bot got, as it got it (without the
    /// date line Codex's first request ends with).
    #[must_use]
    pub fn told(&self) -> Option<String> {
        let text = self
            .messages
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .cloned()?;
        Some(match text.rfind("\n\n[oxbot] Today is ") {
            Some(at) => text[..at].to_owned(),
            None => text,
        })
    }

    /// Every tool result the bots read so far, in order (each once).
    #[must_use]
    pub fn heard(&self) -> Vec<String> {
        self.results
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The helpers' model (a page question), answering from this fake.
    #[must_use]
    pub fn model(self: &Arc<Self>) -> Arc<ScriptedModel> {
        let fake = Arc::clone(self);
        Arc::new(ScriptedModel::new().with_responder(move |req| {
            let conv = Conversation {
                system: req
                    .messages
                    .first()
                    .filter(|m| m.role == Role::System)
                    .map(oxbot_llm::Message::text)
                    .unwrap_or_default(),
                messages: req
                    .messages
                    .iter()
                    .filter(|m| m.role != Role::System)
                    .map(|m| Turn {
                        role: m.role,
                        text: m.text(),
                        calls: m.tool_calls.len(),
                    })
                    .collect(),
            };
            let answer = fake.answer(&conv);
            match answer.call {
                Some((name, args)) => Step::Answer {
                    text: answer.text,
                    tool_calls: vec![ToolCall::new("", name, args)],
                    usage: oxbot_llm::CallUsage::default(),
                    model: None,
                },
                None => Step::text(answer.text),
            }
        }))
    }

    /// `OpenRouter`'s Responses API as Codex reaches it through oxbot's
    /// relay, answering from this fake.
    #[must_use]
    pub fn upstream(self: &Arc<Self>) -> Arc<FakeResponses> {
        Arc::new(FakeResponses {
            fake: Arc::clone(self),
            bodies: Mutex::new(Vec::new()),
        })
    }

    /// The tool for `bot`'s `n`th message.
    fn tool_for(&self, bot: &str, n: usize) -> Tool {
        let list = if bot == "helper" && !self.helper_tools.is_empty() {
            &self.helper_tools
        } else {
            &self.tools
        };
        list.get(n.saturating_sub(1))
            .cloned()
            .or_else(|| self.tool.clone())
            .unwrap_or(Tool::Upload)
    }

    /// The work folder of the bot called `name` (renamed bots too).
    fn work_of(&self, name: &str) -> PathBuf {
        std::fs::read_dir(&self.bots_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Bot::load(&e.path()).ok())
            .find(|b| same_name(name, &b.name))
            .map_or_else(|| self.bots_dir.join(name).join("work"), |b| b.work_dir())
    }

    /// `bot`'s plan for its `n`th message.
    fn plan(&self, bot: &str, n: usize) -> Plan {
        let mut plans = self.plans.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(plan) = plans.get(&(bot.to_owned(), n)) {
            return plan.clone();
        }
        let tool = self.tool_for(bot, n);
        let mut calls = Vec::new();
        let call = |name: &str, args: Value| Call {
            name: name.to_owned(),
            args,
            say: String::new(),
        };
        if self.read_file {
            calls.push(call("Read", json!({"file_path": "notes.txt"})));
        }
        let page = !self.no_page
            && tool != Tool::Note
            && self.page_once.as_ref().is_none_or(|once| {
                if once.exists() {
                    false
                } else {
                    std::fs::write(once, "").is_ok()
                }
            });
        if page {
            calls.push(call("mcp__chrome-devtools__take_snapshot", json!({})));
        }
        let target = self.target.clone().unwrap_or_else(|| "helper".to_owned());
        let work = self.work_of(bot);
        let report = self.report.clone().unwrap_or_else(|| {
            json!({"title": "Jobs", "summary": "One job fits.",
                "markdown": "The apply page has one job [1].",
                "sources": [{"url": "https://jobs.example.com/apply", "title": "Jobs"}]})
        });
        let mut create = json!({"name": "Mail Checker", "instructions": "Check my mail."});
        if self.private {
            create["private"] = json!(true);
        }
        match &tool {
            Tool::Upload => calls.push(call(
                "mcp__chrome-devtools__upload_file",
                json!({"uid": "1_2", "filePaths": [work.join("cv.pdf").display().to_string()]}),
            )),
            Tool::Post => calls.push(call("mcp__chrome-devtools__click", json!({"uid": "1_3"}))),
            Tool::Password => calls.push(call(
                "mcp__chrome-devtools__fill",
                json!({"uid": "1_4", "value": "hunter2"}),
            )),
            Tool::SignIn => calls.push(call(
                "mcp__oxbot__request_sign_in",
                json!({"url": "https://mail.example.com/login", "why": "to read your mail"}),
            )),
            Tool::CreateBot => calls.push(call("mcp__oxbot__create_bot", create)),
            Tool::Task => calls.push(call(
                "mcp__oxbot__give_task",
                json!({"bot": target, "message": "say hi"}),
            )),
            Tool::List => calls.push(call("mcp__oxbot__list_bots", json!({}))),
            Tool::Read => calls.push(call("mcp__oxbot__read_reply", json!({"bot": target}))),
            Tool::Report => calls.push(call("mcp__oxbot__save_report", report)),
            Tool::Note => calls.push(Call {
                say: "I'll save that note.".to_owned(),
                ..call(
                    "Write",
                    json!({"file_path": "notes.md", "content": "Rust day one"}),
                )
            }),
            Tool::Call(name, args) => calls.push(call(name, args.clone())),
            Tool::None => {}
        }
        let plan = Plan { calls, tool };
        plans.insert((bot.to_owned(), n), plan.clone());
        plan
    }

    /// The next step for `conv`.
    fn answer(&self, conv: &Conversation) -> Answer {
        let bot = bot_name(&conv.system);
        let persons: Vec<usize> = conv
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::User && !m.text.trim_start().starts_with(NOTE))
            .map(|(i, _)| i)
            .collect();
        let Some(&last) = persons.last() else {
            return Answer {
                text: "ok".to_owned(),
                call: None,
            };
        };
        {
            let mut told = self
                .messages
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let text = &conv.messages[last].text;
            if told.last() != Some(text) {
                told.push(text.clone());
            }
        }
        {
            let mut heard = self
                .results
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for m in conv.messages.iter().filter(|m| m.role == Role::Tool) {
                if !heard.contains(&m.text) {
                    heard.push(m.text.clone());
                }
            }
        }
        // oxbot sent an answer back to be checked: the same answer (or
        // `checked`).
        if let Some(note) = conv
            .messages
            .last()
            .filter(|m| m.role == Role::User && m.text.trim_start().starts_with(NOTE))
            && let Some(said) = conv.messages[last..]
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant && m.calls == 0 && !m.text.is_empty())
        {
            let mut notes = self.notes.0.lock().unwrap_or_else(PoisonError::into_inner);
            if notes.last() != Some(&note.text) {
                notes.push(note.text.clone());
            }
            return Answer {
                text: self.checked.clone().unwrap_or_else(|| said.text.clone()),
                call: None,
            };
        }
        let plan = self.plan(&bot, persons.len());
        let after = &conv.messages[last + 1..];
        let made: usize = after
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .map(|m| m.calls)
            .sum();
        if let Some(next) = plan.calls.get(made) {
            return Answer {
                text: next.say.clone(),
                call: Some((next.name.clone(), next.args.clone())),
            };
        }
        let result = after
            .iter()
            .rev()
            .find(|m| m.role == Role::Tool)
            .map(|m| m.text.clone())
            .unwrap_or_default();
        let text = match (&plan.tool, &self.answer) {
            (Tool::None, Some(answer)) => answer.clone(),
            _ => finish(&plan.tool, &result),
        };
        Answer { text, call: None }
    }

    /// oxbot's notes the bots got so far (a check, say), in order.
    #[must_use]
    pub fn noted(&self) -> Vec<String> {
        self.notes
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Whether `seen` (a name as the model got it, perhaps with stand-ins for
/// personal details: `[person 1]'s Recipes`) is the bot called `name`.
fn same_name(seen: &str, name: &str) -> bool {
    if seen == name {
        return true;
    }
    // Each `[… N]` stands for some text: the rest must match in order.
    let mut parts = Vec::new();
    let mut rest = seen;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        parts.push(&rest[..open]);
        rest = &rest[open + close + 1..];
    }
    if parts.is_empty() {
        return false;
    }
    parts.push(rest);
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    let Some(mut left) = name.strip_prefix(first) else {
        return false;
    };
    for part in &parts[1..parts.len() - 1] {
        match left.find(part) {
            Some(i) => left = &left[i + part.len()..],
            None => return false,
        }
    }
    left.len() > last.len() && left.ends_with(last)
}

/// The bot's name, from its instructions (`You are "Name"`).
fn bot_name(system: &str) -> String {
    system
        .split_once("You are \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(name, _)| name.to_owned())
        .unwrap_or_default()
}

/// What the fake says once its call ran, from the call's result.
fn finish(tool: &Tool, result: &str) -> String {
    let result = result.trim();
    if result.starts_with("[oxbot said no]") {
        return "behavior=deny".to_owned();
    }
    let failed = result.strip_prefix("[tool error]").map(str::trim);
    match tool {
        Tool::None => "ok".to_owned(),
        Tool::Note => "Saved to notes.md.".to_owned(),
        Tool::SignIn => format!("signed_in={}", failed.is_none()),
        Tool::CreateBot | Tool::Task | Tool::List | Tool::Read | Tool::Report => match failed {
            Some(text) => format!("tool_ok=false {text}"),
            None => format!("tool_ok=true {result}"),
        },
        // One of oxbot's own tools (the schedule tools, say): as above.
        Tool::Call(name, _) if name.starts_with("mcp__oxbot__") => match failed {
            Some(text) => format!("tool_ok=false {text}"),
            None => format!("tool_ok=true {result}"),
        },
        Tool::Upload | Tool::Post | Tool::Password | Tool::Call(..) => match failed {
            Some(text) => format!("behavior=error {text}"),
            None => "behavior=allow".to_owned(),
        },
    }
}

/// `OpenRouter`'s Responses API for Codex, from a [`FakeBots`]. Keeps
/// every request body.
#[derive(Debug)]
pub struct FakeResponses {
    fake: Arc<FakeBots>,
    bodies: Mutex<Vec<Value>>,
}

impl FakeResponses {
    /// Every request body so far, in order.
    #[must_use]
    pub fn bodies(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The answer to one Responses request, as its events.
    fn events(&self, req: &Value) -> Vec<Value> {
        let mut conv = Conversation {
            system: req["instructions"].as_str().unwrap_or_default().to_owned(),
            messages: Vec::new(),
        };
        for item in req["input"].as_array().into_iter().flatten() {
            let text = |v: &Value| match v {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            match item["type"].as_str() {
                Some("message") | None => {
                    let role = match item["role"].as_str() {
                        Some("assistant") => Role::Assistant,
                        Some("user") => Role::User,
                        _ => continue,
                    };
                    conv.messages.push(Turn {
                        role,
                        text: text(&item["content"]),
                        calls: 0,
                    });
                }
                Some("function_call") => conv.messages.push(Turn {
                    role: Role::Assistant,
                    text: String::new(),
                    calls: 1,
                }),
                Some("function_call_output") => conv.messages.push(Turn {
                    role: Role::Tool,
                    text: text(&item["output"]),
                    calls: 0,
                }),
                _ => {}
            }
        }
        let answer = self.fake.answer(&conv);
        let n = self
            .bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let mut events =
            vec![json!({"type": "response.created", "response": {"id": format!("r{n}")}})];
        if !answer.text.is_empty() {
            let id = format!("msg_{n}");
            let item = json!({"type": "message", "id": id, "role": "assistant",
                "content": [{"type": "output_text", "text": answer.text}]});
            events.push(json!({"type": "response.output_item.added", "item": {"type": "message", "id": id, "role": "assistant", "content": []}}));
            events.push(
                json!({"type": "response.output_text.delta", "item_id": id, "delta": answer.text}),
            );
            events.push(json!({"type": "response.output_item.done", "item": item}));
        }
        if let Some((name, args)) = answer.call {
            let item = json!({"type": "function_call", "id": format!("fc_{n}"),
                "call_id": format!("call_{n}"), "name": requested_name(req, &name),
                "arguments": args.to_string()});
            events.push(json!({"type": "response.output_item.added", "item": item}));
            events.push(json!({"type": "response.output_item.done", "item": item}));
        }
        events.push(
            json!({"type": "response.completed", "response": {"id": format!("r{n}"),
            "model": MODEL,
            "usage": {"input_tokens": 100, "output_tokens": 5, "total_tokens": 105}}}),
        );
        events
    }
}

/// The request's own name for a planned call: Codex gets oxbot's tools
/// under their own names (`click` for `mcp__chrome-devtools__click`).
fn requested_name(req: &Value, planned: &str) -> String {
    let own = planned.rsplit("__").next().unwrap_or(planned);
    let names: Vec<&str> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .collect();
    if names.contains(&planned) {
        planned.to_owned()
    } else {
        own.to_owned()
    }
}

impl oxbot_relay::Upstream for FakeResponses {
    fn send(
        &self,
        body: Vec<u8>,
        _stream: bool,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RawAnswer, LlmError>> {
        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let events = self.events(&req);
        self.bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(req);
        let mut text = String::new();
        for e in &events {
            let _ = write!(
                text,
                "event: {}\ndata: {e}\n\n",
                e["type"].as_str().unwrap_or("")
            );
        }
        Box::pin(async move {
            let chunks: Vec<Result<Bytes, LlmError>> = vec![Ok(Bytes::from(text))];
            Ok(RawAnswer {
                content_type: "text/event-stream".into(),
                body: futures_util::stream::iter(chunks).boxed(),
            })
        })
    }
}

/// Where `codex` is on this Mac (`OXBOT_CODEX`, else `PATH` and
/// Homebrew's folders), if it's a version oxbot speaks.
pub async fn codex_program(scratch: &Path) -> Option<PathBuf> {
    let configured = std::env::var_os("OXBOT_CODEX").map(PathBuf::from);
    let path = std::env::var("PATH").ok();
    match oxbot_codex::binary::find(configured.as_deref(), path.as_deref(), scratch).await {
        oxbot_codex::Found::Ready(b) => Some(b.path),
        oxbot_codex::Found::Unusable { why, .. } => {
            eprintln!("no Codex on this Mac ({why})");
            None
        }
    }
}

/// The `codex` program a test runs its bots on, if this Mac has Codex
/// 0.148 (`OXBOT_CODEX`, else `PATH` and Homebrew's folders). A test
/// without it skips, saying why:
///
/// ```ignore
/// let Some(codex) = testing::codex(&tmp).await else { return };
/// ```
pub async fn codex(scratch: &Path) -> Option<PathBuf> {
    let found = codex_program(&scratch.join("codex-probe")).await;
    if found.is_none() {
        eprintln!("SKIPPED: this Mac has no Codex 0.148, which every bot runs on");
    }
    found
}

/// The bots `ids` in `bots_dir`, as they were left (or new, named as
/// their ids).
///
/// # Panics
/// If a bot's files can't be written.
#[must_use]
pub fn bots(bots_dir: &Path, ids: &[&str]) -> Vec<Bot> {
    std::fs::create_dir_all(bots_dir).unwrap_or_else(|e| panic!("{e}"));
    ids.iter()
        .map(|id| {
            Bot::load(&bots_dir.join(id)).unwrap_or_else(|_| {
                let id = BotId::new(*id).unwrap_or_else(|e| panic!("{e}"));
                Bot::create(bots_dir, id.clone(), id.as_str(), "").unwrap_or_else(|e| panic!("{e}"))
            })
        })
        .collect()
}

/// Hub settings for bots in `bots_dir` (browser tools under `home`)
/// running on Codex (`codex`, the program; `None`: no Codex, so no bot can
/// take a turn) against `fake`: short waits for cards and sign-ins, and
/// the scripted browser showing [`SNAPSHOT`].
#[must_use]
pub fn config(
    bots_dir: &Path,
    home: &Path,
    fake: &Arc<FakeBots>,
    codex: Option<&Path>,
) -> HubConfig {
    config_with(bots_dir, home, fake.model(), fake, codex)
}

/// Like [`config`], with the helpers' `model` made from `fake` already
/// (to read what it was asked).
#[must_use]
pub fn config_with(
    bots_dir: &Path,
    home: &Path,
    model: Arc<ScriptedModel>,
    fake: &Arc<FakeBots>,
    codex: Option<&Path>,
) -> HubConfig {
    let page = fake.page.clone().unwrap_or_else(|| SNAPSHOT.to_owned());
    let calls = Arc::clone(fake);
    let browser = ScriptedBrowser::new(move |name, _| {
        calls
            .browser_calls
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(name.to_owned());
        oxbot_loop::BrowserOutput {
            text: if name == "take_snapshot" {
                page.clone()
            } else {
                format!("Done: {name}")
            },
            ..oxbot_loop::BrowserOutput::default()
        }
    });
    HubConfig {
        browser: Some(tools(home)),
        approval_timeout: Duration::from_secs(5),
        sign_in_timeout: Duration::from_secs(5),
        openrouter: Some(LoopParts {
            browser: Some(Arc::new(SameBrowser(browser))),
            free_per_minute: 1000,
            ..LoopParts::new(model, None, None)
        }),
        codex: codex.map(|program| CodexParts {
            upstream: fake.upstream(),
            program: Some(program.to_owned()),
            built_in: None,
        }),
        data_dir: home.join("data"),
        ..HubConfig::new(bots_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(system: &str, turns: &[(Role, &str, usize)]) -> Conversation {
        Conversation {
            system: system.to_owned(),
            messages: turns
                .iter()
                .map(|(role, text, calls)| Turn {
                    role: *role,
                    text: (*text).to_owned(),
                    calls: *calls,
                })
                .collect(),
        }
    }

    #[test]
    fn a_fake_bot_reads_the_page_calls_its_tool_and_says_what_oxbot_answered() {
        let fake = FakeBots::new("/bots", Tool::Post);
        let sys = "You are \"poster\", a bot";
        let first = fake.answer(&conv(sys, &[(Role::User, "[Jay] go", 0)]));
        assert_eq!(first.call.unwrap().0, "mcp__chrome-devtools__take_snapshot");
        let second = fake.answer(&conv(
            sys,
            &[
                (Role::User, "[Jay] go", 0),
                (Role::Assistant, "", 1),
                (Role::Tool, SNAPSHOT, 0),
            ],
        ));
        assert_eq!(
            second.call,
            Some((
                "mcp__chrome-devtools__click".to_owned(),
                json!({"uid": "1_3"})
            ))
        );
        let done = |result: &str| {
            fake.answer(&conv(
                sys,
                &[
                    (Role::User, "[Jay] go", 0),
                    (Role::Assistant, "", 1),
                    (Role::Tool, SNAPSHOT, 0),
                    (Role::Assistant, "", 1),
                    (Role::Tool, result, 0),
                ],
            ))
            .text
        };
        assert_eq!(done("Done: click"), "behavior=allow");
        assert_eq!(done("[oxbot said no] the user said no"), "behavior=deny");
        // An answer sent back to be checked comes back the same.
        let checked = fake.answer(&conv(
            sys,
            &[
                (Role::User, "[Jay] go", 0),
                (Role::Assistant, "", 1),
                (Role::Tool, SNAPSHOT, 0),
                (Role::Assistant, "", 1),
                (Role::Tool, "Done", 0),
                (Role::Assistant, "behavior=allow", 0),
                (Role::User, "[oxbot] Before you answer, check", 0),
            ],
        ));
        assert_eq!(checked.text, "behavior=allow");
        assert_eq!(bot_name(sys), "poster");
    }

    #[test]
    fn each_message_takes_its_own_tool_and_helpers_their_own() {
        let fake = FakeBots {
            tools: vec![Tool::List, Tool::Task],
            helper_tools: vec![Tool::None],
            no_page: true,
            ..FakeBots::default()
        };
        let first = fake.answer(&conv("You are \"Assistant\"", &[(Role::User, "a", 0)]));
        assert_eq!(first.call.unwrap().0, "mcp__oxbot__list_bots");
        let second = fake.answer(&conv(
            "You are \"Assistant\"",
            &[
                (Role::User, "a", 0),
                (Role::Assistant, "tool_ok=true", 0),
                (Role::User, "b", 0),
            ],
        ));
        assert_eq!(second.call.unwrap().0, "mcp__oxbot__give_task");
        let helper = fake.answer(&conv("You are \"helper\"", &[(Role::User, "say hi", 0)]));
        assert_eq!((helper.text.as_str(), helper.call), ("ok", None));
        assert_eq!(
            finish(&Tool::Task, "[tool error] busy"),
            "tool_ok=false busy"
        );
        assert_eq!(finish(&Tool::SignIn, "Signed in."), "signed_in=true");
    }

    #[test]
    fn codex_gets_calls_under_the_names_it_was_offered() {
        let req = json!({"tools": [{"type": "function", "name": "click"}, {"type": "function", "name": "Read"}]});
        assert_eq!(requested_name(&req, "mcp__chrome-devtools__click"), "click");
        assert_eq!(requested_name(&req, "Read"), "Read");
        assert_eq!(requested_name(&req, "mcp__oxbot__give_task"), "give_task");
    }
}
