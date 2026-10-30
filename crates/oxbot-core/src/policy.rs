//! What a bot may do with a tool, decided by oxbot (not by the bot).
//!
//! Claude asks oxbot before every browser action (see
//! `ClaudeConfig::ask_oxbot`). The shared browser stays signed in to the
//! user's sites, so anything that changes something there needs the user's
//! yes. [`decide`] answers:
//!
//! | Browser tools | Answer |
//! |---|---|
//! | reading, opening web pages, scrolling, hovering, typing | run |
//! | clicking something that changes things (Send, Post, Delete, Buy…) | red card |
//! | pressing Enter in a text box (not a search box) | red card |
//! | uploading a file from the Mac | card |
//! | typing into a password or code field | refuse: the user signs in |
//! | running scripts in a page | refuse |
//! | opening anything but a web page (`file:`, `chrome:`…) | refuse |
//! | reading or writing files outside the bot's folder | refuse |
//! | anything else (extensions, audits, page tools…) | refuse |
//! | oxbot's own `request_sign_in` | run |
//! | Herder's `list_bots`, `give_task`, `read_reply` | run |
//! | Herder's `create_bot` | card with the name and instructions |
//!
//! Labels come from the page snapshot (`button "Post"`), so a change behind
//! an oddly named button can slip past; the bot is also told to ask first.

use std::path::Path;

use oxbot_browser::{MCP_SERVER_NAME, web_host};
use oxbot_protocol::ApprovalKind;
use serde_json::Value;

use crate::tools::{
    self, CREATE_BOT_TOOL, GIVE_TASK_TOOL, LIST_BOTS_TOOL, READ_REPLY_TOOL, SIGN_IN_TOOL,
};

/// oxbot's answer to "may the bot run this?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it.
    Allow,
    /// Don't; tell the bot why.
    Deny(String),
    /// Ask the user.
    Ask(Ask),
}

/// What to show the user on the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// What kind of request.
    pub kind: ApprovalKind,
    /// The action, e.g. `click` or `upload a file to`.
    pub action: String,
    /// What it acts on, e.g. `button "Post"`.
    pub target: String,
    /// The site, e.g. `example.com`.
    pub site: String,
    /// More: the files it will upload.
    pub detail: Option<String>,
}

/// What [`decide`] needs to know about the bot right now.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// Its folder: the only place the browser tool may read or write files.
    pub work_dir: &'a Path,
    /// The page it's on, if known.
    pub page_url: Option<&'a str>,
    /// What oxbot read from the page itself, if it looked (see
    /// [`needs_facts`]).
    pub facts: Option<&'a PageFacts>,
}

/// What oxbot reads from the page before some actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageFacts {
    /// The field that has the keyboard focus, if any.
    pub active: Option<Field>,
    /// The labels of the page's password fields.
    pub password_fields: Vec<String>,
    /// The label of what's at the point a `click_at` aims at.
    pub at_point: Option<String>,
}

/// A form field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// What kind.
    pub kind: FieldKind,
    /// Its label, placeholder or name.
    pub label: String,
}

/// Kinds of form field that matter to the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// A password field.
    Password,
    /// A search box.
    Search,
    /// A text box, text area or editable area: Enter may send.
    Text,
    /// Something else (a button, a link, the page).
    Other,
}

/// Whether `tool` needs [`PageFacts`] (oxbot looks at the page first).
#[must_use]
pub fn needs_facts(tool: &str) -> bool {
    matches!(
        browser_action(tool),
        Some("press_key" | "type_text" | "fill" | "fill_form" | "click_at")
    )
}

/// The script oxbot runs in the page to get [`PageFacts`]; `point` is a
/// `click_at` target. It only reads.
#[must_use]
pub fn facts_script(input: &Value) -> String {
    let x = input.get("x").and_then(Value::as_f64).unwrap_or(-1.0);
    let y = input.get("y").and_then(Value::as_f64).unwrap_or(-1.0);
    format!(
        r"(() => {{
  const name = e => ((e.getAttribute && (e.getAttribute('aria-label') || e.getAttribute('placeholder') || e.getAttribute('title')))
    || (e.labels && e.labels[0] && e.labels[0].innerText) || e.name || e.id || e.innerText || '').trim().slice(0, 80);
  const a = document.activeElement;
  let active = null;
  if (a && a !== document.body && a !== document.documentElement) {{
    const type = (a.type || '').toLowerCase(), tag = a.tagName.toLowerCase(), role = (a.getAttribute('role') || '').toLowerCase();
    const label = name(a);
    let kind = 'other';
    if (type === 'password') kind = 'password';
    else if (type === 'search' || role === 'searchbox' || role === 'combobox' || /search/i.test(label + ' ' + (a.name || '') + ' ' + (a.id || ''))) kind = 'search';
    else if (tag === 'textarea' || a.isContentEditable || (tag === 'input' && !['button','submit','checkbox','radio','reset','image','file'].includes(type))) kind = 'text';
    active = {{ kind, label }};
  }}
  const passwords = [...document.querySelectorAll('input[type=password]')].map(name);
  let at = null;
  if ({x} >= 0 && {y} >= 0) {{
    const e = document.elementFromPoint({x}, {y});
    const c = e && (e.closest('button,a,[role=button],[role=link],[role=menuitem],input') || e);
    if (c) at = name(c);
  }}
  return {{ active, passwords, at }};
}})()"
    )
}

impl PageFacts {
    /// Reads what [`facts_script`] returned.
    #[must_use]
    pub fn from_value(v: &Value) -> Self {
        let active = v.get("active").filter(|a| a.is_object()).map(|a| Field {
            kind: match str_of(a, "kind") {
                "password" => FieldKind::Password,
                "search" => FieldKind::Search,
                "text" => FieldKind::Text,
                _ => FieldKind::Other,
            },
            label: str_of(a, "label").to_owned(),
        });
        let password_fields = v
            .get("passwords")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let at_point = v.get("at").and_then(Value::as_str).map(str::to_owned);
        Self {
            active,
            password_fields,
            at_point,
        }
    }
}

/// What the bot is told when it tries to type a password or code.
const NO_SECRETS: &str = "oxbot never lets bots type passwords, codes or card numbers. If this site needs \
    signing in, call request_sign_in so the user can sign in themselves.";

/// The browser action of a browser tool, e.g. `click`.
fn browser_action(tool: &str) -> Option<&str> {
    tool.strip_prefix("mcp__")?
        .strip_prefix(MCP_SERVER_NAME)?
        .strip_prefix("__")
}

/// oxbot's own tools: sign-in and Herder's team tools run (oxbot
/// itself checks who may call them, see `tools`); creating a bot asks.
fn decide_oxbot_tool(tool: &str, input: &Value) -> Option<Verdict> {
    let runs = [
        SIGN_IN_TOOL,
        LIST_BOTS_TOOL,
        GIVE_TASK_TOOL,
        READ_REPLY_TOOL,
    ];
    if runs.iter().any(|t| tool == tools::full_name(t)) {
        return Some(Verdict::Allow);
    }
    (tool == tools::full_name(CREATE_BOT_TOOL)).then(|| {
        Verdict::Ask(Ask {
            kind: ApprovalKind::CreateBot,
            action: if input.get("private").and_then(Value::as_bool) == Some(true) {
                "create a private bot"
            } else {
                "create a bot"
            }
            .to_owned(),
            target: tools::text_arg(input, "name").trim().to_owned(),
            site: "oxbot".to_owned(),
            detail: Some(tools::text_arg(input, "instructions").trim().to_owned()),
        })
    })
}

/// How `tool` with `input` should be handled. `describe` turns an element
/// uid from the page snapshot into words (e.g. `button "Post"`).
#[must_use]
pub fn decide(
    tool: &str,
    input: &Value,
    ctx: Context<'_>,
    describe: impl Fn(&str) -> Option<String>,
) -> Verdict {
    if let Some(verdict) = decide_oxbot_tool(tool, input) {
        return verdict;
    }
    let Some(action) = browser_action(tool) else {
        // File tools inside the bot's folder never ask (acceptEdits), so
        // anything that does is outside it.
        return Verdict::Deny(format!(
            "oxbot doesn't allow {tool} here: bots may only use files in their own folder."
        ));
    };
    if let Some(path) = file_paths(input)
        .into_iter()
        .find(|p| !is_inside(Path::new(p), ctx.work_dir))
    {
        return Verdict::Deny(format!(
            "oxbot doesn't allow browser files outside your folder ({path}). Use a path inside {}.",
            ctx.work_dir.display()
        ));
    }
    let site = || {
        ctx.page_url
            .and_then(web_host)
            .unwrap_or_else(|| "this site".to_owned())
    };
    let change = |action: &str, target: String| {
        Verdict::Ask(Ask {
            kind: ApprovalKind::Change,
            action: action.to_owned(),
            target,
            site: site(),
            detail: None,
        })
    };
    match action {
        "list_pages"
        | "select_page"
        | "close_page"
        | "take_snapshot"
        | "take_screenshot"
        | "wait_for"
        | "list_network_requests"
        | "get_network_request"
        | "list_console_messages"
        | "get_console_message"
        | "hover"
        | "resize_page"
        | "get_tab_id"
        | "get_css_styles"
        | "drag"
        | "handle_dialog" => Verdict::Allow,
        "click" | "click_at" | "fill" | "fill_form" | "type_text" | "press_key" => {
            decide_input(action, input, ctx.facts, &describe, &change)
        }
        "evaluate_script" => Verdict::Deny(
            "oxbot doesn't let bots run scripts in pages, because the browser is signed in to the user's \
             sites. Read pages with take_snapshot and act with click and fill."
                .to_owned(),
        ),
        "navigate_page" | "new_page" => {
            let kind = str_of(input, "type");
            if action == "navigate_page" && !kind.is_empty() && kind != "url" {
                return Verdict::Allow; // back, forward, reload
            }
            let url = str_of(input, "url");
            if url == "about:blank" || web_host(url).is_some() {
                Verdict::Allow
            } else {
                Verdict::Deny(format!(
                    "oxbot only opens web pages (http or https), not {url:?}."
                ))
            }
        }
        "upload_file" => {
            let uid = str_of(input, "uid");
            Verdict::Ask(Ask {
                kind: ApprovalKind::Upload,
                action: "upload a file to".to_owned(),
                target: describe(uid).unwrap_or_else(|| format!("element {uid}")),
                site: site(),
                detail: Some(file_paths(input).join("\n")),
            })
        }
        other => Verdict::Deny(format!(
            "The browser tool {other} isn't available to oxbot bots."
        )),
    }
}

/// Clicking and typing: free, unless it changes something (a card) or
/// types a secret (refused).
fn decide_input(
    action: &str,
    input: &Value,
    facts: Option<&PageFacts>,
    describe: &dyn Fn(&str) -> Option<String>,
    change: &dyn Fn(&str, String) -> Verdict,
) -> Verdict {
    match action {
        "click" => {
            let uid = str_of(input, "uid");
            match describe(uid) {
                Some(label) if !is_change(&label) => Verdict::Allow,
                Some(label) => change("click", label),
                None => change(
                    "click",
                    format!("element {uid} (oxbot couldn't see what it is)"),
                ),
            }
        }
        "click_at" => match facts.and_then(|f| f.at_point.clone()) {
            Some(label) if !is_change(&label) => Verdict::Allow,
            Some(label) => change("click", format!("\"{label}\"")),
            None => change("click", "a spot on the page oxbot couldn't read".to_owned()),
        },
        "fill" | "fill_form" => {
            let uids: Vec<&str> = if action == "fill" {
                vec![str_of(input, "uid")]
            } else {
                input
                    .get("elements")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().map(|e| str_of(e, "uid")).collect())
                    .unwrap_or_default()
            };
            let secret = uids
                .iter()
                .any(|uid| describe(uid).is_some_and(|label| is_secret(&label, facts)));
            if secret {
                Verdict::Deny(NO_SECRETS.to_owned())
            } else {
                Verdict::Allow
            }
        }
        "type_text" => {
            let active = facts.and_then(|f| f.active.as_ref());
            if active.is_some_and(|a| a.kind == FieldKind::Password || is_secret(&a.label, None)) {
                return Verdict::Deny(NO_SECRETS.to_owned());
            }
            if is_enter(str_of(input, "submitKey")) {
                enter_rule(facts, change)
            } else {
                Verdict::Allow
            }
        }
        "press_key" => {
            if is_enter(str_of(input, "key")) {
                enter_rule(facts, change)
            } else {
                Verdict::Allow
            }
        }
        _ => Verdict::Allow,
    }
}

/// Pressing Enter: free in a search box or outside text boxes, a card in a
/// text box (it may send), refused in a password field.
fn enter_rule(facts: Option<&PageFacts>, change: &dyn Fn(&str, String) -> Verdict) -> Verdict {
    let Some(facts) = facts else {
        return change("press Enter on", "a page oxbot couldn't read".to_owned());
    };
    match &facts.active {
        None => Verdict::Allow,
        Some(field) => match field.kind {
            FieldKind::Password => Verdict::Deny(NO_SECRETS.to_owned()),
            FieldKind::Text => change("press Enter in", quoted(&field.label, "a text box")),
            FieldKind::Other if is_change(&field.label) => {
                change("press Enter on", quoted(&field.label, "a button"))
            }
            FieldKind::Search | FieldKind::Other => Verdict::Allow,
        },
    }
}

fn quoted(label: &str, fallback: &str) -> String {
    if label.trim().is_empty() {
        fallback.to_owned()
    } else {
        format!("\"{}\"", label.trim())
    }
}

/// Whether `key` (e.g. `Enter`, `Control+Enter`) presses Enter without
/// Shift (Shift+Enter is a new line).
fn is_enter(key: &str) -> bool {
    let parts: Vec<&str> = key.split('+').map(str::trim).collect();
    parts
        .last()
        .is_some_and(|k| k.eq_ignore_ascii_case("enter"))
        && !parts.iter().any(|p| p.eq_ignore_ascii_case("shift"))
}

/// Words on a button that mean it changes something.
const CHANGE_WORDS: &[&str] = &[
    "send",
    "post",
    "publish",
    "reply",
    "comment",
    "delete",
    "remove",
    "trash",
    "archive",
    "buy",
    "purchase",
    "pay",
    "order",
    "checkout",
    "submit",
    "confirm",
    "save",
    "share",
    "repost",
    "retweet",
    "like",
    "follow",
    "unfollow",
    "subscribe",
    "unsubscribe",
    "connect",
    "invite",
    "accept",
    "decline",
    "approve",
    "transfer",
    "donate",
    "book",
    "reserve",
    "apply",
    "upload",
    "schedule",
    "update",
    "logout",
    "signout",
    "forward",
    "tweet",
    "react",
];

/// Phrases that mean a change, for two-word buttons.
const CHANGE_PHRASES: &[&str] = &[
    "check out",
    "sign out",
    "log out",
    "place order",
    "cancel order",
    "cancel subscription",
    "cancel booking",
    "cancel membership",
    "cancel plan",
    "mark as",
    "move to",
];

/// Whether a snapshot label (`button "Post"`) or a plain label names a
/// change. Typing fields never do.
fn is_change(label: &str) -> bool {
    let (role, name) = split_label(label);
    if matches!(role, "textbox" | "searchbox" | "combobox") {
        return false;
    }
    let name = name.to_lowercase();
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().any(|w| CHANGE_WORDS.contains(w))
        || CHANGE_PHRASES.iter().any(|p| words.join(" ").contains(p))
}

/// Whether a field is for a password, a code or a card number.
fn is_secret(label: &str, facts: Option<&PageFacts>) -> bool {
    let (_, name) = split_label(label);
    let name = name.to_lowercase();
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let joined = words.join(" ");
    let named_secret = name.contains("password")
        || name.contains("passcode")
        || words
            .iter()
            .any(|w| ["pin", "otp", "2fa", "cvv", "cvc", "ssn"].contains(w))
        || [
            "one time",
            "verification code",
            "security code",
            "2 step",
            "two factor",
            "two step",
            "card number",
            "authenticator code",
        ]
        .iter()
        .any(|p| joined.contains(p));
    named_secret
        || facts.is_some_and(|f| {
            f.password_fields
                .iter()
                .any(|p| !p.trim().is_empty() && p.trim().eq_ignore_ascii_case(name.trim()))
        })
}

/// `button "Post" focusable` → (`button`, `Post`); a plain label is all name.
fn split_label(label: &str) -> (&str, &str) {
    if let Some(open) = label.find('"')
        && let Some(close) = label[open + 1..].find('"')
    {
        let role = label[..open].split_whitespace().next().unwrap_or("");
        return (role, &label[open + 1..open + 1 + close]);
    }
    ("", label)
}

/// Every file path in a tool's input (`filePath`, `filePaths`,
/// `requestFilePath`, `responseFilePath`…).
fn file_paths(input: &Value) -> Vec<String> {
    let Some(map) = input.as_object() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for (key, value) in map {
        if !(key.ends_with("FilePath") || key == "filePath" || key == "filePaths") {
            continue;
        }
        match value {
            Value::String(p) => paths.push(p.clone()),
            Value::Array(items) => {
                paths.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            _ => {}
        }
    }
    paths
}

/// Whether `path` is an absolute path inside `dir`, after resolving `..`
/// and links (on a Mac `/var` is a link to `/private/var`, so one folder can
/// have two names).
fn is_inside(path: &Path, dir: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let path = lexical(path);
    path.starts_with(dir) || real(&path).starts_with(real(dir))
}

/// `path` with `.` and `..` resolved, without touching the disk.
fn lexical(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut resolved = std::path::PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}

/// `path` with links resolved, as far as it exists (a file about to be
/// written doesn't exist yet, but its folder does).
fn real(path: &Path) -> std::path::PathBuf {
    let mut rest = Vec::new();
    let mut base = path;
    loop {
        if let Ok(found) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(found, |p, part| p.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WORK: &str = "/bots/news/work";

    fn ctx() -> Context<'static> {
        Context {
            work_dir: Path::new(WORK),
            page_url: Some("https://example.com/jobs"),
            facts: None,
        }
    }

    fn with_facts(facts: &PageFacts) -> Context<'_> {
        Context {
            facts: Some(facts),
            ..ctx()
        }
    }

    fn field(kind: FieldKind, label: &str) -> PageFacts {
        PageFacts {
            active: Some(Field {
                kind,
                label: label.into(),
            }),
            ..PageFacts::default()
        }
    }

    fn names(uid: &str) -> Option<String> {
        match uid {
            "1" => Some("button \"Post\"".into()),
            "2" => Some("link \"Inbox (3)\"".into()),
            "3" => Some("textbox \"Password\"".into()),
            "4" => Some("textbox \"Search mail\"".into()),
            "5" => Some("button \"Delete forever\"".into()),
            "6" => Some("tab \"Posts\"".into()),
            _ => None,
        }
    }

    fn tool(name: &str) -> String {
        format!("mcp__chrome-devtools__{name}")
    }

    fn no_names(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn browsing_runs_without_asking() {
        assert_eq!(
            decide(&tool("take_snapshot"), &json!({}), ctx(), no_names),
            Verdict::Allow
        );
        for uid in ["2", "6"] {
            assert_eq!(
                decide(&tool("click"), &json!({ "uid": uid }), ctx(), names),
                Verdict::Allow,
                "{uid}"
            );
        }
        assert_eq!(
            decide(
                &tool("fill"),
                &json!({"uid":"4","value":"invoice"}),
                ctx(),
                names
            ),
            Verdict::Allow
        );
        assert_eq!(
            decide(&tool("press_key"), &json!({"key":"PageDown"}), ctx(), names),
            Verdict::Allow
        );
        assert_eq!(
            decide(
                &tool("new_page"),
                &json!({"url":"https://news.ycombinator.com/"}),
                ctx(),
                no_names
            ),
            Verdict::Allow
        );
        assert_eq!(
            decide(
                &tool("navigate_page"),
                &json!({"type":"back"}),
                ctx(),
                no_names
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn changes_ask_with_a_red_card() {
        for uid in ["1", "5"] {
            let Verdict::Ask(ask) = decide(&tool("click"), &json!({ "uid": uid }), ctx(), names)
            else {
                panic!("{uid} should ask")
            };
            assert_eq!(ask.kind, ApprovalKind::Change);
            assert_eq!(ask.action, "click");
            assert_eq!(ask.site, "example.com");
        }
        assert!(
            matches!(
                decide(&tool("click"), &json!({"uid":"99"}), ctx(), names),
                Verdict::Ask(_)
            ),
            "an unknown element asks"
        );
        let facts = PageFacts {
            at_point: Some("Send".into()),
            ..PageFacts::default()
        };
        assert!(matches!(
            decide(
                &tool("click_at"),
                &json!({"x":5,"y":5}),
                with_facts(&facts),
                names
            ),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn enter_asks_in_a_text_box_but_not_in_search() {
        let message = field(FieldKind::Text, "Write a message");
        let search = field(FieldKind::Search, "Search");
        let enter = json!({"key":"Enter"});
        assert!(matches!(
            decide(&tool("press_key"), &enter, with_facts(&message), names),
            Verdict::Ask(_)
        ));
        assert_eq!(
            decide(&tool("press_key"), &enter, with_facts(&search), names),
            Verdict::Allow
        );
        assert_eq!(
            decide(
                &tool("press_key"),
                &json!({"key":"Shift+Enter"}),
                with_facts(&message),
                names
            ),
            Verdict::Allow,
            "Shift+Enter is a new line"
        );
        assert!(matches!(
            decide(
                &tool("type_text"),
                &json!({"text":"hi","submitKey":"Enter"}),
                with_facts(&message),
                names
            ),
            Verdict::Ask(_)
        ));
        assert!(
            matches!(
                decide(&tool("press_key"), &enter, ctx(), names),
                Verdict::Ask(_)
            ),
            "no facts: ask"
        );
    }

    #[test]
    fn passwords_and_codes_are_never_typed() {
        assert!(matches!(
            decide(&tool("fill"), &json!({"uid":"3","value":"x"}), ctx(), names),
            Verdict::Deny(_)
        ));
        assert!(matches!(
            decide(
                &tool("fill_form"),
                &json!({"elements":[{"uid":"4","value":"a"},{"uid":"3","value":"b"}]}),
                ctx(),
                names
            ),
            Verdict::Deny(_)
        ));
        let code = field(FieldKind::Text, "Enter the verification code");
        assert!(matches!(
            decide(
                &tool("type_text"),
                &json!({"text":"123456"}),
                with_facts(&code),
                names
            ),
            Verdict::Deny(_)
        ));
        let pw = field(FieldKind::Password, "");
        assert!(matches!(
            decide(
                &tool("type_text"),
                &json!({"text":"x"}),
                with_facts(&pw),
                names
            ),
            Verdict::Deny(_)
        ));
        let unnamed = |_: &str| Some("textbox \"Secret word\"".to_owned());
        let facts = PageFacts {
            password_fields: vec!["Secret word".into()],
            ..PageFacts::default()
        };
        assert!(
            matches!(
                decide(
                    &tool("fill"),
                    &json!({"uid":"7","value":"x"}),
                    with_facts(&facts),
                    unnamed
                ),
                Verdict::Deny(_)
            ),
            "a password field with an odd label"
        );
    }

    #[test]
    fn scripts_are_refused_and_sign_in_is_allowed() {
        assert!(matches!(
            decide(
                &tool("evaluate_script"),
                &json!({"function":"() => 1"}),
                ctx(),
                names
            ),
            Verdict::Deny(_)
        ));
        assert_eq!(
            decide(
                "mcp__oxbot__request_sign_in",
                &json!({"url":"https://x.com"}),
                ctx(),
                names
            ),
            Verdict::Allow
        );
        assert!(needs_facts(&tool("press_key")) && !needs_facts(&tool("click")));
    }

    #[test]
    fn creating_a_bot_needs_the_users_ok_and_tasks_dont() {
        let Verdict::Ask(ask) = decide(
            "mcp__oxbot__create_bot",
            &json!({"name":" Gmail Checker ","instructions":"Check my mail every morning."}),
            ctx(),
            names,
        ) else {
            panic!("expected a card");
        };
        assert_eq!(ask.kind, ApprovalKind::CreateBot);
        assert_eq!(ask.target, "Gmail Checker");
        assert_eq!(ask.detail.as_deref(), Some("Check my mail every morning."));
        for tool in [
            "mcp__oxbot__list_bots",
            "mcp__oxbot__give_task",
            "mcp__oxbot__read_reply",
        ] {
            assert_eq!(
                decide(tool, &json!({}), ctx(), names),
                Verdict::Allow,
                "{tool}"
            );
        }
    }

    #[test]
    fn page_facts_are_read_from_the_script_result() {
        let v = json!({"active":{"kind":"password","label":"Password"},"passwords":["Password"],"at":null});
        let facts = PageFacts::from_value(&v);
        assert_eq!(facts.active.unwrap().kind, FieldKind::Password);
        assert_eq!(facts.password_fields, ["Password"]);
        assert!(facts.at_point.is_none());
        assert!(facts_script(&json!({"x":10,"y":20})).contains("elementFromPoint(10, 20)"));
    }

    #[test]
    fn only_web_pages_can_be_opened() {
        for url in [
            "file:///etc/passwd",
            "chrome://settings",
            "javascript:alert(1)",
            "data:text/html,hi",
        ] {
            assert!(
                matches!(
                    decide(&tool("new_page"), &json!({ "url": url }), ctx(), no_names),
                    Verdict::Deny(_)
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn files_stay_in_the_bot_folder() {
        let inside = json!({"filePath": format!("{WORK}/shot.png")});
        let outside = json!({"filePath": "/Users/me/Desktop/shot.png"});
        let sneaky = json!({"responseFilePath": format!("{WORK}/../../../../etc/x")});
        assert_eq!(
            decide(&tool("take_screenshot"), &inside, ctx(), no_names),
            Verdict::Allow
        );
        assert!(matches!(
            decide(&tool("take_screenshot"), &outside, ctx(), no_names),
            Verdict::Deny(_)
        ));
        assert!(matches!(
            decide(&tool("get_network_request"), &sneaky, ctx(), no_names),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn a_folder_reached_through_a_link_still_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();
        assert!(is_inside(&real_dir.join("new.pdf"), &link));
        assert!(is_inside(&link.join("sub/new.pdf"), &real_dir));
        assert!(!is_inside(&tmp.path().join("other.pdf"), &link));
    }

    #[test]
    fn uploads_ask_and_only_from_the_bot_folder() {
        let named = |uid: &str| (uid == "3_2").then(|| "button \"Upload CV\"".to_owned());
        let ok = json!({"uid":"3_2","filePaths": [format!("{WORK}/cv.pdf")]});
        let Verdict::Ask(ask) = decide(&tool("upload_file"), &ok, ctx(), named) else {
            panic!("uploads ask")
        };
        assert_eq!(ask.kind, ApprovalKind::Upload);
        assert_eq!(ask.target, "button \"Upload CV\"");
        assert_eq!(ask.site, "example.com");
        assert_eq!(ask.detail.as_deref(), Some("/bots/news/work/cv.pdf"));
        let bad = json!({"uid":"3_2","filePaths": ["/Users/me/.ssh/id_ed25519"]});
        assert!(matches!(
            decide(&tool("upload_file"), &bad, ctx(), no_names),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn unknown_tools_and_files_outside_are_refused() {
        assert!(matches!(
            decide(&tool("install_extension"), &json!({}), ctx(), no_names),
            Verdict::Deny(_)
        ));
        assert!(matches!(
            decide("Read", &json!({"file_path":"/etc/hosts"}), ctx(), no_names),
            Verdict::Deny(_)
        ));
    }
}
