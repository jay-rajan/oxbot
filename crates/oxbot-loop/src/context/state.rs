//! Each stored result's notes (what it is, its tool and page, whether it's
//! outside text) and the model's to-do list.
//!
//! All of it is plain JSON through serde with sorted keys, so the same
//! state always gives the same bytes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most to-do items.
pub const MAX_TODO: usize = 30;
/// Most characters of a to-do item.
pub const MAX_TODO_CHARS: usize = 200;

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
    /// oxbot's note asking the model to check its answer: a user-role
    /// message that is never the person's words.
    Check,
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

    /// Whether clearing may never touch it: errors, refusals, the to-do
    /// list, and rows (small, exact, and what answers are counted from).
    #[must_use]
    pub fn kept(&self) -> bool {
        self.kind != EntryKind::Normal
            || self
                .tool
                .as_deref()
                .is_some_and(|t| crate::tools::page::ROWS_TOOLS.contains(&t))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
