//! The stand-ins' reply scripts (`e2e/claude/*.toml`), shared by the
//! stand-in Claude, the stand-in `OpenRouter` and the stand-in browser tool,
//! so one script drives a test on either engine.
//!
//! A script is a list of replies; the first whose `when` appears in the
//! message (any case) is used, and one without `when` is the fallback:
//!
//! ```toml
//! [[reply]]
//! when = "make a bot"
//! tool = "create_bot"                      # oxbot's own tool, a full mcp__… name, or a file tool (Read)
//! args = { name = "Recipes", instructions = "Find recipes." }
//! say = "Making it…"                        # said before the tool (optional)
//! snapshots = ["## Pages\n1: A (https://a.example.com/) [selected]"]  # pages "read" first
//! after = "Done: {result}"                  # said after it worked
//! denied = "Okay, I won't."                 # said if the user said no
//!
//! [[reply]]
//! when = "half"
//! say = "About half."
//! limits = { five_hour = 0.55, seven_day = 0.2 } # plan usage reported first (Claude)
//! model = "claude-sonnet-4-5-20250929"           # the model it says it is (Claude)
//!
//! [[reply]]
//! when = "busy"
//! say = "Answered by the backup."
//! busy = ["e2e/tool-model:free"]            # these models answer 429 (OpenRouter)
//! data_policy = ["e2e/backup:free"]         # the account's privacy settings rule these out (OpenRouter)
//! usage = { prompt = 1200, completion = 80, cost = 0.0 } # reported usage (OpenRouter)
//! stall_at = 2                              # the first request after 2 tool results waits a minute (OpenRouter)
//!
//! [[reply]]
//! when = "compare lamps"
//! big = { pages = 8, items = 75 }           # big fake pages read first (see crate::pages)
//! tool = "mcp__oxbot__read_result"          # then this, with args
//! args = { handle = "r1", find = "Lamp 1001" }
//! then = [{ tool = "mcp__oxbot__find_in_page", args = { query = "lamp" } }] # more calls after it
//! notes = 7800                              # each call step also says this many characters of notes (OpenRouter)
//! ask_answer = "Lamp 1003 (uid=2_14)."      # what the helper says to ask_page (OpenRouter)
//! checked = "Lamp 1003, checked."           # the answer after oxbot sends one back to be checked (OpenRouter)
//!
//! [[reply]]
//! when = "what's in"
//! read = true                               # open each attached file (see below)
//!
//! [[reply]]
//! say = "Hello from the stand-in."
//! ```
//!
//! With `read`, it opens every file oxbot's note lists, as Claude would with
//! Read, and says what it found in each: the first line of a text file or of
//! the text oxbot made of it, how many still frames a video became, or the
//! kind of a picture or PDF. That shows what oxbot made of a file without a
//! real model.
//!
//! Knobs one stand-in doesn't use are ignored by it: the stand-in Claude
//! ignores `busy`, `data_policy`, `usage` and `stall_at`, the stand-in `OpenRouter` ignores `limits`
//! and `model` (it says which model it routed to).

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

/// A reply script.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Script {
    /// The replies, in order.
    #[serde(default)]
    pub reply: Vec<Reply>,
}

/// One scripted reply.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Reply {
    /// Used when the message contains this (any case); none = fallback.
    pub when: Option<String>,
    /// Said before the tool, or as the whole answer without one.
    pub say: Option<String>,
    /// The tool to call: oxbot's own (`create_bot`) or a full `mcp__…` name.
    pub tool: Option<String>,
    /// The tool's arguments.
    #[serde(default)]
    pub args: toml::Table,
    /// A fake page snapshot to "read" first (for browser tools).
    pub snapshot: Option<String>,
    /// More fake page snapshots to "read" first, in order (after
    /// `snapshot`), e.g. the pages a report cites.
    #[serde(default)]
    pub snapshots: Vec<String>,
    /// Said after the tool worked; `{result}` is its result.
    pub after: Option<String>,
    /// Said if the tool was refused.
    pub denied: Option<String>,
    /// Wait this long before answering (to see "working" states).
    #[serde(default)]
    pub delay_ms: u64,
    /// Open the attached files and say what's in them.
    #[serde(default)]
    pub read: bool,
    /// Plan usage to report before answering, as Claude's `rate_limit_event`.
    pub limits: Option<Limits>,
    /// The model the stand-in Claude says it is (default `fake`).
    pub model: Option<String>,
    /// Models the stand-in `OpenRouter` says are busy (429) for this reply.
    #[serde(default)]
    pub busy: Vec<String>,
    /// Models the stand-in `OpenRouter` rules out for this reply as the
    /// account's privacy settings would (only providers that may train).
    #[serde(default)]
    pub data_policy: Vec<String>,
    /// The usage the stand-in `OpenRouter` reports for each request.
    pub usage: Option<Usage>,
    /// The stand-in `OpenRouter`'s first request for this reply after this
    /// many tool results gets no answer for a minute (once per stand-in),
    /// so a test can restart oxbot in the middle of a turn.
    pub stall_at: Option<usize>,
    /// Reserved for knowledge tests: text the bot is expected to know
    /// (from its notes or the request). No stand-in acts on it yet.
    pub knows: Option<String>,
    /// Said after a practice run's `[test run]` answer (the stand-in
    /// Claude gets it as a refusal) instead of `denied`.
    pub dry: Option<String>,
    /// Big fake pages to read first (after `snapshots`).
    pub big: Option<crate::pages::BigPages>,
    /// More tool calls after `tool`, in order (the stand-in `OpenRouter`).
    #[serde(default)]
    pub then: Vec<Then>,
    /// Characters of notes said with each tool call (the stand-in
    /// `OpenRouter`), to fill a conversation the way a chatty model does.
    pub notes: Option<usize>,
    /// What the stand-in `OpenRouter`'s helper answers to `ask_page`.
    pub ask_answer: Option<String>,
    /// What the stand-in `OpenRouter` answers when oxbot sends its answer
    /// back to be checked (default: the same answer again).
    pub checked: Option<String>,
}

/// One more tool call of a reply.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Then {
    /// The tool, as in `tool`.
    pub tool: String,
    /// Its arguments.
    #[serde(default)]
    pub args: toml::Table,
}

/// The plan's windows, from 0 to 1. The 5-hour one resets in
/// `resets_in_min` (default 3 hours), the weekly one in 3 days.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// The 5-hour window's use.
    pub five_hour: Option<f64>,
    /// The weekly window's use.
    pub seven_day: Option<f64>,
    /// When the 5-hour window resets, in minutes.
    pub resets_in_min: Option<i64>,
}

/// Tokens and cost reported for one model request.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Prompt tokens.
    #[serde(default)]
    pub prompt: u64,
    /// Completion tokens.
    #[serde(default)]
    pub completion: u64,
    /// Cost in US dollars.
    #[serde(default)]
    pub cost: f64,
}

impl Script {
    /// Reads a script; a missing or broken one is empty (every message
    /// gets the built-in echo), as the stand-in Claude always did.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let mut script: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        // Big pages are made once, as if written out in `snapshots`.
        for reply in &mut script.reply {
            if let Some(big) = &reply.big {
                reply.snapshots.extend(big.snapshots());
            }
        }
        script
    }

    /// The script named by `OXBOT_FAKE_SCRIPT`, or an empty one.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var_os("OXBOT_FAKE_SCRIPT")
            .map(|p| Self::load(Path::new(&p)))
            .unwrap_or_default()
    }

    /// The reply for a message.
    #[must_use]
    pub fn pick(&self, text: &str) -> Reply {
        let lower = text.to_lowercase();
        self.reply
            .iter()
            .find(|r| {
                r.when
                    .as_ref()
                    .is_some_and(|w| lower.contains(&w.to_lowercase()))
            })
            .or_else(|| self.reply.iter().find(|r| r.when.is_none()))
            .cloned()
            .unwrap_or_else(|| Reply {
                say: Some(format!("(stand-in) You said: {text}")),
                ..Reply::default()
            })
    }

    /// The first snapshot in the script whose selected page is `url`.
    #[must_use]
    pub fn snapshot_for(&self, url: &str) -> Option<&str> {
        self.reply
            .iter()
            .flat_map(Reply::page_snapshots)
            .find(|s| selected_page(s).is_some_and(|p| p.url == url))
    }

    /// The selected page of the first snapshot showing `url`.
    #[must_use]
    pub fn page_for(&self, url: &str) -> Option<Page> {
        self.snapshot_for(url).and_then(selected_page)
    }
}

impl Reply {
    /// The tool's full name: oxbot's own tools get `mcp__oxbot__`; full
    /// `mcp__…` names and file tools (`Read`, with a capital) stay as they are.
    #[must_use]
    pub fn tool_name(&self) -> Option<String> {
        self.tool.as_deref().map(full_tool_name)
    }

    /// The tool's arguments as JSON.
    #[must_use]
    pub fn args_json(&self) -> Value {
        table_json(&self.args)
    }

    /// The pages it "reads" before its tool, in order.
    pub fn page_snapshots(&self) -> impl Iterator<Item = &str> {
        self.snapshot
            .iter()
            .chain(&self.snapshots)
            .map(String::as_str)
    }
}

/// A script's tool name in full (see [`Reply::tool_name`]).
#[must_use]
pub fn full_tool_name(t: &str) -> String {
    if t.starts_with("mcp__") || t.starts_with(|c: char| c.is_ascii_uppercase()) {
        t.to_owned()
    } else {
        format!("mcp__oxbot__{t}")
    }
}

fn table_json(args: &toml::Table) -> Value {
    Value::Object(
        args.iter()
            .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
            .collect(),
    )
}

/// The message's text, without the `[Name] ` oxbot puts in front.
#[must_use]
pub fn user_text(content: &Value) -> String {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    match text.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((_, rest)) => rest.to_owned(),
        None => text,
    }
}

/// A page in a snapshot's `## Pages` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Its id (the browser tool's `pageId`).
    pub id: u64,
    /// Its title (may be empty).
    pub title: String,
    /// Its address.
    pub url: String,
}

impl Page {
    /// Its line in a `## Pages` list, as the browser tool writes it.
    #[must_use]
    pub fn line(&self) -> String {
        if self.title.is_empty() {
            format!("{}: {} [selected]", self.id, self.url)
        } else {
            format!("{}: {} ({}) [selected]", self.id, self.title, self.url)
        }
    }
}

/// The selected page of a snapshot (`1: Title (url) [selected]`).
#[must_use]
pub fn selected_page(snapshot: &str) -> Option<Page> {
    snapshot.lines().find_map(|line| {
        let (head, _) = line.split_once(" [selected]")?;
        let (id, label) = head.split_once(": ")?;
        let id = id.trim().parse().ok()?;
        let (title, url) = match label.strip_suffix(')').and_then(|l| l.rsplit_once(" (")) {
            Some((title, url)) => (title.to_owned(), url.to_owned()),
            None => (String::new(), label.to_owned()),
        };
        Some(Page { id, title, url })
    })
}

/// A file oxbot's note lists, as the stand-ins open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The path the stand-in reads (the text or still frame oxbot made of
    /// it, or the file itself).
    pub opened: String,
    /// What it found, in a few words.
    pub found: String,
}

/// The files oxbot's note lists (a `- /path (…)` line), each with what the
/// stand-ins say is in it: the first line of a text file or of the text
/// oxbot made of it, how many still frames a video became, or the kind of
/// a picture or PDF.
#[must_use]
pub fn attached(text: &str) -> Vec<Attached> {
    let mut files = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("- /") else {
            continue;
        };
        let (path, about) = rest.split_once(" (").unwrap_or((rest, ""));
        let path = format!("/{path}");
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let about = about
            .strip_suffix(')')
            .unwrap_or(about)
            .trim_end_matches(')');
        let readable = about.rsplit_once(": ").map(|(_, p)| p.to_owned());
        let (opened, found) = if let Some(frames) = about
            .split_once("still frames: ")
            .or_else(|| about.split_once("still frame: "))
        {
            let dir = frames.1.split("/01.jpg").next().unwrap_or("");
            let n = std::fs::read_dir(dir).map_or(0, Iterator::count);
            (format!("{dir}/01.jpg"), format!("{n} frames of {name}"))
        } else if let Some(txt) = readable.filter(|p| {
            Path::new(p)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
        }) {
            let first = first_line(&txt);
            (txt, format!("{name}: {first}"))
        } else if about == "text" {
            (path.clone(), format!("{name}: {}", first_line(&path)))
        } else {
            let kind = about.split([',', ':']).next().unwrap_or("file");
            (path.clone(), format!("{name} ({kind})"))
        };
        files.push(Attached { opened, found });
    }
    files
}

/// What a reply with `read` says once it has opened the files.
#[must_use]
pub fn read_answer(say: Option<&str>, files: &[Attached]) -> String {
    let found = if files.is_empty() {
        "I don't see any files.".to_owned()
    } else {
        files
            .iter()
            .map(|f| format!("Read {}", f.found))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match say {
        Some(say) => format!("{say}\n{found}"),
        None => found,
    }
}

/// The first line of a file that has words.
#[must_use]
pub fn first_line(path: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(str::to_owned))
        .unwrap_or_else(|| "(can't read it)".to_owned())
}

/// One tool call a loop-engine stand-in makes for a reply.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedCall {
    /// The tool's full name.
    pub name: String,
    /// Its arguments.
    pub args: Value,
}

/// The tool calls a model would make for `reply`, one per step, as the
/// stand-in Claude acts them out in one go: each attached file is Read
/// (with `read`); otherwise each snapshot's page is opened and
/// snapshotted, then the scripted tool runs. `needs_page` says whether a
/// browser tool requires `pageId` (from the request's tool list); the id is
/// the page last opened.
#[must_use]
pub fn plan(reply: &Reply, text: &str, needs_page: impl Fn(&str) -> bool) -> Vec<PlannedCall> {
    const DEVTOOLS: &str = "mcp__chrome-devtools__";
    let mut calls = Vec::new();
    if reply.read {
        for file in attached(text) {
            calls.push(PlannedCall {
                name: "Read".to_owned(),
                args: json!({ "file_path": file.opened }),
            });
        }
        return calls;
    }
    let Some(tool) = reply.tool_name() else {
        return calls;
    };
    let with_page = |name: &str, mut args: Value, page: Option<u64>| {
        if let (Some(id), Some(map)) = (page, args.as_object_mut())
            && !map.contains_key("pageId")
            && needs_page(name)
        {
            map.insert("pageId".to_owned(), json!(id));
        }
        PlannedCall {
            name: name.to_owned(),
            args,
        }
    };
    let mut page = None;
    for snapshot in reply.page_snapshots() {
        if let Some(p) = selected_page(snapshot) {
            page = Some(p.id);
            calls.push(with_page(
                &format!("{DEVTOOLS}navigate_page"),
                json!({ "type": "url", "url": p.url }),
                page,
            ));
        }
        calls.push(with_page(
            &format!("{DEVTOOLS}take_snapshot"),
            json!({}),
            page,
        ));
    }
    let args = reply.args_json();
    calls.push(if tool.starts_with(DEVTOOLS) {
        with_page(&tool, args, page)
    } else {
        PlannedCall { name: tool, args }
    });
    for more in &reply.then {
        let name = full_tool_name(&more.tool);
        let args = table_json(&more.args);
        calls.push(if name.starts_with(DEVTOOLS) {
            with_page(&name, args, page)
        } else {
            PlannedCall { name, args }
        });
    }
    calls
}

/// The text inside one outer `<web_page …>` or `<file …>` block, as oxbot
/// wraps outside text for a loop model, unescaped; other text as it is. A
/// `[stored as …]` line before the block is left out.
#[must_use]
pub fn unwrap_data_block(text: &str) -> String {
    let text = match text.strip_prefix("[stored as ") {
        Some(rest) => rest.split_once("]\n").map_or(text, |(_, block)| block),
        None => text,
    };
    for tag in ["web_page", "file"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        if let Some(rest) = text.strip_prefix(&open)
            && let Some((_, inner)) = rest.split_once('>')
            && let Some(inner) = inner.strip_suffix(&close)
        {
            return inner
                .replace("<\\", "<")
                .replace("&lt;", "<")
                .replace("&amp;", "&");
        }
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> Script {
        toml::from_str(
            r###"
[[reply]]
when = "post it"
tool = "mcp__chrome-devtools__click"
args = { uid = "1_3" }
snapshot = "## Pages\n1: Jobs (https://jobs.example.com/apply) [selected]\n## Latest page snapshot\nuid=1_1 RootWebArea \"Jobs\""
after = "Posted."
busy = ["e2e/tool-model:free"]
usage = { prompt = 1200, completion = 80 }

[[reply]]
when = "make a bot"
tool = "create_bot"
args = { name = "Recipes" }

[[reply]]
when = "files"
say = "Here."
read = true

[[reply]]
say = "Got it."
"###,
        )
        .unwrap()
    }

    #[test]
    fn replies_are_picked_by_their_words() {
        let s = script();
        assert_eq!(
            s.pick("please POST IT now").after.as_deref(),
            Some("Posted.")
        );
        assert_eq!(s.pick("anything").say.as_deref(), Some("Got it."));
        let empty = Script::default();
        assert_eq!(
            empty.pick("hi").say.as_deref(),
            Some("(stand-in) You said: hi")
        );
        let r = s.pick("post it");
        assert_eq!(r.busy, ["e2e/tool-model:free"]);
        assert_eq!(
            r.usage,
            Some(Usage {
                prompt: 1200,
                completion: 80,
                cost: 0.0
            })
        );
    }

    #[test]
    fn user_text_drops_the_sender() {
        assert_eq!(user_text(&json!("[Sam] hello there")), "hello there");
        assert_eq!(
            user_text(&json!([{"type":"text","text":"[Ann] a"},{"type":"text","text":"b"}])),
            "a b"
        );
        assert_eq!(user_text(&json!("plain")), "plain");
    }

    #[test]
    fn pages_are_read_from_snapshots() {
        let p = selected_page("## Pages\n1: Jobs (https://jobs.example.com/apply) [selected]\n")
            .unwrap();
        assert_eq!(
            p,
            Page {
                id: 1,
                title: "Jobs".into(),
                url: "https://jobs.example.com/apply".into()
            }
        );
        assert_eq!(
            p.line(),
            "1: Jobs (https://jobs.example.com/apply) [selected]"
        );
        let bare = selected_page("## Pages\n2: about:blank [selected]").unwrap();
        assert_eq!((bare.id, bare.url.as_str()), (2, "about:blank"));
        assert!(selected_page("uid=1_1 RootWebArea").is_none());
        let s = script();
        assert!(s.snapshot_for("https://jobs.example.com/apply").is_some());
        assert!(s.snapshot_for("https://other.example.com/").is_none());
    }

    #[test]
    fn a_browser_reply_opens_and_reads_its_page_first() {
        let s = script();
        let calls = plan(&s.pick("post it"), "post it", |n| {
            !n.ends_with("list_pages")
        });
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "mcp__chrome-devtools__navigate_page",
                "mcp__chrome-devtools__take_snapshot",
                "mcp__chrome-devtools__click"
            ]
        );
        assert_eq!(
            calls[0].args,
            json!({"type":"url","url":"https://jobs.example.com/apply","pageId":1})
        );
        assert_eq!(calls[2].args, json!({"uid":"1_3","pageId":1}));
        // A tool list without `pageId` gets none.
        let calls = plan(&s.pick("post it"), "post it", |_| false);
        assert_eq!(calls[2].args, json!({"uid":"1_3"}));
    }

    #[test]
    fn oxbot_tools_get_their_full_name() {
        let s = script();
        let calls = plan(&s.pick("make a bot"), "make a bot", |_| true);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "mcp__oxbot__create_bot");
        assert_eq!(calls[0].args, json!({"name":"Recipes"}));
        assert_eq!(plan(&s.pick("hello"), "hello", |_| true).len(), 0);
        let read = Reply {
            tool: Some("Read".into()),
            ..Reply::default()
        };
        assert_eq!(read.tool_name().as_deref(), Some("Read"));
    }

    #[test]
    fn attached_files_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "\nFirst line\nsecond").unwrap();
        let text = format!(
            "files attached:\n- {} (text)\n- /x/photo.png (picture, 3 KB)",
            notes.display()
        );
        let files = attached(&text);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].found, "notes.txt: First line");
        assert_eq!(files[1].found, "photo.png (picture)");
        let s = script();
        let calls = plan(&s.pick("files"), &text, |_| true);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].args, json!({"file_path":"/x/photo.png"}));
        assert_eq!(
            read_answer(Some("Here."), &files),
            "Here.\nRead notes.txt: First line\nRead photo.png (picture)"
        );
        assert_eq!(read_answer(None, &[]), "I don't see any files.");
    }

    #[test]
    fn data_blocks_are_unwrapped() {
        assert_eq!(
            unwrap_data_block(
                "<web_page url=\"https://a.example.com/\">a &lt;b> &amp; c</web_page>"
            ),
            "a <b> & c"
        );
        assert_eq!(unwrap_data_block("Created Recipes."), "Created Recipes.");
    }
}
