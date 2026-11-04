//! The loop's own tools: `ask_user` (ends the turn with a question),
//! `update_todo` (the model's plan, recited back now and then),
//! `note_progress` (notes that outlive a window) and `read_result` (reads a
//! stored result back by its handle).

use oxbot_engine::text::data_block;
use oxbot_llm::ToolSpec;
use serde_json::{Value, json};

use crate::budget::est;
use crate::results::ResultStore;
use crate::store::ResultKind;

/// The name models call `ask_user` by.
pub const ASK_USER: &str = "mcp__oxbot__ask_user";
/// The name models call `update_todo` by.
pub const UPDATE_TODO: &str = "mcp__oxbot__update_todo";
/// The name models call `note_progress` by.
pub const NOTE_PROGRESS: &str = "mcp__oxbot__note_progress";
/// The name models call `read_result` by.
pub const READ_RESULT: &str = "mcp__oxbot__read_result";

/// What the model is told when it asked the person something (the turn
/// ends; their answer is the next message).
pub const ASKED: &str = "Asked the person; waiting for them.";

/// Longest progress note.
pub const MAX_NOTE: usize = 600;
/// Most lines `read_result` shows at once.
const MAX_LINES: usize = 200;
/// Most matches `read_result` shows.
const MAX_HITS: usize = 20;
/// Lines shown around each match.
const AROUND: usize = 3;

/// `ask_user` as the model sees it.
#[must_use]
pub fn ask_user_spec() -> ToolSpec {
    ToolSpec {
        name: ASK_USER.to_owned(),
        description:
            "Ask the person one short question when you can't go on without their answer. \
            Your turn ends; their reply comes as the next message."
                .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The question, in one or two sentences."}
            },
            "required": ["question"]
        }),
    }
}

/// `update_todo` as the model sees it.
#[must_use]
pub fn update_todo_spec() -> ToolSpec {
    ToolSpec {
        name: UPDATE_TODO.to_owned(),
        description: "Keep a short to-do list for a task with several parts. Send new items, or \
            the id and new state of existing ones. Items can't be reworded or removed: mark one \
            dropped with a why, and add a new one."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "description": "New items, and state changes of existing ones.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "A short id, like t1."},
                            "text": {"type": "string", "description": "What to do (for new items)."},
                            "state": {"type": "string", "enum": ["todo", "doing", "done", "dropped"]},
                            "why": {"type": "string", "description": "Why it was dropped."}
                        },
                        "required": ["id", "state"]
                    }
                }
            },
            "required": ["items"]
        }),
    }
}

/// `note_progress` as the model sees it.
#[must_use]
pub fn note_progress_spec() -> ToolSpec {
    ToolSpec {
        name: NOTE_PROGRESS.to_owned(),
        description: "Leave yourself a short note on where a long task stands (what's done, \
            what's next, what to watch out for). It's shown again if the conversation starts \
            over. At most 600 characters."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "The note."}
            },
            "required": ["text"]
        }),
    }
}

/// `read_result` as the model sees it.
#[must_use]
pub fn read_result_spec() -> ToolSpec {
    ToolSpec {
        name: READ_RESULT.to_owned(),
        description: "Read a result oxbot stored (its handle, like r12, is in the result or its \
            stub). Without from_line or find it shows the start. find: plain words, any case; \
            shows each matching line with 3 lines around it (at most 20 matches). For web \
            pages, find_in_page and read_section work better."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "handle": {"type": "string", "description": "The handle, like r12."},
                "from_line": {"type": "integer", "description": "First line to show, from 1."},
                "lines": {"type": "integer", "description": "How many lines (at most 200)."},
                "find": {"type": "string", "description": "Text to look for."}
            },
            "required": ["handle"]
        }),
    }
}

/// A progress note from the model's arguments, cut to [`MAX_NOTE`].
///
/// # Errors
/// What to tell the model.
pub fn progress_note(args: &Value) -> Result<String, String> {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or("note_progress needs text.")?;
    Ok(text.chars().take(MAX_NOTE).collect())
}

/// What `read_result` shows: the text and whether it's outside text.
///
/// # Errors
/// What to tell the model.
pub fn read_result(
    results: &ResultStore,
    args: &Value,
    cap_tokens: u64,
) -> Result<(String, Option<String>), String> {
    let handle = args
        .get("handle")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let (row, text) = results.get(handle)?;
    if row.kind == ResultKind::Rows {
        return Err(format!(
            "{} is a table read off a page; use query_rows(data: \"{}\") to see its rows.",
            row.handle, row.handle
        ));
    }
    let lines: Vec<&str> = text.lines().collect();
    let find = args
        .get("find")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| !f.is_empty());
    // A Read result has its own line numbers already.
    let numbered = row.tool != "Read";
    let shown = if let Some(find) = find {
        find_lines(&lines, find, &row.handle, numbered)
    } else {
        range_lines(&lines, args, &row.handle, numbered)
    };
    let shown = cut_lines(&shown, cap_tokens);
    let hint = if row.kind == ResultKind::Page {
        "\n(This is a web page: find_in_page and read_section show its structure.)"
    } else {
        ""
    };
    let attrs = format!(" handle=\"{}\"", row.handle);
    let body = data_block("stored_result", &attrs, &format!("{shown}{hint}"));
    Ok((body, row.tainted_by))
}

fn number(i: usize, line: &str, numbered: bool) -> String {
    if numbered {
        format!("{}: {line}", i + 1)
    } else {
        (*line).to_owned()
    }
}

/// Lines with `find` (any case), 3 lines around each.
fn find_lines(lines: &[&str], find: &str, handle: &str, numbered: bool) -> String {
    let needle = find.to_lowercase();
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();
    if hits.is_empty() {
        return format!("No line of {handle} has \"{find}\".");
    }
    let mut out = vec![format!(
        "{} matches for \"{find}\" in {handle}{}:",
        hits.len(),
        if hits.len() > MAX_HITS {
            format!(" (the first {MAX_HITS})")
        } else {
            String::new()
        }
    )];
    let mut shown_to = 0usize;
    for &hit in hits.iter().take(MAX_HITS) {
        let from = hit.saturating_sub(AROUND).max(shown_to);
        let to = (hit + AROUND + 1).min(lines.len());
        if from > shown_to && shown_to > 0 {
            out.push("…".to_owned());
        }
        for (i, line) in lines.iter().enumerate().take(to).skip(from) {
            out.push(number(i, line, numbered));
        }
        shown_to = to;
    }
    out.join("\n")
}

/// Lines `from_line` on, at most `lines` of them.
fn range_lines(lines: &[&str], args: &Value, handle: &str, numbered: bool) -> String {
    let from = args
        .get("from_line")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(1)
        .max(1)
        - 1;
    let count = args
        .get("lines")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(MAX_LINES)
        .clamp(1, MAX_LINES);
    if from >= lines.len() {
        return format!("{handle} has {} lines.", lines.len());
    }
    let to = (from + count).min(lines.len());
    let mut out: Vec<String> = lines[from..to]
        .iter()
        .enumerate()
        .map(|(i, l)| number(from + i, l, numbered))
        .collect();
    if to < lines.len() {
        out.push(format!(
            "({} lines in all; from_line: {} for more)",
            lines.len(),
            to + 1
        ));
    }
    out.join("\n")
}

/// `text` cut at a line break to at most `cap_tokens`.
#[must_use]
pub fn cut_lines(text: &str, cap_tokens: u64) -> String {
    if est(text) <= cap_tokens {
        return text.to_owned();
    }
    let mut out = String::new();
    for line in text.lines() {
        if est(&out) + est(line) + 1 > cap_tokens.saturating_sub(20) {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("(… cut to fit; ask for fewer lines or use find)");
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::results::NewResult;
    use crate::store::MemoryLoopStore;

    #[test]
    fn read_result_pages_finds_and_keeps_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let results = ResultStore::new(Arc::new(MemoryLoopStore::new()), "scout", "s1", tmp.path());
        let mut text = String::new();
        for i in 1..=500 {
            text.push_str("line ");
            text.push_str(&i.to_string());
            text.push('\n');
        }
        results
            .put(&NewResult {
                turn: 0,
                step: 1,
                tool: "Grep",
                kind: ResultKind::Text,
                page_url: None,
                page_title: None,
                tainted_by: Some("a file (notes.md)".into()),
                text: &text,
            })
            .unwrap();
        let (start, why) = read_result(&results, &json!({"handle": "r1"}), 4_000).unwrap();
        assert_eq!(why.as_deref(), Some("a file (notes.md)"));
        assert!(
            start.starts_with("<stored_result handle=\"r1\">1: line 1\n"),
            "{start}"
        );
        assert!(start.contains("200: line 200\n(500 lines in all; from_line: 201 for more)"));
        let (part, _) = read_result(
            &results,
            &json!({"handle": "r1", "from_line": 499, "lines": 5}),
            4_000,
        )
        .unwrap();
        assert!(part.contains("499: line 499\n500: line 500<"), "{part}");
        let (found, _) = read_result(
            &results,
            &json!({"handle": "r1", "find": "LINE 250"}),
            4_000,
        )
        .unwrap();
        assert!(
            found.contains("1 matches for \"LINE 250\" in r1:\n247: line 247"),
            "{found}"
        );
        assert!(found.contains("253: line 253<"));
        let (many, _) =
            read_result(&results, &json!({"handle": "r1", "find": "line 1"}), 4_000).unwrap();
        assert!(many.contains("(the first 20)"));
        // A small cap cuts at a line.
        let (cut, _) = read_result(&results, &json!({"handle": "r1"}), 100).unwrap();
        assert!(cut.contains("(… cut to fit"));
        assert!(est(&cut) < 140);
        assert!(read_result(&results, &json!({"handle": "../x"}), 4_000).is_err());
    }

    #[test]
    fn notes_are_cut() {
        assert_eq!(progress_note(&json!({"text": " x "})).unwrap(), "x");
        assert_eq!(
            progress_note(&json!({"text": "y".repeat(900)}))
                .unwrap()
                .len(),
            600
        );
        assert!(progress_note(&json!({})).is_err());
    }
}
