//! Only oxbot's tools reach Codex: a model's answer that calls any other
//! tool (Codex's own `apply_patch` or `shell`, say, which oxbot never
//! offers) can't make Codex run it, streamed or whole.
//!
//! A `function_call` or `custom_tool_call` whose name is one of the tools
//! the request offered (see [`crate::shape::offered`]) passes as it is.
//! One to any other tool is renamed (`unavailable_tool_apply_patch`), so
//! Codex answers the model that it has no such tool and the turn goes on.
//! Any other kind of call item (`local_shell_call`, …) is left out, with
//! every event about it, and taken out of the finished response's
//! `output`. Messages and reasoning pass as they are.

use std::collections::HashSet;

use serde_json::Value;

/// What a renamed call is called: this, then its own name.
pub const UNAVAILABLE: &str = "unavailable_tool_";

/// What happens to one output item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// It passes.
    Keep,
    /// A call to a tool the request didn't offer: renamed.
    Rename,
    /// Another kind of call: left out.
    Drop,
}

/// Neutralizes calls to tools the request didn't offer in one answer.
#[derive(Debug)]
pub struct CallFilter {
    allowed: HashSet<String>,
    /// Ids and output positions of the items renamed so far.
    renamed_ids: HashSet<String>,
    renamed_at: HashSet<u64>,
    /// Ids and output positions of the items dropped so far.
    dropped_ids: HashSet<String>,
    dropped_at: HashSet<u64>,
    /// A streamed answer's bytes not yet a whole event.
    buf: Vec<u8>,
}

impl CallFilter {
    /// For an answer to a request that offered the tools named `allowed`.
    #[must_use]
    pub fn new(allowed: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed: allowed.into_iter().collect(),
            renamed_ids: HashSet::new(),
            renamed_at: HashSet::new(),
            dropped_ids: HashSet::new(),
            dropped_at: HashSet::new(),
            buf: Vec::new(),
        }
    }

    /// What happens to `item` (an output item).
    fn fate(&self, item: &Value) -> Fate {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        let name = item.get("name").and_then(Value::as_str);
        match kind {
            "function_call" | "custom_tool_call" => {
                if name.is_some_and(|n| self.allowed.contains(n)) {
                    Fate::Keep
                } else {
                    Fate::Rename
                }
            }
            other if other.ends_with("_call") => Fate::Drop,
            _ => Fate::Keep,
        }
    }

    /// Applies `item`'s fate (at `index`, if known): `false` if it goes.
    fn apply(&mut self, item: &mut Value, index: Option<u64>) -> bool {
        let fate = self.fate(item);
        if fate == Fate::Keep {
            return true;
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        tracing::warn!(kind, name, "the model called a tool oxbot didn't offer");
        let id = item.get("id").and_then(Value::as_str).map(str::to_owned);
        let (ids, at) = if fate == Fate::Rename {
            (&mut self.renamed_ids, &mut self.renamed_at)
        } else {
            (&mut self.dropped_ids, &mut self.dropped_at)
        };
        ids.extend(id);
        at.extend(index);
        if fate == Fate::Rename {
            item["name"] = Value::String(unavailable(&name));
            true
        } else {
            false
        }
    }

    /// Applies each item's fate in a response's `output`. Whether it
    /// changed.
    fn clean_output(&mut self, response: &mut Value) -> bool {
        let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) else {
            return false;
        };
        let mut items = std::mem::take(output);
        let before = items.clone();
        items.retain_mut(|item| self.apply(item, None));
        let changed = items != before;
        if let Some(output) = response.get_mut("output") {
            *output = Value::Array(items);
        }
        changed
    }

    /// One streamed event: `None` to leave it out, else the event (changed
    /// or not) and whether it changed.
    fn event(&mut self, mut event: Value) -> Option<(Value, bool)> {
        let index = event.get("output_index").and_then(Value::as_u64);
        if let Some(item) = event.get_mut("item") {
            let before = item.clone();
            let mut item = before.clone();
            if !self.apply(&mut item, index) {
                return None;
            }
            let changed = item != before;
            event["item"] = item;
            return Some((event, changed));
        }
        let id = event.get("item_id").and_then(Value::as_str);
        let about = |ids: &HashSet<String>, at: &HashSet<u64>| {
            id.is_some_and(|id| ids.contains(id)) || index.is_some_and(|i| at.contains(&i))
        };
        if about(&self.dropped_ids, &self.dropped_at) {
            return None;
        }
        let mut changed = false;
        if about(&self.renamed_ids, &self.renamed_at)
            && let Some(name) = event.get("name").and_then(Value::as_str)
            && !self.allowed.contains(name)
        {
            event["name"] = Value::String(unavailable(name));
            changed = true;
        }
        if let Some(response) = event.get_mut("response") {
            changed |= self.clean_output(response);
        }
        Some((event, changed))
    }

    /// Takes the next bytes of a streamed answer (server-sent events);
    /// gives what Codex gets now.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some((end, blank)) = frame_end(&self.buf) {
            let frame: Vec<u8> = self.buf.drain(..end + blank).collect();
            self.frame(&frame, end, &mut out);
        }
        out
    }

    /// The answer ended: whatever is left (an unfinished event passes on
    /// as it is only if it isn't about a dropped call).
    pub fn finish(&mut self) -> Vec<u8> {
        let rest = std::mem::take(&mut self.buf);
        if rest.is_empty() {
            return rest;
        }
        let mut out = Vec::new();
        let end = rest.len();
        self.frame(&rest, end, &mut out);
        out
    }

    /// One event (`frame`, its text ending at `end`).
    fn frame(&mut self, frame: &[u8], end: usize, out: &mut Vec<u8>) {
        let Ok(text) = std::str::from_utf8(&frame[..end]) else {
            out.extend_from_slice(frame);
            return;
        };
        let mut lines = Vec::new();
        let mut data: Option<String> = None;
        for line in text.split('\n').map(|l| l.trim_end_matches('\r')) {
            if let Some(d) = line.strip_prefix("data:") {
                let d = d.strip_prefix(' ').unwrap_or(d);
                data = Some(match data {
                    Some(prev) => format!("{prev}\n{d}"),
                    None => d.to_owned(),
                });
            } else {
                lines.push(line);
            }
        }
        let Some(event) = data.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else {
            out.extend_from_slice(frame);
            return;
        };
        match self.event(event) {
            None => {}
            Some((_, false)) => out.extend_from_slice(frame),
            Some((event, true)) => {
                for line in lines {
                    out.extend_from_slice(line.as_bytes());
                    out.push(b'\n');
                }
                out.extend_from_slice(b"data: ");
                out.extend_from_slice(event.to_string().as_bytes());
                out.extend_from_slice(b"\n\n");
            }
        }
    }

    /// A whole (not streamed) answer: its `output` without the dropped
    /// calls. A body that isn't JSON passes as it is.
    pub fn whole(&mut self, body: Vec<u8>) -> Vec<u8> {
        let Ok(mut response) = serde_json::from_slice::<Value>(&body) else {
            return body;
        };
        if self.clean_output(&mut response) {
            serde_json::to_vec(&response).unwrap_or(body)
        } else {
            body
        }
    }
}

/// The name a call to the tool `name` gets: one Codex has no tool for.
fn unavailable(name: &str) -> String {
    if name.starts_with(UNAVAILABLE) {
        return name.to_owned();
    }
    let safe: String = name
        .chars()
        .take(64)
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{UNAVAILABLE}{safe}")
}

/// Where the first event in `buf` ends, and how long its blank line is.
fn frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filter() -> CallFilter {
        CallFilter::new(["click".to_owned(), "find_in_page".to_owned()])
    }

    fn sse(events: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for e in events {
            let kind = e["type"].as_str().unwrap_or("");
            out.extend_from_slice(format!("event: {kind}\ndata: {e}\n\n").as_bytes());
        }
        out
    }

    fn events(bytes: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(bytes)
            .split("\n\n")
            .filter_map(|f| f.lines().find_map(|l| l.strip_prefix("data: ")))
            .map(|d| serde_json::from_str(d).unwrap())
            .collect()
    }

    #[test]
    fn a_streamed_call_to_a_tool_not_offered_never_reaches_codex() {
        let patch = json!({"type": "custom_tool_call", "id": "ctc_1", "call_id": "c1",
            "name": "apply_patch", "input": "*** Begin Patch"});
        let click = json!({"type": "function_call", "id": "fc_2", "call_id": "c2",
            "name": "click", "arguments": "{\"uid\":\"1_2\"}"});
        let shell = json!({"type": "local_shell_call", "id": "lsc_3", "call_id": "c3",
            "action": {"command": ["ls"]}});
        let stream = sse(&[
            json!({"type": "response.output_item.added", "output_index": 0, "item": patch}),
            json!({"type": "response.custom_tool_call_input.delta", "output_index": 0, "item_id": "ctc_1", "delta": "*** Begin"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": patch}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": click}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_2", "delta": "{}"}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": click}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": shell}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": shell}),
            json!({"type": "response.completed", "response": {"id": "r", "output": [patch, click, shell]}}),
        ]);
        let mut f = filter();
        // In awkward pieces, as the network hands them over.
        let mut out = Vec::new();
        for piece in stream.chunks(7) {
            out.extend(f.push(piece));
        }
        out.extend(f.finish());
        let got = events(&out);
        let text = String::from_utf8_lossy(&out);
        assert!(
            !text.contains("\"apply_patch\"") && !text.contains("local_shell_call"),
            "{text}"
        );
        assert_eq!(got.len(), 7, "{got:#?}");
        let renamed = json!({"type": "custom_tool_call", "id": "ctc_1", "call_id": "c1",
            "name": "unavailable_tool_apply_patch", "input": "*** Begin Patch"});
        assert_eq!(got[0]["item"], renamed);
        assert_eq!(got[2]["item"], renamed);
        assert_eq!(got[3]["item"], click, "oxbot's own tool passes");
        assert_eq!(got[6]["response"]["output"], json!([renamed, click]));
    }

    #[test]
    fn a_whole_answer_keeps_only_offered_calls_and_messages() {
        let body = json!({"output": [
            {"type": "reasoning", "id": "rs_1"},
            {"type": "message", "id": "m_1", "content": []},
            {"type": "function_call", "name": "find_in_page", "call_id": "a"},
            {"type": "function_call", "name": "mcp__x__shell", "call_id": "b"},
            {"type": "function_call", "call_id": "nameless"},
            {"type": "web_search_call", "id": "ws"},
        ]});
        let out = filter().whole(serde_json::to_vec(&body).unwrap());
        let out: Value = serde_json::from_slice(&out).unwrap();
        let kept: Vec<String> = out["output"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                format!(
                    "{} {}",
                    i["id"].as_str().or(i["call_id"].as_str()).unwrap(),
                    i["name"].as_str().unwrap_or("-")
                )
            })
            .collect();
        assert_eq!(
            kept,
            [
                "rs_1 -",
                "m_1 -",
                "a find_in_page",
                "b unavailable_tool_mcp__x__shell",
                "nameless unavailable_tool_"
            ]
        );
        let plain = b"not json".to_vec();
        assert_eq!(filter().whole(plain.clone()), plain);
    }

    #[test]
    fn events_without_calls_pass_byte_for_byte() {
        let stream = b"event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}\r\n\r\n: keep-alive\n\n".to_vec();
        let mut f = filter();
        let mut out = f.push(&stream);
        out.extend(f.finish());
        assert_eq!(out, stream);
    }
}
