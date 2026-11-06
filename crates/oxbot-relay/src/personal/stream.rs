//! Turning stand-ins back in a streamed answer (server-sent events) as it
//! passes to Codex: each event is read whole, its text restored and
//! written out again, so Codex always gets valid events and JSON.
//!
//! A stand-in can be split across two text deltas (`… [em` then
//! `ail 1] …`): only a possible start of one at the end of a delta is
//! held back, and goes out with the next delta of the same text, or on
//! its own before any other event.

use std::sync::Arc;

use serde_json::Value;

use super::StandIns;

/// The longest stand-in's inside (`postal code 999999`), plus room.
const MAX_INSIDE: usize = 20;

/// The start of a stand-in a delta may end with.
struct Held {
    /// Which text it belongs to.
    key: String,
    /// The held text.
    tail: String,
    /// The delta event it came in (written again for it, if need be).
    event: Value,
    /// That event's other lines (`event: …`).
    lines: Vec<String>,
}

/// Restores one streamed answer.
pub struct SseRestorer {
    stand_ins: Arc<StandIns>,
    buf: Vec<u8>,
    held: Option<Held>,
}

impl std::fmt::Debug for SseRestorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseRestorer")
            .field("stand_ins", &self.stand_ins.len())
            .finish_non_exhaustive()
    }
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

/// `text` split where a stand-in may be starting at its end.
fn split_partial(text: &str) -> (&str, &str) {
    if let Some(i) = text.rfind('[') {
        let inside = &text[i + 1..];
        if inside.len() <= MAX_INSIDE
            && inside
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == ' ')
        {
            return text.split_at(i);
        }
    }
    (text, "")
}

/// Which text a delta event adds to.
fn delta_key(v: &Value) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        v["type"], v["item_id"], v["output_index"], v["content_index"], v["summary_index"]
    )
}

fn write(out: &mut Vec<u8>, lines: &[String], data: &Value) {
    for line in lines {
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(b"data: ");
    out.extend_from_slice(data.to_string().as_bytes());
    out.extend_from_slice(b"\n\n");
}

impl SseRestorer {
    /// For an answer to a request redacted with `stand_ins`.
    #[must_use]
    pub fn new(stand_ins: Arc<StandIns>) -> Self {
        Self {
            stand_ins,
            buf: Vec::new(),
            held: None,
        }
    }

    /// Takes the next bytes from the provider; gives what Codex gets now.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some((end, blank)) = frame_end(&self.buf) {
            let frame: Vec<u8> = self.buf.drain(..end + blank).collect();
            self.frame(&frame[..end], &mut out);
        }
        out
    }

    /// The answer ended: whatever is left.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.flush(&mut out);
        let rest = std::mem::take(&mut self.buf);
        match std::str::from_utf8(&rest) {
            Ok(text) => out.extend_from_slice(self.stand_ins.restore(text).as_bytes()),
            Err(_) => out.extend_from_slice(&rest),
        }
        out
    }

    /// Sends what's held, as a delta of its own.
    fn flush(&mut self, out: &mut Vec<u8>) {
        if let Some(held) = self.held.take() {
            let mut event = held.event;
            event["delta"] = Value::String(self.restore_delta(&event, &held.tail));
            write(out, &held.lines, &event);
        }
    }

    fn restore_delta(&self, event: &Value, text: &str) -> String {
        let in_json = event["type"]
            .as_str()
            .is_some_and(|t| t.contains("arguments"));
        if in_json {
            self.stand_ins.restore_in_json_string(text)
        } else {
            self.stand_ins.restore(text)
        }
    }

    /// One event (without its blank line).
    fn frame(&mut self, frame: &[u8], out: &mut Vec<u8>) {
        let pass = |out: &mut Vec<u8>| {
            out.extend_from_slice(frame);
            out.extend_from_slice(b"\n\n");
        };
        let Ok(text) = std::str::from_utf8(frame) else {
            self.flush(out);
            return pass(out);
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
                lines.push(line.to_owned());
            }
        }
        let Some(mut event) = data.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else {
            self.flush(out);
            return pass(out);
        };
        let is_delta = event["type"]
            .as_str()
            .is_some_and(|t| t.rsplit('.').next() == Some("delta"))
            && event["delta"].is_string();
        if !is_delta {
            self.flush(out);
            self.stand_ins.restore_value(&mut event);
            return write(out, &lines, &event);
        }
        let key = delta_key(&event);
        let mut text = match self.held.take() {
            Some(held) if held.key == key => held.tail,
            other => {
                self.held = other;
                self.flush(out);
                String::new()
            }
        };
        text.push_str(event["delta"].as_str().unwrap_or_default());
        let (now, later) = split_partial(&text);
        let now = self.restore_delta(&event, now);
        if !later.is_empty() {
            self.held = Some(Held {
                key,
                tail: later.to_owned(),
                event: event.clone(),
                lines: lines.clone(),
            });
        }
        if !now.is_empty() {
            event["delta"] = Value::String(now);
            write(out, &lines, &event);
        }
    }
}
