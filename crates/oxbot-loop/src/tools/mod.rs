//! The tools a loop session offers, in a fixed order (oxbot's own tools,
//! browser tools sorted by name, file tools, control tools), with the same
//! names as on Claude. [`ToolRegistry::run`] needs a [`Permit`]: only the
//! gate makes one.

pub mod browser;
pub mod control;
pub mod files;
pub mod mcp;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use oxbot_engine::text::data_block;
use oxbot_engine::trust::{HOST_TOOL_PREFIX, is_trusted_tool};
use oxbot_engine::{EngineEvent, HostToolCall, Reply, ToolReply};
use oxbot_llm::{Part, ToolCall, ToolSpec};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::gate::Permit;
use browser::{BROWSER_PREFIX, BrowserTools, SIDE_EFFECT_TOOLS};

/// Results longer than this (characters) are saved to a file.
pub const SPILL_OVER: usize = 20_000;
/// How much of a saved result the model sees.
pub const SPILL_SHOWN: usize = 12_000;
/// Where saved results go, inside `work/`.
pub const RESULTS_DIR: &str = ".oxbot/results";

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
    /// The loop's own (`ask_user`).
    Control,
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
    /// Where a big result was saved, under `work/`.
    pub handle: Option<String>,
    /// The tool failed.
    pub error: bool,
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
        }
    }
}

/// What running a tool needs from the session.
#[derive(Debug)]
pub struct ToolEnv<'a> {
    /// The bot's `work/` folder.
    pub work_dir: &'a Path,
    /// Where host tool calls go.
    pub events: &'a mpsc::Sender<EngineEvent>,
    /// The browser tool, if the session has one.
    pub browser: Option<&'a Arc<dyn BrowserTools>>,
    /// Stops the call.
    pub cancel: &'a CancellationToken,
    /// A name for a saved big result, e.g. `3-2-0`.
    pub result_name: String,
    /// The page the browser last said was selected.
    pub page: &'a Mutex<Option<String>>,
}

/// A session's tools.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    entries: Vec<ToolEntry>,
}

impl ToolRegistry {
    /// The tools: `host` (oxbot's own, as given), `browser` (already
    /// prefixed and sorted), the file tools, and `ask_user`.
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
        entries.push(ToolEntry {
            spec: control::ask_user_spec(),
            kind: ToolKind::Control,
            side_effect: false,
        });
        Self { entries }
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
            return Err(format!(
                "There is no tool called \"{}\". Use one of the tools you were given.",
                shorten(&call.name, 80)
            ));
        };
        if let Some(why) = call.arguments_error() {
            return Err(format!(
                "Your call to {} couldn't be read ({why}). You sent: {}",
                call.name,
                shorten(&call.raw, 500)
            ));
        }
        let required = entry
            .spec
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for field in required {
            if call.arguments.get(field).is_none_or(Value::is_null) {
                return Err(format!("{} needs {field}.", call.name));
            }
        }
        Ok(entry)
    }

    /// Runs a call the gate allowed.
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
            ToolKind::Control => Raw {
                text: "Asked.".to_owned(),
                wrap: None,
                error: false,
            },
        };
        finish(raw, untrusted, env).await
    }
}

/// A result before it's wrapped and cut for the model.
struct Raw {
    text: String,
    /// Wrap as data: (tag, attributes).
    wrap: Option<(&'static str, String)>,
    error: bool,
}

impl Raw {
    fn error(text: impl std::fmt::Display) -> Self {
        Self {
            text: format!("[tool error] {text}"),
            wrap: None,
            error: true,
        }
    }
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
    Raw {
        text: reply.text,
        wrap: None,
        error: false,
    }
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
            let page = selected_url(&text);
            let mut known = env.page.lock().unwrap_or_else(PoisonError::into_inner);
            if page.is_some() {
                known.clone_from(&page);
            }
            let url = page.or_else(|| known.clone()).unwrap_or_default();
            Raw {
                text,
                wrap: Some(("web_page", format!(" url=\"{}\"", attr(&url)))),
                error: false,
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
            let wrap = match (call.name.as_str(), result.read_path) {
                ("Read", Some(path)) => Some(("file", format!(" path=\"{}\"", attr(&path)))),
                ("Grep" | "Glob", _) => Some(("file", " search=\"results\"".to_owned())),
                _ => None,
            };
            Raw {
                text: result.text,
                wrap,
                error: false,
            }
        }
        Ok(Err(err)) => Raw::error(err),
        Err(err) => Raw::error(format!("the file tool failed: {err}")),
    }
}

/// Wraps and, for big results, saves the whole text and cuts it.
async fn finish(raw: Raw, untrusted: bool, env: &ToolEnv<'_>) -> ToolOutcome {
    let mut shown = raw.text.clone();
    let mut handle = None;
    if raw.text.chars().count() > SPILL_OVER {
        let rel = format!("{RESULTS_DIR}/{}.txt", env.result_name);
        let path = env.work_dir.join(&rel);
        let text = raw.text.clone();
        let saved = tokio::task::spawn_blocking(move || save(&path, &text))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let head: String = raw.text.chars().take(SPILL_SHOWN).collect();
        shown = match saved {
            Ok(()) => {
                handle = Some(rel.clone());
                format!(
                    "{head}\n(… the full result is saved as {rel}; Read it with offset to see more)"
                )
            }
            Err(err) => {
                tracing::warn!(%err, "couldn't save a big tool result");
                format!("{head}\n(… the rest was left out)")
            }
        };
    }
    let for_model = match &raw.wrap {
        Some((tag, attrs)) => data_block(tag, attrs, &shown),
        None => shown,
    };
    ToolOutcome {
        text_for_hub: raw.text,
        for_model: vec![Part::Text(for_model)],
        untrusted,
        handle,
        error: raw.error,
    }
}

fn save(path: &PathBuf, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// The selected page's address from a browser result's page list
/// (`N: Title (url) [selected]`).
fn selected_url(text: &str) -> Option<String> {
    let mut in_pages = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            in_pages = line.trim_end() == "## Pages";
            continue;
        }
        if in_pages && let Some(label) = line.trim().strip_suffix(" [selected]") {
            let inner = label.strip_suffix(')')?;
            return inner.rsplit_once(" (").map(|(_, url)| url.to_owned());
        }
    }
    None
}

/// Text safe inside a double-quoted attribute.
fn attr(text: &str) -> String {
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
mod tests {
    use super::*;
    use serde_json::json;

    fn registry() -> ToolRegistry {
        ToolRegistry::new(
            vec![ToolSpec {
                name: "mcp__oxbot__give_task".into(),
                description: String::new(),
                parameters: json!({"type": "object", "required": ["bot", "task"]}),
            }],
            browser::model_specs(
                vec![
                    ToolSpec {
                        name: "take_snapshot".into(),
                        description: String::new(),
                        parameters: json!({"type": "object"}),
                    },
                    ToolSpec {
                        name: "click".into(),
                        description: String::new(),
                        parameters: json!({"type": "object", "required": ["uid"]}),
                    },
                ],
                &["click".to_owned(), "take_snapshot".to_owned()],
            ),
        )
    }

    #[test]
    fn order_is_fixed_and_side_effects_known() {
        let r = registry();
        let names: Vec<String> = r.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "mcp__oxbot__give_task",
                "mcp__chrome-devtools__click",
                "mcp__chrome-devtools__take_snapshot",
                "Read",
                "Write",
                "Edit",
                "Glob",
                "Grep",
                "mcp__oxbot__ask_user"
            ]
        );
        assert!(r.get("mcp__oxbot__give_task").unwrap().side_effect);
        assert!(r.get("mcp__chrome-devtools__click").unwrap().side_effect);
        assert!(
            !r.get("mcp__chrome-devtools__take_snapshot")
                .unwrap()
                .side_effect
        );
        assert!(r.get("Write").unwrap().side_effect);
        assert!(!r.get("Read").unwrap().side_effect);
    }

    #[test]
    fn bad_calls_are_explained() {
        let r = registry();
        let e = r
            .validate(&ToolCall::new("1", "Bash", json!({})))
            .unwrap_err();
        assert!(e.contains("no tool called \"Bash\""), "{e}");
        let e = r
            .validate(&ToolCall::from_raw(
                "1",
                "mcp__chrome-devtools__click",
                "{uid: 1",
            ))
            .unwrap_err();
        assert!(
            e.contains("couldn't be read") && e.contains("{uid: 1"),
            "{e}"
        );
        let e = r
            .validate(&ToolCall::new(
                "1",
                "mcp__chrome-devtools__click",
                json!({}),
            ))
            .unwrap_err();
        assert_eq!(e, "mcp__chrome-devtools__click needs uid.");
        assert!(
            r.validate(&ToolCall::new(
                "1",
                "mcp__chrome-devtools__click",
                json!({"uid": "1_2"})
            ))
            .is_ok()
        );
    }

    #[test]
    fn the_selected_page_is_read_from_the_page_list() {
        let text = "## Pages\n1: Home (https://a.example/) \n2: Jobs (https://b.example/jobs) [selected]\n## Snapshot\nx";
        assert_eq!(
            selected_url(text).as_deref(),
            Some("https://b.example/jobs")
        );
        assert_eq!(selected_url("## Snapshot\n1: x (y) [selected]"), None);
        assert_eq!(attr("a\"<b&\n"), "a&quot;&lt;b&amp;");
    }
}
