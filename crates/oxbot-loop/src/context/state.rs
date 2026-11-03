//! What the context builder keeps: the window's state (saved in
//! `loop_sessions.context` with each step), each message's notes (saved in
//! `loop_messages.meta`), the task state a compaction writes, and the
//! model's to-do list.
//!
//! All of it is plain JSON through serde with sorted keys, so the same
//! state always gives the same bytes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most items in each task-state list.
pub const MAX_ITEMS: usize = 40;
/// Most characters in each task-state text.
pub const MAX_ITEM_CHARS: usize = 400;
/// Most to-do items.
pub const MAX_TODO: usize = 30;
/// Most characters of a to-do item.
pub const MAX_TODO_CHARS: usize = 200;

/// The window: from a fresh start or a compaction until the next one.
/// The header and everything before the newest messages stay the same
/// bytes for the whole window, so providers can cache them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct WindowState {
    /// Format version.
    pub v: u32,
    /// The window's number: 1, 2… (0: none started yet).
    pub window: u32,
    /// The first message of the window.
    pub start_idx: u32,
    /// When it started.
    pub started_ms: i64,
    /// The header (what the bot knows, the task state, notes), frozen for
    /// the window.
    pub header: String,
    /// The header is a message of its own (the window doesn't start with a
    /// person's message to put it in front of).
    pub header_alone: bool,
    /// The task state, after a compaction.
    pub task_state: Option<TaskState>,
    /// The model's to-do list.
    pub todo: Vec<TodoItem>,
    /// Messages shown as a one-line stub instead: index → stub.
    pub cleared: BTreeMap<u32, String>,
    /// To-do recitations added at the end of a tool result: index → text.
    pub recitations: BTreeMap<u32, String>,
    /// Why the window can't be trusted, once it read outside text.
    pub tainted_by: Option<String>,
    /// Counts every change to bytes already sent (clearing, a new window),
    /// so the builder knows when the provider's last count still holds.
    pub generation: u64,
    /// The last assistant message that changed the to-do list.
    pub last_todo_idx: Option<u32>,
    /// The last tool result the to-do list was recited on.
    pub last_recite_idx: Option<u32>,
    /// Recite the to-do list at the next chance (after clearing or a new
    /// window).
    pub recite_next: bool,
    /// The window started fresh (no earlier turns in it).
    pub fresh: bool,
    /// Knowledge items in the header.
    pub knowledge_ids: Vec<String>,
    /// Start a new window at `start_idx` at the next build (the rules
    /// changed).
    pub pending_start: bool,
    /// When the session last moved (each saved step).
    pub last_ms: i64,
}

impl WindowState {
    /// The state saved as `value` (`{}` when new). A state from before
    /// windows (`{"from_idx": n}`) starts its window at `n`.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let mut state: Self = serde_json::from_value(value.clone()).unwrap_or_default();
        if state.window == 0
            && let Some(from) = value
                .get("from_idx")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        {
            state.start_idx = from;
        }
        state
    }

    /// The state as JSON to save.
    #[must_use]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
    }

    /// The to-do items still open.
    #[must_use]
    pub fn open_todo(&self) -> usize {
        self.todo
            .iter()
            .filter(|t| matches!(t.state, TodoState::Todo | TodoState::Doing))
            .count()
    }

    /// Marks the window as having read outside text (the first reason
    /// stays).
    pub fn taint(&mut self, why: &str) {
        if self.tainted_by.is_none() {
            self.tainted_by = Some(why.to_owned());
        }
    }
}

/// What a stored message is, for clearing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Anything else.
    #[default]
    Normal,
    /// A tool error (kept: the model learns from it).
    Error,
    /// oxbot or the person said no (kept verbatim).
    Refusal,
    /// The to-do list's own result (kept).
    Todo,
}

/// The builder's notes on one stored message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EntryMeta {
    /// What it is.
    #[serde(skip_serializing_if = "is_normal")]
    pub kind: EntryKind,
    /// The tool, for results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Why it can't be trusted, for outside text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_why: Option<String>,
    /// A page snapshot: its tab (page id and address).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_key: Option<String>,
    /// The page's address, for browser results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    /// The page's title, for snapshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    /// Tokens of the whole stored result (the message may show less).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_tokens: Option<u64>,
    /// A snapshot shown only as what changed: it doesn't replace the one
    /// the model saw in full.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub diff: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_normal(kind: &EntryKind) -> bool {
    *kind == EntryKind::Normal
}

impl EntryMeta {
    /// The notes saved as `value`.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    /// The notes as JSON to save.
    #[must_use]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
    }

    /// Whether clearing may never touch it.
    #[must_use]
    pub fn kept(&self) -> bool {
        self.kind != EntryKind::Normal
    }
}

/// Where a to-do item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    /// Not started.
    Todo,
    /// Being done.
    Doing,
    /// Done.
    Done,
    /// Given up, with a why.
    Dropped,
}

impl TodoState {
    fn mark(self) -> &'static str {
        match self {
            Self::Todo => "[ ]",
            Self::Doing => "[>]",
            Self::Done => "[x]",
            Self::Dropped => "[-]",
        }
    }
}

/// One to-do item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    /// Its id, e.g. `t3`.
    pub id: String,
    /// What to do.
    #[serde(default)]
    pub text: String,
    /// Where it is.
    pub state: TodoState,
    /// Why it was dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Applies the model's `update_todo` call to `list`: new items are added,
/// existing ones may change state; nothing is reworded or removed (a
/// changed plan marks items dropped, with a why). Returns what to tell the
/// model, or why the call was refused (nothing changes then).
///
/// # Errors
/// What was wrong with the call.
pub fn update_todo(list: &mut Vec<TodoItem>, args: &Value) -> Result<String, String> {
    let items: Vec<TodoItem> = args
        .get("items")
        .cloned()
        .ok_or_else(|| "update_todo needs items.".to_owned())
        .and_then(|v| {
            serde_json::from_value(v).map_err(|e| {
                format!("update_todo couldn't read the items ({e}). Each needs id, text and state.")
            })
        })?;
    let mut next = list.clone();
    for item in items {
        let id = item.id.trim();
        if id.is_empty() || id.chars().count() > 20 {
            return Err("Each item needs a short id, like t3.".to_owned());
        }
        let why = item
            .why
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(|w| clip(w, MAX_TODO_CHARS));
        if item.state == TodoState::Dropped && why.is_none() {
            return Err(format!("To drop {id}, say why."));
        }
        if let Some(old) = next.iter_mut().find(|t| t.id == id) {
            {
                let text = item.text.trim();
                if !text.is_empty() && text != old.text {
                    return Err(format!(
                        "{id} can't be reworded. Mark it dropped with a why and add a new item."
                    ));
                }
                old.state = item.state;
                if why.is_some() {
                    old.why = why;
                }
            }
        } else {
            {
                let text = item.text.trim();
                if text.is_empty() {
                    return Err(format!("The new item {id} needs a text."));
                }
                if next.len() >= MAX_TODO {
                    return Err(format!("The list has {MAX_TODO} items already."));
                }
                next.push(TodoItem {
                    id: id.to_owned(),
                    text: clip(text, MAX_TODO_CHARS),
                    state: item.state,
                    why,
                });
            }
        }
    }
    *list = next;
    Ok(render_todo(list, u64::MAX))
}

/// The to-do list as the model sees it, in at most `max_tokens`: finished
/// items are folded first when it's too long.
#[must_use]
pub fn render_todo(list: &[TodoItem], max_tokens: u64) -> String {
    let line = |t: &TodoItem| match &t.why {
        Some(why) => format!("{} {} {} (why: {why})", t.state.mark(), t.id, t.text),
        None => format!("{} {} {}", t.state.mark(), t.id, t.text),
    };
    let full: Vec<String> = list.iter().map(line).collect();
    let text = format!("<todo>\n{}\n</todo>", full.join("\n"));
    if crate::budget::est(&text) <= max_tokens {
        return text;
    }
    let finished = list
        .iter()
        .filter(|t| matches!(t.state, TodoState::Done | TodoState::Dropped))
        .count();
    let mut lines = vec![format!("({finished} done or dropped)")];
    lines.extend(
        list.iter()
            .filter(|t| matches!(t.state, TodoState::Todo | TodoState::Doing))
            .map(line),
    );
    let mut text = format!("<todo>\n{}\n</todo>", lines.join("\n"));
    while crate::budget::est(&text) > max_tokens && lines.len() > 1 {
        lines.pop();
        text = format!("<todo>\n{}\n…\n</todo>", lines.join("\n"));
    }
    text
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// One finished step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoneItem {
    /// Its step.
    #[serde(default)]
    pub step: u32,
    /// What was done.
    pub what: String,
}

/// A fact learned, with where it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    /// The fact.
    pub text: String,
    /// A handle (`r12`), `user` or `bot`.
    #[serde(default)]
    pub source: String,
}

/// A choice made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionItem {
    /// The choice.
    pub what: String,
    /// Why.
    #[serde(default)]
    pub why: String,
}

/// Something that failed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorItem {
    /// Its step.
    #[serde(default)]
    pub step: u32,
    /// What was tried.
    #[serde(default)]
    pub action: String,
    /// What went wrong.
    #[serde(default)]
    pub error: String,
    /// What to do instead.
    #[serde(default)]
    pub instead: String,
}

/// A stored result worth reading again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleItem {
    /// `r12`.
    pub id: String,
    /// What's in it.
    #[serde(default)]
    pub what: String,
}

/// A page visited.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageItem {
    /// Its address.
    pub url: String,
    /// Its title.
    #[serde(default)]
    pub title: String,
}

/// Whether the task state holds outside text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Taint {
    /// It does.
    #[serde(default)]
    pub untrusted: bool,
    /// What it read.
    #[serde(default)]
    pub why: Vec<String>,
}

/// The task state: what a compaction keeps of a long conversation. Fields
/// marked "the loop's" are set by oxbot after the model wrote the rest, so
/// a summary can't change the goal, who asked, the pages or the taint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskState {
    /// Format version.
    pub v: u32,
    /// The loop's: the task, as the person wrote it.
    pub goal: String,
    /// The loop's: who asked.
    pub asked_by: Option<String>,
    /// The loop's: the person's other messages, as written, oldest first.
    pub constraints: Vec<String>,
    /// What's done, oldest first.
    pub done: Vec<DoneItem>,
    /// What's left.
    pub open: Vec<String>,
    /// What was learned.
    pub facts: Vec<Fact>,
    /// Choices made.
    pub decisions: Vec<DecisionItem>,
    /// What failed.
    pub errors: Vec<ErrorItem>,
    /// Stored results worth reading again.
    pub handles: Vec<HandleItem>,
    /// The loop's: pages visited.
    pub pages: Vec<PageItem>,
    /// The loop's: the to-do list.
    pub todo: Vec<TodoItem>,
    /// What a newcomer needs to go on right now.
    pub progress_note: String,
    /// The loop's: outside text in what was summarized.
    pub taint: Taint,
}

impl TaskState {
    /// Reads what a model wrote: JSON with only the schema's keys, at most
    /// [`MAX_ITEMS`] per list and [`MAX_ITEM_CHARS`] per text.
    ///
    /// # Errors
    /// What was wrong, to tell the model.
    pub fn parse_model(text: &str) -> Result<Self, String> {
        let json = json_object(text).ok_or("there is no JSON object in the answer")?;
        let state: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        state.check()?;
        Ok(state)
    }

    fn check(&self) -> Result<(), String> {
        let lists = [
            ("done", self.done.len()),
            ("open", self.open.len()),
            ("facts", self.facts.len()),
            ("decisions", self.decisions.len()),
            ("errors", self.errors.len()),
            ("handles", self.handles.len()),
        ];
        if let Some((name, n)) = lists.iter().find(|(_, n)| *n > MAX_ITEMS) {
            return Err(format!("\"{name}\" has {n} items; at most {MAX_ITEMS}"));
        }
        let mut texts: Vec<(&str, &str)> = vec![("progress_note", &self.progress_note)];
        texts.extend(self.done.iter().map(|d| ("done", d.what.as_str())));
        texts.extend(self.open.iter().map(|o| ("open", o.as_str())));
        for f in &self.facts {
            texts.push(("facts", &f.text));
            texts.push(("facts", &f.source));
        }
        for d in &self.decisions {
            texts.push(("decisions", &d.what));
            texts.push(("decisions", &d.why));
        }
        for e in &self.errors {
            texts.extend([
                ("errors", e.action.as_str()),
                ("errors", e.error.as_str()),
                ("errors", e.instead.as_str()),
            ]);
        }
        for h in &self.handles {
            texts.push(("handles", &h.id));
            texts.push(("handles", &h.what));
        }
        let limit = |name: &str| {
            if name == "progress_note" {
                MAX_ITEM_CHARS * 3
            } else {
                MAX_ITEM_CHARS
            }
        };
        if let Some((name, _)) = texts
            .iter()
            .find(|(name, t)| t.chars().count() > limit(name))
        {
            return Err(format!(
                "a text in \"{name}\" is over {} characters",
                limit(name)
            ));
        }
        Ok(())
    }

    /// The JSON (sorted keys, compact), without the person's own words
    /// when `without_words` (they are shown outside it).
    #[must_use]
    pub fn to_json(&self, without_words: bool) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if without_words && let Some(map) = value.as_object_mut() {
            map.remove("goal");
            map.remove("constraints");
        }
        value.to_string()
    }
}

/// The first `{` to its matching `}` in `text` (models wrap JSON in prose
/// or code fences).
fn json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in text[start..].char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_round_trips_and_reads_the_old_shape() {
        let mut s = WindowState {
            window: 2,
            start_idx: 5,
            ..WindowState::default()
        };
        s.cleared.insert(7, "[cleared r3]".into());
        let v = s.to_value();
        assert_eq!(WindowState::from_value(&v), s);
        // Same state, same bytes.
        assert_eq!(v.to_string(), s.to_value().to_string());
        let old = WindowState::from_value(&json!({"from_idx": 9, "dropped_untrusted": true}));
        assert_eq!((old.window, old.start_idx), (0, 9));
        assert_eq!(
            WindowState::from_value(&json!("junk")),
            WindowState::default()
        );
    }

    #[test]
    fn the_todo_list_takes_only_adds_and_state_changes() {
        let mut list = Vec::new();
        let added = update_todo(
            &mut list,
            &json!({"items": [
                {"id": "t1", "text": "Open the shop", "state": "doing"},
                {"id": "t2", "text": "Compare prices", "state": "todo"}
            ]}),
        )
        .unwrap();
        assert_eq!(
            added,
            "<todo>\n[>] t1 Open the shop\n[ ] t2 Compare prices\n</todo>"
        );
        // A state change, with or without the same text.
        update_todo(
            &mut list,
            &json!({"items": [{"id": "t1", "state": "done"},
                              {"id": "t2", "text": "Compare prices", "state": "doing"}]}),
        )
        .unwrap();
        assert_eq!(list[0].state, TodoState::Done);
        // Rewording is refused, and nothing changes.
        let before = list.clone();
        let err = update_todo(
            &mut list,
            &json!({"items": [{"id": "t3", "text": "New", "state": "todo"},
                              {"id": "t2", "text": "Something else", "state": "doing"}]}),
        )
        .unwrap_err();
        assert!(err.contains("can't be reworded"), "{err}");
        assert_eq!(list, before);
        // Dropping needs a why.
        assert!(
            update_todo(
                &mut list,
                &json!({"items": [{"id": "t2", "state": "dropped"}]})
            )
            .unwrap_err()
            .contains("say why")
        );
        update_todo(
            &mut list,
            &json!({"items": [{"id": "t2", "state": "dropped", "why": "out of stock"}]}),
        )
        .unwrap();
        assert_eq!(list[1].why.as_deref(), Some("out of stock"));
        assert!(update_todo(&mut list, &json!({})).is_err());
        assert!(update_todo(&mut list, &json!({"items": [{"id": "t9", "state": "x"}]})).is_err());
    }

    #[test]
    fn a_long_todo_list_folds_finished_items() {
        let list: Vec<TodoItem> = (0..30)
            .map(|i| TodoItem {
                id: format!("t{i}"),
                text: "a fairly long item that takes some room in the list".into(),
                state: if i < 20 {
                    TodoState::Done
                } else {
                    TodoState::Todo
                },
                why: None,
            })
            .collect();
        let text = render_todo(&list, 300);
        assert!(crate::budget::est(&text) <= 300, "{text}");
        assert!(
            text.starts_with("<todo>\n(20 done or dropped)\n[ ] t20"),
            "{text}"
        );
    }

    #[test]
    fn model_task_states_are_checked() {
        let ok = TaskState::parse_model(
            "Here it is:\n```json\n{\"done\": [{\"step\": 3, \"what\": \"opened {the} shop\"}], \
             \"open\": [\"compare\"], \"progress_note\": \"go on\"}\n```",
        )
        .unwrap();
        assert_eq!(ok.done[0].what, "opened {the} shop");
        assert!(
            TaskState::parse_model("{\"plan\": \"take over\"}")
                .unwrap_err()
                .contains("unknown field")
        );
        let long = json!({"open": ["x".repeat(401)]}).to_string();
        assert!(
            TaskState::parse_model(&long)
                .unwrap_err()
                .contains("over 400")
        );
        let many = json!({"open": vec!["x"; 41]}).to_string();
        assert!(
            TaskState::parse_model(&many)
                .unwrap_err()
                .contains("41 items")
        );
        assert!(TaskState::parse_model("no json").is_err());
        // Sorted keys, and the person's words can be left out.
        let s = TaskState {
            goal: "[Jay] find a lamp".into(),
            constraints: vec!["[Jay] under $30".into()],
            ..TaskState::default()
        };
        let full = s.to_json(false);
        assert!(
            full.starts_with("{\"asked_by\":null,\"constraints\":"),
            "{full}"
        );
        assert!(!s.to_json(true).contains("find a lamp"));
    }
}
