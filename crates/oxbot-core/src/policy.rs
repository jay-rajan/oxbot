//! What a bot may do with a tool, decided by oxbot (not by the bot).
//!
//! Every engine asks oxbot before every browser action (its tool service
//! sends each call to the hub's gate). The shared browser stays signed in to the
//! user's sites, so anything that changes something there needs the user's
//! yes. [`decide`] answers:
//!
//! | Browser tools | Answer |
//! |---|---|
//! | reading, opening web pages, scrolling, hovering, typing | run |
//! | clicking links, tabs, search, filters, sorting, paging, "Show more" | run |
//! | clicking a search form's or a GET form's button | run |
//! | clicking an element oxbot can't name, even after looking it up | run, logged |
//! | clicking something that changes things (Send, Post, Save, Apply…) | red card |
//! | clicking a POST form's unclear button when the form has something filled in | red card |
//! | clicking pay, buy, book, delete or another can't-undo button | red card, Allow once only |
//! | a change click the user chose "Always allow" for (same bot, exact host, same button) | run, with a quiet line |
//! | pressing Enter in a text box (not a search box) | red card |
//! | uploading a file from the Mac | card |
//! | typing into a password or code field | refuse: the user signs in |
//! | running scripts in a page | refuse |
//! | opening anything but a web page (`file:`, `chrome:`…) | refuse |
//! | opening a page on this Mac or the home network (oxbot's own page, the Gate door, routers) | refuse |
//! | reading or writing files outside the bot's folder | refuse |
//! | anything else (extensions, audits, page tools…) | refuse |
//! | oxbot's own `request_sign_in` and `save_report` | run |
//! | Herder's `list_bots`, `give_task`, `read_reply` | run |
//! | Herder's `create_bot` | card with the name and instructions |
//!
//! Labels come from the page snapshot (`button "Post"`), so a change behind
//! an oddly named button can slip past; the bot is also told to ask first.
//! The page controls its labels: a "Delete" button can call itself "Apply
//! filters". What oxbot reads about a button's form only ever relaxes an
//! unclear label ("Submit", "Go"), never a clear change or a can't-undo word.
//! An "Always allow" rule is per bot, per exact host and per exact label,
//! so another site never matches, and can't-undo words never get one.

use std::path::Path;

pub use oxbot_browser::ElementFacts;
use oxbot_browser::{ELEMENT_FACTS_FN, MCP_SERVER_NAME, is_home_network, web_host};
use oxbot_protocol::ApprovalKind;
use serde_json::Value;

use crate::tools::{
    self, CREATE_BOT_TOOL, GIVE_TASK_TOOL, LIST_BOTS_TOOL, READ_REPLY_TOOL, SAVE_REPORT_TOOL,
    SIGN_IN_TOOL,
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
    /// Run it, though oxbot couldn't tell what it clicks (a missing label
    /// never blocks browsing); the hub logs what.
    AllowUnnamed(String),
}

/// What a click would do, as far as oxbot can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Click {
    /// Search, filter, sort, paging, opening a result: runs.
    Query,
    /// Links, tabs, toggles, buttons outside a changing form: runs.
    Plain,
    /// A change on the site (send, post, save, a filled POST form): a card.
    Change,
    /// Money or something that can't be undone (pay, book, delete): a card,
    /// and never one to remember.
    Final,
    /// oxbot couldn't name it: runs, logged.
    Unknown,
}

impl Click {
    /// How strict it is, for picking between two readings of one element:
    /// final over change over query over plain.
    #[must_use]
    pub fn strictness(self) -> u8 {
        match self {
            Self::Final => 4,
            Self::Change => 3,
            Self::Query => 2,
            Self::Plain => 1,
            Self::Unknown => 0,
        }
    }

    /// Its name: `query`, `plain`, `change`, `final`, `unknown`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Plain => "plain",
            Self::Change => "change",
            Self::Final => "final",
            Self::Unknown => "unknown",
        }
    }
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
    /// Whether it's about a private bot: one being made, or one getting a
    /// task.
    pub private: bool,
    /// For a click card: what kind of click ([`Click::Change`] or
    /// [`Click::Final`]).
    pub click: Option<Click>,
    /// The element an "Always allow" rule would name (see [`rule_label`]):
    /// set only for a [`Click::Change`] on a named element on a known web
    /// site. Never for can't-undo clicks, Enter, uploads or Herder's cards.
    pub rule: Option<String>,
}

impl Ask {
    /// A card of `kind`: the bot wants to `action` `target` on `site`.
    #[must_use]
    pub fn new(
        kind: ApprovalKind,
        action: impl Into<String>,
        target: impl Into<String>,
        site: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            action: action.into(),
            target: target.into(),
            site: site.into(),
            detail: None,
            private: false,
            click: None,
            rule: None,
        }
    }
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
    /// What oxbot read about the element a `click` aims at, if it looked
    /// (see [`wants_element_facts`]).
    pub element: Option<&'a ElementFacts>,
}

/// What oxbot reads from the page before some actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageFacts {
    /// The field that has the keyboard focus, if any.
    pub active: Option<Field>,
    /// The labels of the page's password fields.
    pub password_fields: Vec<String>,
    /// What's at the point a `click_at` aims at, as a snapshot label
    /// (`button "Send"`).
    pub at_point: Option<String>,
    /// What oxbot read about it (its form).
    pub at_element: Option<ElementFacts>,
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

/// The script oxbot runs in the page (in its own isolated world) to get
/// [`PageFacts`]; `x`, `y` are a `click_at` target. It only reads.
#[must_use]
pub fn facts_script(input: &Value) -> String {
    let x = input.get("x").and_then(Value::as_f64).unwrap_or(-1.0);
    let y = input.get("y").and_then(Value::as_f64).unwrap_or(-1.0);
    format!(
        r"(() => {{
  const name = e => ((e.getAttribute && (e.getAttribute('aria-label') || e.getAttribute('placeholder') || e.getAttribute('title')))
    || (e.labels && e.labels[0] && e.labels[0].innerText) || e.name || e.id || e.innerText || e.value || '').trim().slice(0, 80);
  const roleOf = e => {{
    const role = (e.getAttribute('role') || '').toLowerCase();
    if (role) return role;
    const tag = e.tagName.toLowerCase(), type = (e.type || '').toLowerCase();
    if (tag === 'a') return 'link';
    if (tag === 'button' || (tag === 'input' && ['submit', 'button', 'reset', 'image'].includes(type))) return 'button';
    if (tag === 'input' && (type === 'checkbox' || type === 'radio')) return type;
    if (tag === 'input' && type === 'search') return 'searchbox';
    if (tag === 'input' || tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    return '';
  }};
  const elementFacts = {ELEMENT_FACTS_FN};
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
    const c = e && (e.closest('button,a,[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=checkbox],[role=radio],[role=switch],input,select,textarea') || e);
    if (c) at = {{ name: name(c), role: roleOf(c), facts: elementFacts.call(c) }};
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
        let at = v.get("at").unwrap_or(&Value::Null);
        let at_point = match at {
            Value::String(name) => Some(name.clone()),
            Value::Object(_) => {
                let (role, name) = (str_of(at, "role"), str_of(at, "name"));
                Some(if role.is_empty() {
                    format!("\"{name}\"")
                } else {
                    format!("{role} \"{name}\"")
                })
            }
            _ => None,
        };
        let at_element = at.get("facts").and_then(ElementFacts::from_value);
        Self {
            active,
            password_fields,
            at_point,
            at_element,
        }
    }
}

/// The loop's own question tool (`mcp__oxbot__ask_user`): it only ends the
/// turn with a question for the person.
pub const ASK_USER_TOOL: &str = "ask_user";

/// The loop's own tools over its context (`mcp__oxbot__…`): the model's
/// to-do list and progress notes, stored results read back, and the page
/// tools over stored snapshots. They change nothing outside oxbot, so they
/// run; the gate still sees every call.
pub const LOOP_CONTEXT_TOOLS: [&str; 8] = [
    "update_todo",
    "note_progress",
    "read_result",
    "find_in_page",
    "read_section",
    "ask_page",
    "extract_rows",
    "query_rows",
];

/// The browser tools oxbot's own loop offers models (their own names). None
/// is one [`decide`] always refuses; scripts, network and console tools
/// are left out, and fewer tools help small models.
pub const LOOP_BROWSER_TOOLS: [&str; 17] = [
    "navigate_page",
    "new_page",
    "list_pages",
    "select_page",
    "close_page",
    "take_snapshot",
    "take_screenshot",
    "wait_for",
    "hover",
    "click",
    "fill",
    "fill_form",
    "type_text",
    "press_key",
    "drag",
    "upload_file",
    "handle_dialog",
];

/// The file tools, as models know them.
const FILE_TOOLS: [&str; 5] = ["Read", "Write", "Edit", "Glob", "Grep"];

/// The rule for file tools, shared by both engines: a file tool
/// whose every path (`file_path`, `path`; no `path` means the folder
/// itself; relative paths are inside `work_dir`) is inside `work_dir`,
/// as written and after following links, runs without asking. Anything
/// else goes to [`decide`], which refuses files outside the folder.
#[must_use]
pub fn file_call_inside(tool: &str, input: &Value, work_dir: &Path) -> bool {
    if !FILE_TOOLS.contains(&tool) {
        return false;
    }
    let inside = |given: &str| {
        let path = Path::new(given);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            work_dir.join(path)
        };
        let written = lexical(&path);
        (written.starts_with(lexical(work_dir)) || written.starts_with(real(work_dir)))
            && real(&written).starts_with(real(work_dir))
    };
    ["file_path", "path"]
        .iter()
        .all(|key| match input.get(*key) {
            None | Some(Value::Null) => *key == "path" || tool == "Glob" || tool == "Grep",
            Some(Value::String(p)) => inside(p),
            Some(_) => false,
        })
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

/// oxbot's own tools: sign-in, saving a report (which stays inside oxbot)
/// and Herder's team tools run (oxbot itself checks who may call them, see
/// `tools`); creating a bot asks.
fn decide_oxbot_tool(tool: &str, input: &Value) -> Option<Verdict> {
    let runs = [
        SIGN_IN_TOOL,
        SAVE_REPORT_TOOL,
        LIST_BOTS_TOOL,
        GIVE_TASK_TOOL,
        READ_REPLY_TOOL,
        ASK_USER_TOOL,
    ];
    if runs
        .iter()
        .chain(LOOP_CONTEXT_TOOLS.iter())
        .any(|t| tool == tools::full_name(t))
    {
        return Some(Verdict::Allow);
    }
    (tool == tools::full_name(CREATE_BOT_TOOL)).then(|| {
        let private = input.get("private").and_then(Value::as_bool) == Some(true);
        let action = if private {
            "create a private bot"
        } else {
            "create a bot"
        };
        Verdict::Ask(Ask {
            detail: Some(tools::text_arg(input, "instructions").trim().to_owned()),
            private,
            ..Ask::new(
                ApprovalKind::CreateBot,
                action,
                tools::text_arg(input, "name").trim(),
                "oxbot",
            )
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
        Verdict::Ask(Ask::new(ApprovalKind::Change, action, target, site()))
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
            decide_input(action, input, ctx, &describe, &change)
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
            if is_home_network(url) {
                return Verdict::Deny(format!(
                    "oxbot doesn't open pages on this Mac or the home network ({url}), only on the web."
                ));
            }
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
                detail: Some(file_paths(input).join("\n")),
                ..Ask::new(
                    ApprovalKind::Upload,
                    "upload a file to",
                    describe(uid).unwrap_or_else(|| format!("element {uid}")),
                    site(),
                )
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
    ctx: Context<'_>,
    describe: &dyn Fn(&str) -> Option<String>,
    change: &dyn Fn(&str, String) -> Verdict,
) -> Verdict {
    let facts = ctx.facts;
    let on_web = ctx.page_url.and_then(web_host).is_some();
    match action {
        "click" => {
            let uid = str_of(input, "uid");
            let label = describe(uid);
            click_verdict(label.as_deref(), ctx.element, on_web, change, || {
                format!("element {uid}")
            })
        }
        "click_at" => {
            let label = facts.and_then(|f| f.at_point.as_deref());
            let element = facts.and_then(|f| f.at_element.as_ref());
            click_verdict(label, element, on_web, change, || {
                let at = |key| input.get(key).and_then(Value::as_f64).unwrap_or(-1.0);
                format!("the spot at ({}, {})", at("x"), at("y"))
            })
        }
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

/// A click on `label` (with what oxbot read about it): free, a card, or
/// free and logged when oxbot couldn't name it (`unnamed` says what). A
/// change card on a web page (`on_web`) may be remembered.
fn click_verdict(
    label: Option<&str>,
    element: Option<&ElementFacts>,
    on_web: bool,
    change: &dyn Fn(&str, String) -> Verdict,
    unnamed: impl FnOnce() -> String,
) -> Verdict {
    let click = classify(label, element);
    let target = || {
        let (role, name) = split_label(label.unwrap_or_default());
        let name = shown_name(name);
        let unclear = matches!(click_words(&name), Words::Soft | Words::Nothing);
        if click == Click::Change && unclear && element.is_some_and(|e| e.submits) {
            format!("the form's \"{name}\" button")
        } else if role.is_empty() {
            format!("\"{name}\"")
        } else {
            format!("{role} \"{name}\"")
        }
    };
    match click {
        Click::Query | Click::Plain => Verdict::Allow,
        Click::Unknown => Verdict::AllowUnnamed(unnamed()),
        Click::Change | Click::Final => match change("click", target()) {
            Verdict::Ask(ask) => Verdict::Ask(Ask {
                click: Some(click),
                rule: label
                    .filter(|_| click == Click::Change && on_web)
                    .and_then(rule_label),
                ..ask
            }),
            other => other,
        },
    }
}

/// How a rule names an element: its role and name (`button "Apply
/// filters"`), with the name trimmed and its spaces collapsed, and
/// attributes such as `focusable` dropped. Case counts. `None` for an
/// element with no name.
#[must_use]
pub fn rule_label(label: &str) -> Option<String> {
    let (role, name) = split_label(label);
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        None
    } else if role.is_empty() {
        Some(format!("\"{name}\""))
    } else {
        Some(format!("{role} \"{name}\""))
    }
}

/// Roles that only show or move around: clicking them changes nothing on
/// the site unless their words say so.
const PLAIN_ROLES: &[&str] = &[
    "link", "tab", "menuitem", "option", "checkbox", "radio", "switch", "treeitem", "row", "cell",
    "img", "image", "heading",
];

/// What a click on `label` (a snapshot label, `button "Post"`) would do,
/// given what oxbot read about the element (`facts`), if anything.
///
/// Words win over facts: a can't-undo word is [`Click::Final`] and a clear
/// change word is [`Click::Change`] whatever the form says. Facts only
/// settle unclear buttons ("Submit", "Go", or no telling word at all): a
/// search or GET form's button is a query, and a POST form's is a change
/// once the form has something filled in.
#[must_use]
pub fn classify(label: Option<&str>, facts: Option<&ElementFacts>) -> Click {
    let Some(label) = label else {
        return Click::Unknown;
    };
    let (role, name) = split_label(label);
    if matches!(role, "textbox" | "searchbox" | "combobox") {
        return Click::Plain;
    }
    // A card or headline link (or a heading that is an item's title): its
    // name is the item's text, so words in it ("Save Recipe", "Delete your
    // account in 5 steps") don't say what a click does. A link is a GET; a
    // link that changes something has a short action name ("Unsubscribe",
    // "Log out"). A heading can't submit anything.
    if matches!(role, "link" | "heading") && name_words(name) > CARD_LINK_WORDS {
        return Click::Plain;
    }
    let words = click_words(name);
    match words {
        Words::Final => return Click::Final,
        Words::Hard => return Click::Change,
        Words::Query => return Click::Query,
        Words::Soft | Words::Nothing => {}
    }
    if PLAIN_ROLES.contains(&role) {
        return Click::Plain;
    }
    let submit = facts.filter(|f| f.submits);
    let method = submit.and_then(|f| f.form_method.as_deref());
    match (words, submit) {
        (_, Some(f)) if f.in_search || method == Some("get") => Click::Query,
        (_, Some(_)) if method == Some("dialog") => Click::Plain,
        (Words::Soft, _) => Click::Change,
        (_, Some(f)) if method == Some("post") && f.filled_fields > 0 => Click::Change,
        _ => Click::Plain,
    }
}

/// A link or heading named with more words than this is a card or headline.
const CARD_LINK_WORDS: usize = 5;

/// Words in a name (runs with a letter or digit; `…` and `|` don't count).
fn name_words(name: &str) -> usize {
    name.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
}

/// Whether reading the element itself could change what a click on `label`
/// is: an unclear button (see [`classify`]). Links, tabs and clear words
/// never need the look.
#[must_use]
pub fn wants_element_facts(label: &str) -> bool {
    let (role, name) = split_label(label);
    role == "button"
        && !name.trim().is_empty()
        && matches!(click_words(name), Words::Soft | Words::Nothing)
}

/// The accessibility role and name in a snapshot label, for looking the
/// element up on the page.
#[must_use]
pub fn role_and_name(label: &str) -> (&str, &str) {
    split_label(label)
}

/// Of the facts read about several elements with the same label, the one
/// that makes the strictest click (a change over a query over a plain
/// click).
#[must_use]
pub fn strictest<'a>(label: &str, found: &'a [ElementFacts]) -> Option<&'a ElementFacts> {
    found
        .iter()
        .max_by_key(|f| classify(Some(label), Some(f)).strictness())
}

/// What the words on a button say, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Words {
    /// Money or can't-undo.
    Final,
    /// A clear change.
    Hard,
    /// Looking things up.
    Query,
    /// A change unless the words around say it's a query ("Apply filters").
    Soft,
    /// Nothing telling.
    Nothing,
}

/// Words on a button that spend money or can't be undone.
const FINAL_WORDS: &[&str] = &[
    "pay", "buy", "purchase", "order", "checkout", "transfer", "donate", "book", "reserve",
    "delete", "remove", "trash", "withdraw", "tip",
];

/// Phrases that spend money or can't be undone.
const FINAL_PHRASES: &[&str] = &[
    "check out",
    "place order",
    "cancel order",
    "cancel subscription",
    "cancel booking",
    "cancel membership",
    "cancel plan",
    "delete forever",
    "close account",
    "send money",
];

/// Words on a button that clearly change something.
const CHANGE_WORDS: &[&str] = &[
    "send",
    "post",
    "publish",
    "reply",
    "comment",
    "archive",
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
    "upload",
    "schedule",
    "logout",
    "signout",
    "signup",
    "register",
    "join",
    "forward",
    "tweet",
    "react",
];

/// Phrases that clearly change something.
const CHANGE_PHRASES: &[&str] = &["sign out", "log out", "sign up", "mark as", "move to"];

/// Words that are a change only when no query word is next to them:
/// "Apply" asks, "Apply filters" doesn't.
const SOFT_CHANGE_WORDS: &[&str] = &["apply", "submit", "update", "done", "ok", "continue"];

/// Words on a button that look things up.
const QUERY_WORDS: &[&str] = &[
    "search", "find", "filter", "filters", "sort", "go", "refine", "results", "more", "next",
    "previous", "prev", "page", "show", "view", "load", "browse", "expand", "collapse", "clear",
];

/// Phrases that look things up even though a word in them alone wouldn't
/// ("Sort order" isn't an order). They're taken out before the other lists
/// are checked.
const QUERY_PHRASES: &[&str] = &[
    "sort order",
    "order by",
    "sort by",
    "remove filter",
    "remove filters",
    "remove all filters",
];

/// What the words of `name` say (see [`Words`]).
fn click_words(name: &str) -> Words {
    let name = name.to_lowercase();
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut text = format!(" {} ", words.join(" "));
    let mut query = false;
    for phrase in QUERY_PHRASES {
        let phrase = format!(" {phrase} ");
        if text.contains(&phrase) {
            text = text.replace(&phrase, " ");
            query = true;
        }
    }
    let has = |list: &[&str]| text.split_whitespace().any(|w| list.contains(&w));
    let has_phrase = |list: &[&str]| list.iter().any(|p| text.contains(&format!(" {p} ")));
    if has(FINAL_WORDS) || has_phrase(FINAL_PHRASES) {
        Words::Final
    } else if has(CHANGE_WORDS) || has_phrase(CHANGE_PHRASES) {
        Words::Hard
    } else if query || has(QUERY_WORDS) {
        Words::Query
    } else if has(SOFT_CHANGE_WORDS) {
        Words::Soft
    } else {
        Words::Nothing
    }
}

/// A name as a card shows it: trimmed, spaces collapsed, at most 80
/// characters.
fn shown_name(name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    match name.char_indices().nth(80) {
        Some((cut, _)) => format!("{}…", &name[..cut]),
        None => name,
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

/// Whether a snapshot label (`button "Post"`) or a plain label names a
/// change (or a can't-undo action). Typing fields never do.
fn is_change(label: &str) -> bool {
    let (role, name) = split_label(label);
    !matches!(role, "textbox" | "searchbox" | "combobox")
        && matches!(click_words(name), Words::Final | Words::Hard | Words::Soft)
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
pub(crate) fn is_inside(path: &Path, dir: &Path) -> bool {
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

// ---------------------------------------------------------------------------
// Practice runs (#21): one overlay on the live verdict, for both engines.
// ---------------------------------------------------------------------------

/// How strict a practice run's network guard is, per test case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrialMode {
    /// Every request but GET, HEAD and OPTIONS is blocked (the default).
    #[default]
    Strict,
    /// Pages may also read with POST from scripts (sites that search with
    /// GraphQL, say); form posts and every other write stay blocked. Toggles
    /// outside search areas and clicks oxbot can't name are recorded, not
    /// done.
    #[serde(rename = "xhr")]
    XhrAllowed,
}

impl TrialMode {
    /// Its name in the database and the protocol: `strict` or `xhr`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::XhrAllowed => "xhr",
        }
    }

    /// The mode called `name` (as [`TrialMode::as_str`] writes it).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "strict" => Some(Self::Strict),
            "xhr" => Some(Self::XhrAllowed),
            _ => None,
        }
    }
}

/// A step a practice run recorded instead of doing, e.g. `click button
/// "Send" on mail.example.com`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Intent {
    /// What kind of step (a change, an upload…).
    pub kind: ApprovalKind,
    /// The action, e.g. `click`.
    pub action: String,
    /// What it acts on, e.g. `button "Send"`.
    pub target: String,
    /// The site, e.g. `mail.example.com`.
    pub site: String,
    /// For a click: what kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<Click>,
    /// One of the bot's "Always allow" rules would have let it run live.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_rule: bool,
}

impl Intent {
    /// The step a card would have asked about.
    #[must_use]
    pub fn from_ask(ask: &Ask) -> Self {
        Self {
            kind: ask.kind,
            action: ask.action.clone(),
            target: ask.target.clone(),
            site: ask.site.clone(),
            click: ask.click,
            by_rule: false,
        }
    }

    /// One line: `click button "Send" on mail.example.com`.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.site.is_empty() || self.site == "oxbot" {
            format!("{} {}", self.action, self.target)
        } else {
            format!("{} {} on {}", self.action, self.target, self.site)
        }
    }

    /// Whether it changes something on a site (a change, a can't-undo
    /// click, Enter in a text box, an upload) rather than a toggle or a
    /// click oxbot couldn't name.
    #[must_use]
    pub fn is_change(&self) -> bool {
        matches!(self.click, Some(Click::Change | Click::Final) | None)
    }
}

/// What a practice run does with a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrialVerdict {
    /// Run it, as live.
    Run,
    /// Refuse it, as live.
    Refuse(String),
    /// Don't run it: record it, and tell the model it was done.
    Record(Intent),
}

/// The tool call a practice run checks, as far as the gate read it.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrialCall<'a> {
    /// The browser action (`click`, `click_at`…), if it's a browser tool.
    pub action: Option<&'a str>,
    /// The label of what a click aims at (`switch "Dark mode"`), if known.
    pub label: Option<&'a str>,
    /// What oxbot read about that element, if it looked.
    pub element: Option<&'a ElementFacts>,
    /// The page's site, e.g. `mail.example.com`.
    pub site: &'a str,
}

/// How a practice run treats a tool call whose live verdict is `verdict`.
/// Both engines use it, through the hub's gate:
///
/// - refused live: refused (and counted);
/// - a card live: recorded, never shown, whatever "Always allow" rules say;
/// - a click oxbot can't name: run in [`TrialMode::Strict`] (the network
///   guard blocks any write it makes), recorded in
///   [`TrialMode::XhrAllowed`];
/// - a switch, checkbox or radio outside a search area: recorded in
///   [`TrialMode::XhrAllowed`] (a script may save it with a POST);
/// - anything else: run.
#[must_use]
pub fn trial_overlay(verdict: &Verdict, call: TrialCall<'_>, mode: TrialMode) -> TrialVerdict {
    match verdict {
        Verdict::Deny(why) => TrialVerdict::Refuse(why.clone()),
        Verdict::Ask(ask) => TrialVerdict::Record(Intent::from_ask(ask)),
        Verdict::AllowUnnamed(what) => match mode {
            TrialMode::Strict => TrialVerdict::Run,
            TrialMode::XhrAllowed => TrialVerdict::Record(Intent {
                kind: ApprovalKind::Change,
                action: "click".to_owned(),
                target: what.clone(),
                site: call.site.to_owned(),
                click: Some(Click::Unknown),
                by_rule: false,
            }),
        },
        Verdict::Allow => {
            let toggle = mode == TrialMode::XhrAllowed
                && matches!(call.action, Some("click" | "click_at"))
                && call.label.is_some_and(|label| {
                    let (role, _) = split_label(label);
                    matches!(role, "switch" | "checkbox" | "radio")
                        && classify(Some(label), call.element) == Click::Plain
                })
                && !call.element.is_some_and(|e| e.in_search);
            if toggle {
                let label = call.label.unwrap_or_default();
                let (role, name) = split_label(label);
                TrialVerdict::Record(Intent {
                    kind: ApprovalKind::Change,
                    action: "click".to_owned(),
                    target: format!("{role} \"{}\"", shown_name(name)),
                    site: call.site.to_owned(),
                    click: Some(Click::Plain),
                    by_rule: false,
                })
            } else {
                TrialVerdict::Run
            }
        }
    }
}

/// What every practice-run result oxbot makes up starts with. The stand-in
/// model looks for it.
pub const DRY_RUN_PREFIX: &str = "[test run]";

/// What the model is told about a step a practice run recorded instead of
/// doing. The same words on every engine; nothing else in a practice run
/// says it's a test (the system prompt stays as live).
#[must_use]
pub fn dry_run_text(intent: &Intent) -> String {
    format!(
        "{DRY_RUN_PREFIX} oxbot recorded this instead of doing it: {}. This task is being replayed \
         as a test, so nothing was sent. Treat it as done and successful, don't retry it or check \
         for its effect, and carry on with the rest of the task.",
        intent.describe()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WORK: &str = "/bots/news/work";

    #[test]
    fn the_loop_never_lists_a_tool_policy_always_refuses() {
        for tool in LOOP_BROWSER_TOOLS {
            let input = json!({"uid": "9", "url": "https://example.com/", "type": "url",
                "key": "a", "value": "x", "text": "x", "elements": []});
            let verdict = decide(
                &format!("mcp__chrome-devtools__{tool}"),
                &input,
                ctx(),
                |_| None,
            );
            assert!(!matches!(verdict, Verdict::Deny(_)), "{tool}: {verdict:?}");
        }
        assert!(matches!(
            decide(
                "mcp__oxbot__ask_user",
                &json!({"question": "?"}),
                ctx(),
                |_| None
            ),
            Verdict::Allow
        ));
        // Every tool of the loop's own (to-do list, notes, stored results,
        // page tools) runs.
        let browser = vec![oxbot_llm::ToolSpec {
            name: "mcp__chrome-devtools__take_snapshot".into(),
            description: String::new(),
            parameters: json!({}),
        }];
        let own = oxbot_loop::ToolRegistry::new(Vec::new(), browser);
        for spec in own.specs() {
            if let Some(name) = spec.name.strip_prefix("mcp__oxbot__") {
                let verdict = decide(&spec.name, &json!({}), ctx(), |_| None);
                assert!(matches!(verdict, Verdict::Allow), "{name}: {verdict:?}");
            }
        }
    }

    #[test]
    fn file_calls_inside_the_folder_run_as_with_accept_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("bot/work");
        std::fs::create_dir_all(work.join("notes")).unwrap();
        let w = work.display().to_string();
        for (tool, input) in [
            ("Read", json!({"file_path": format!("{w}/notes/a.md")})),
            ("Read", json!({"file_path": "notes/a.md"})),
            ("Write", json!({"file_path": "new/b.md", "content": "x"})),
            ("Glob", json!({"pattern": "*.md"})),
            ("Grep", json!({"pattern": "x", "path": "notes"})),
        ] {
            assert!(file_call_inside(tool, &input, &work), "{tool} {input}");
        }
        std::os::unix::fs::symlink(tmp.path().join("bot"), work.join("up")).unwrap();
        for (tool, input) in [
            ("Read", json!({"file_path": "../bot.toml"})),
            ("Read", json!({"file_path": "/etc/hosts"})),
            ("Read", json!({"file_path": "up/bot.toml"})),
            ("Read", json!({})),
            ("Glob", json!({"pattern": "*", "path": ".."})),
            ("Write", json!({"file_path": 3})),
            ("Bash", json!({"command": "ls"})),
        ] {
            assert!(!file_call_inside(tool, &input, &work), "{tool} {input}");
        }
    }

    fn ctx() -> Context<'static> {
        Context {
            work_dir: Path::new(WORK),
            page_url: Some("https://example.com/jobs"),
            facts: None,
            element: None,
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
        assert_eq!(
            decide(
                "mcp__oxbot__save_report",
                &json!({"title":"Coffee","summary":"A","markdown":"A [1]","sources":[]}),
                ctx(),
                names
            ),
            Verdict::Allow,
            "a report stays inside oxbot"
        );
        assert!(needs_facts(&tool("press_key")) && !needs_facts(&tool("click")));
        assert!(needs_facts(&tool("click_at")));
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
        let script = facts_script(&json!({"x":10,"y":20}));
        assert!(script.contains("elementFromPoint(10, 20)"));
        assert!(script.contains(ELEMENT_FACTS_FN));
        let v = json!({"active":null,"passwords":[],"at":{"name":"Go","role":"button",
            "facts":{"tag":"button","type":"submit","submits":true,"method":"get","inSearch":false,"filled":0}}});
        let facts = PageFacts::from_value(&v);
        assert_eq!(facts.at_point.as_deref(), Some("button \"Go\""));
        assert!(facts.at_element.unwrap().submits);
        let old = PageFacts::from_value(&json!({"at":"Send"}));
        assert_eq!(old.at_point.as_deref(), Some("Send"));
    }

    fn click(label: &str, element: Option<&ElementFacts>) -> Verdict {
        let ctx = Context { element, ..ctx() };
        decide(&tool("click"), &json!({"uid":"7_1"}), ctx, |_| {
            Some(label.to_owned())
        })
    }

    fn submit(method: &str, in_search: bool, filled: u32) -> ElementFacts {
        ElementFacts {
            tag: "button".into(),
            input_type: "submit".into(),
            submits: true,
            form_method: Some(method.into()),
            in_search,
            filled_fields: filled,
        }
    }

    fn asks(verdict: &Verdict) -> Option<Click> {
        match verdict {
            Verdict::Ask(ask) => {
                assert_eq!(ask.kind, ApprovalKind::Change);
                ask.click
            }
            _ => None,
        }
    }

    #[test]
    fn unknown_uid_is_allowed() {
        assert_eq!(
            decide(&tool("click"), &json!({"uid":"8_242"}), ctx(), names),
            Verdict::AllowUnnamed("element 8_242".into())
        );
        assert_eq!(classify(None, None), Click::Unknown);
    }

    #[test]
    fn unknown_uid_that_resolves_to_pay_asks() {
        // The element book found the label oxbot's last snapshot didn't have.
        for label in [
            "button \"Book now\"",
            "button \"Pay\"",
            "link \"Delete forever\"",
        ] {
            assert_eq!(asks(&click(label, None)), Some(Click::Final), "{label}");
        }
        let Verdict::Ask(ask) = click("button \"Book now\" focusable", None) else {
            panic!("asks")
        };
        assert_eq!(ask.target, "button \"Book now\"");
    }

    #[test]
    fn search_and_filter_clicks_run() {
        for label in [
            "button \"Search\"",
            "button \"Apply filters\"",
            "button \"Sort by date\"",
            "button \"Sort order\"",
            "button \"Show more\"",
            "button \"Update results\"",
            "button \"Remove filter: Remote\"",
            "link \"Next\"",
            "link \"Senior engineer, Toronto\"",
            "tab \"Posts\"",
            "checkbox \"Full time\"",
            "option \"Newest first\"",
            "button \"Menu\"",
        ] {
            assert_eq!(click(label, None), Verdict::Allow, "{label}");
        }
    }

    #[test]
    fn changes_ask_and_money_or_cant_undo_is_final() {
        for label in [
            "button \"Post\"",
            "button \"Send\"",
            "button \"Save\"",
            "button \"Apply\"",
            "button \"Apply now\"",
            "button \"Submit\"",
            "button \"Sign up\"",
            "button \"Confirm\"",
            "link \"Unsubscribe\"",
        ] {
            assert_eq!(asks(&click(label, None)), Some(Click::Change), "{label}");
        }
        for label in [
            "button \"Pay now\"",
            "button \"Buy\"",
            "button \"Place order\"",
            "button \"Book\"",
            "button \"Delete\"",
            "button \"Transfer\"",
            "button \"Check out\"",
        ] {
            assert_eq!(asks(&click(label, None)), Some(Click::Final), "{label}");
        }
    }

    #[test]
    fn card_links_navigate_whatever_words_they_hold() {
        // A recipe card: the link's name is the whole card, "Save" included.
        for label in [
            "link \"Sliced banana bread loaf … Save Recipe BANANA NUT BREAD RECIP…\" url=\"https://recipes.example.com/r/42\"",
            "link \"How to delete your account in 5 steps\"",
            "link \"Book now: three nights by the lake from $199\"",
            "link \"Sam shared a post about the new library hours\"",
            "heading \"Roadmap: The Get-It-Together Guide for Figuring Out What To Do with Your Life (C…\"",
            "heading \"Save Recipe: Banana Nut Bread from the test kitchen\"",
            "heading \"Results\"",
        ] {
            assert_eq!(click(label, None), Verdict::Allow, "{label}");
        }
        // Short links with a change word still ask, as do buttons.
        for label in [
            "link \"Unsubscribe\"",
            "link \"Save recipe\"",
            "link \"Log out\"",
            "link \"Unsubscribe from all emails now\"",
            "button \"Save Recipe for banana nut bread loaf\"",
            "heading \"Save recipe\"",
        ] {
            assert_eq!(asks(&click(label, None)), Some(Click::Change), "{label}");
        }
        assert_eq!(
            asks(&click("link \"Delete\"", None)),
            Some(Click::Final),
            "a short can't-undo link"
        );
        assert_eq!(name_words("Save … Recipe |"), 2);
    }

    #[test]
    fn get_form_submit_runs_post_form_submit_asks() {
        let get = submit("get", false, 2);
        let search = submit("post", true, 1);
        let post = submit("post", false, 2);
        let empty_post = submit("post", false, 0);
        assert_eq!(click("button \"Submit\"", Some(&get)), Verdict::Allow);
        assert_eq!(click("button \"Submit\"", Some(&search)), Verdict::Allow);
        assert_eq!(click("button \"Go\"", Some(&search)), Verdict::Allow);
        assert_eq!(
            asks(&click("button \"Submit\"", Some(&post))),
            Some(Click::Change)
        );
        assert_eq!(
            asks(&click("button \"Send\"", Some(&get))),
            Some(Click::Change),
            "a clear change word wins over the form"
        );
        assert_eq!(
            asks(&click("button \"Pay\"", Some(&search))),
            Some(Click::Final),
            "and so does a can't-undo word"
        );
        let Verdict::Ask(ask) = click("button \"Jobs →\"", Some(&post)) else {
            panic!("a filled POST form's unclear button asks")
        };
        assert_eq!(ask.target, "the form's \"Jobs →\" button");
        assert_eq!(
            click("button \"Jobs →\"", Some(&empty_post)),
            Verdict::Allow
        );
        assert_eq!(click("button \"Jobs →\"", Some(&get)), Verdict::Allow);
        let not_submit = ElementFacts {
            submits: false,
            ..post.clone()
        };
        assert_eq!(
            click("button \"Jobs →\"", Some(&not_submit)),
            Verdict::Allow
        );
        assert_eq!(
            asks(&click("button \"Submit\"", Some(&not_submit))),
            Some(Click::Change),
            "an unclear change word stays a change unless its form is a query"
        );
    }

    #[test]
    fn only_unclear_buttons_are_looked_up() {
        assert!(wants_element_facts("button \"Submit\""));
        assert!(wants_element_facts("button \"Jobs →\" focusable"));
        for label in [
            "button \"Search\"",
            "button \"Post\"",
            "button \"Pay\"",
            "link \"Submit\"",
            "button \"\"",
            "textbox \"Name\"",
        ] {
            assert!(!wants_element_facts(label), "{label}");
        }
        let found = [submit("get", false, 0), submit("post", false, 3)];
        let strict = strictest("button \"Submit\"", &found).unwrap();
        assert_eq!(strict.form_method.as_deref(), Some("post"));
        assert_eq!(role_and_name("button \"Go\" focusable"), ("button", "Go"));
    }

    #[test]
    fn click_at_uses_element_facts() {
        let at = |label: &str, element: Option<ElementFacts>| PageFacts {
            at_point: Some(label.into()),
            at_element: element,
            ..PageFacts::default()
        };
        let input = json!({"x":5,"y":6});
        let go = at("button \"Submit\"", Some(submit("get", false, 1)));
        assert_eq!(
            decide(&tool("click_at"), &input, with_facts(&go), names),
            Verdict::Allow
        );
        let post = at("button \"Submit\"", Some(submit("post", false, 1)));
        assert!(matches!(
            decide(&tool("click_at"), &input, with_facts(&post), names),
            Verdict::Ask(_)
        ));
        assert_eq!(
            decide(
                &tool("click_at"),
                &input,
                with_facts(&PageFacts::default()),
                names
            ),
            Verdict::AllowUnnamed("the spot at (5, 6)".into())
        );
        assert!(matches!(
            decide(&tool("click_at"), &input, ctx(), names),
            Verdict::AllowUnnamed(_)
        ));
    }

    #[test]
    fn only_web_pages_can_be_opened() {
        for url in [
            // oxbot's own page and Gate door, and the home network.
            "http://127.0.0.1:7878/",
            "http://localhost:7878/",
            "https://192.168.2.10:7879/pair",
            "http://jays-mac.local/",
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

    /// The rule an Always answer would set for `verdict`, if it may offer one.
    fn rule_of(verdict: &Verdict) -> Option<String> {
        match verdict {
            Verdict::Ask(ask) => ask.rule.clone(),
            other => panic!("expected a card, got {other:?}"),
        }
    }

    #[test]
    fn final_words_never_offer_always() {
        for label in [
            "button \"Pay now\"",
            "button \"Buy\"",
            "button \"Place order\"",
            "button \"Book\"",
            "button \"Delete\"",
            "button \"Transfer\"",
        ] {
            assert_eq!(rule_of(&click(label, None)), None, "{label}");
        }
        assert_eq!(
            rule_of(&click("button \"Save\" focusable", None)).as_deref(),
            Some("button \"Save\"")
        );
        assert_eq!(
            rule_of(&click("button \"Apply\"", None)).as_deref(),
            Some("button \"Apply\"")
        );
        // An unclear POST-form button names the button itself, not the card's words.
        let post = submit("post", false, 1);
        let verdict = click("button \"Submit\"", Some(&post));
        let Verdict::Ask(ask) = &verdict else {
            panic!("a filled POST form asks")
        };
        assert_eq!(ask.target, "the form's \"Submit\" button");
        assert_eq!(ask.rule.as_deref(), Some("button \"Submit\""));
        // A page oxbot can't place on a web site gets no rule.
        let nowhere = Context {
            page_url: None,
            ..ctx()
        };
        let verdict = decide(&tool("click"), &json!({"uid":"1"}), nowhere, names);
        assert_eq!(rule_of(&verdict), None);
    }

    #[test]
    fn upload_create_give_enter_never_offer_always() {
        let enter = field(FieldKind::Text, "Message");
        let on_button = field(FieldKind::Other, "Send");
        let upload = json!({"uid":"1","filePaths": [format!("{WORK}/cv.pdf")]});
        for verdict in [
            decide(
                &tool("press_key"),
                &json!({"key":"Enter"}),
                with_facts(&enter),
                no_names,
            ),
            decide(
                &tool("press_key"),
                &json!({"key":"Enter"}),
                with_facts(&on_button),
                no_names,
            ),
            decide(&tool("press_key"), &json!({"key":"Enter"}), ctx(), no_names),
            decide(&tool("upload_file"), &upload, ctx(), names),
            decide(
                &tools::full_name(CREATE_BOT_TOOL),
                &json!({"name":"Helper","instructions":"Help."}),
                ctx(),
                no_names,
            ),
        ] {
            assert_eq!(rule_of(&verdict), None, "{verdict:?}");
        }
    }

    #[test]
    fn rule_label_normalizes() {
        assert_eq!(
            rule_label("button \"  Apply   filters \" focusable").as_deref(),
            Some("button \"Apply filters\"")
        );
        assert_eq!(rule_label("\"Save\"").as_deref(), Some("\"Save\""));
        assert_eq!(rule_label("button \"  \""), None);
        assert_ne!(rule_label("button \"save\""), rule_label("button \"Save\""));
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

#[cfg(test)]
mod trial_tests {
    //! The practice-run table (design §2.2): every row, in both network
    //! modes.
    use super::*;
    use serde_json::json;

    const WORK: &str = "/bots/jobs/work";

    fn tool(name: &str) -> String {
        format!("mcp__chrome-devtools__{name}")
    }

    fn names(uid: &str) -> Option<String> {
        match uid {
            "post" => Some("button \"Post\"".into()),
            "delete" => Some("button \"Delete forever\"".into()),
            "search" => Some("button \"Search\"".into()),
            "link" => Some("link \"Jobs in Riverside\"".into()),
            "switch" => Some("switch \"Remote only\"".into()),
            "box" => Some("checkbox \"Full time\"".into()),
            "password" => Some("textbox \"Password\"".into()),
            "what" => Some("textbox \"Job title\"".into()),
            _ => None,
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Want {
        Run,
        Refuse,
        Record,
    }

    struct Row {
        what: &'static str,
        tool: String,
        input: Value,
        facts: Option<PageFacts>,
        element: Option<ElementFacts>,
        strict: Want,
        xhr: Want,
    }

    fn row(what: &'static str, tool: String, input: Value, strict: Want, xhr: Want) -> Row {
        Row {
            what,
            tool,
            input,
            facts: None,
            element: None,
            strict,
            xhr,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one row per line of the design's table"
    )]
    fn table() -> Vec<Row> {
        use Want::{Record, Refuse, Run};
        let text_box = PageFacts {
            active: Some(Field {
                kind: FieldKind::Text,
                label: "Write a message".into(),
            }),
            ..PageFacts::default()
        };
        let search_area = ElementFacts {
            in_search: true,
            ..ElementFacts::default()
        };
        vec![
            row("read", tool("take_snapshot"), json!({}), Run, Run),
            row(
                "navigate",
                tool("navigate_page"),
                json!({"url": "https://jobs.example.test/"}),
                Run,
                Run,
            ),
            row(
                "type",
                tool("fill"),
                json!({"uid": "what", "value": "nurse"}),
                Run,
                Run,
            ),
            row("search", tool("click"), json!({"uid": "search"}), Run, Run),
            row("link", tool("click"), json!({"uid": "link"}), Run, Run),
            row(
                "toggle",
                tool("click"),
                json!({"uid": "switch"}),
                Run,
                Record,
            ),
            Row {
                element: Some(search_area),
                ..row(
                    "toggle in a search area",
                    tool("click"),
                    json!({"uid": "box"}),
                    Run,
                    Run,
                )
            },
            row("unnamed", tool("click"), json!({"uid": "x9"}), Run, Record),
            row(
                "change",
                tool("click"),
                json!({"uid": "post"}),
                Record,
                Record,
            ),
            row(
                "can't undo",
                tool("click"),
                json!({"uid": "delete"}),
                Record,
                Record,
            ),
            Row {
                facts: Some(text_box),
                ..row(
                    "enter in a text box",
                    tool("press_key"),
                    json!({"key": "Enter"}),
                    Record,
                    Record,
                )
            },
            row(
                "upload",
                tool("upload_file"),
                json!({"uid": "x1", "filePath": format!("{WORK}/cv.pdf")}),
                Record,
                Record,
            ),
            row(
                "password",
                tool("fill"),
                json!({"uid": "password", "value": "x"}),
                Refuse,
                Refuse,
            ),
            row(
                "script",
                tool("evaluate_script"),
                json!({"function": "() => 1"}),
                Refuse,
                Refuse,
            ),
            row(
                "file page",
                tool("navigate_page"),
                json!({"url": "file:///etc/passwd"}),
                Refuse,
                Refuse,
            ),
            row(
                "files outside the folder",
                tool("take_screenshot"),
                json!({"filePath": "/tmp/x.png"}),
                Refuse,
                Refuse,
            ),
            row(
                "create a bot",
                "mcp__oxbot__create_bot".into(),
                json!({"name": "Helper", "instructions": "help"}),
                Record,
                Record,
            ),
        ]
    }

    fn run(row: &Row, mode: TrialMode) -> TrialVerdict {
        let ctx = Context {
            work_dir: Path::new(WORK),
            page_url: Some("https://jobs.example.test/post"),
            facts: row.facts.as_ref(),
            element: row.element.as_ref(),
        };
        let verdict = decide(&row.tool, &row.input, ctx, names);
        let label = row.input.get("uid").and_then(Value::as_str).and_then(names);
        let call = TrialCall {
            action: browser_action(&row.tool),
            label: label.as_deref(),
            element: row.element.as_ref(),
            site: "jobs.example.test",
        };
        trial_overlay(&verdict, call, mode)
    }

    fn kind(v: &TrialVerdict) -> Want {
        match v {
            TrialVerdict::Run => Want::Run,
            TrialVerdict::Refuse(_) => Want::Refuse,
            TrialVerdict::Record(_) => Want::Record,
        }
    }

    #[test]
    fn every_row_in_both_modes() {
        for row in table() {
            for (mode, want) in [
                (TrialMode::Strict, row.strict),
                (TrialMode::XhrAllowed, row.xhr),
            ] {
                let got = run(&row, mode);
                assert_eq!(kind(&got), want, "{} in {mode:?}: {got:?}", row.what);
            }
        }
    }

    #[test]
    fn a_recorded_click_says_what_it_would_have_done() {
        let row = table().into_iter().find(|r| r.what == "change").unwrap();
        let TrialVerdict::Record(intent) = run(&row, TrialMode::Strict) else {
            panic!("recorded");
        };
        assert_eq!(
            intent.describe(),
            "click button \"Post\" on jobs.example.test"
        );
        assert_eq!(intent.click, Some(Click::Change));
        assert!(intent.is_change());
        let toggle = table().into_iter().find(|r| r.what == "toggle").unwrap();
        let TrialVerdict::Record(intent) = run(&toggle, TrialMode::XhrAllowed) else {
            panic!("recorded");
        };
        assert_eq!(
            intent.describe(),
            "click switch \"Remote only\" on jobs.example.test"
        );
        assert!(!intent.is_change());
    }

    #[test]
    fn the_dry_run_text_is_stable() {
        let intent = Intent {
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: "button \"Send\"".into(),
            site: "mail.example.com".into(),
            click: Some(Click::Change),
            by_rule: false,
        };
        assert_eq!(
            dry_run_text(&intent),
            "[test run] oxbot recorded this instead of doing it: click button \"Send\" on \
             mail.example.com. This task is being replayed as a test, so nothing was sent. Treat \
             it as done and successful, don't retry it or check for its effect, and carry on with \
             the rest of the task."
        );
        assert!(dry_run_text(&intent).starts_with(DRY_RUN_PREFIX));
    }

    #[test]
    fn modes_and_intents_round_trip() {
        for mode in [TrialMode::Strict, TrialMode::XhrAllowed] {
            assert_eq!(TrialMode::parse(mode.as_str()), Some(mode));
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(mode.as_str()));
        }
        assert_eq!(TrialMode::parse("loose"), None);
        let intent = Intent {
            kind: ApprovalKind::Upload,
            action: "upload a file to".into(),
            target: "element 3".into(),
            site: "jobs.example.test".into(),
            click: None,
            by_rule: true,
        };
        let json = serde_json::to_string(&intent).unwrap();
        assert_eq!(serde_json::from_str::<Intent>(&json).unwrap(), intent);
        assert!(json.contains(r#""kind":"upload""#), "{json}");
    }
}
