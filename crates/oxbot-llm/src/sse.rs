//! A small parser for server-sent events, as chat APIs stream them.
//!
//! Feed it bytes as they arrive; it hands back complete events. Lines may be
//! split anywhere, even inside a UTF-8 character. Comment lines (OpenRouter
//! sends `: OPENROUTER PROCESSING` while it waits) are reported so a caller
//! can treat them as signs of life, `data: [DONE]` ends the stream, and
//! `event:`, `id:` and `retry:` fields are ignored.

/// One parsed event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SseEvent {
    /// The joined `data:` lines of one event.
    Data(String),
    /// A comment line (keep-alive).
    Comment,
    /// `data: [DONE]`: nothing more will come.
    Done,
}

/// The stream broke the format.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    /// One line or event is longer than the limit.
    #[error("a streamed event was longer than {0} bytes")]
    TooLong(usize),
    /// A line isn't UTF-8.
    #[error("a streamed line wasn't UTF-8")]
    NotUtf8,
}

/// Longest line or event accepted: far above any real chunk, low enough
/// that a broken server can't fill memory.
pub const MAX_EVENT: usize = 8 * 1024 * 1024;

/// Incremental SSE parser.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Option<String>,
    skip_lf: bool,
    max: usize,
}

impl SseParser {
    /// A parser with the [`MAX_EVENT`] limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limit(MAX_EVENT)
    }

    /// A parser with its own size limit.
    #[must_use]
    pub fn with_limit(max: usize) -> Self {
        Self {
            buf: Vec::new(),
            data: None,
            skip_lf: false,
            max,
        }
    }

    /// Adds bytes and returns the events they complete.
    ///
    /// # Errors
    /// A line or event over the limit, or a line that isn't UTF-8.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        for &b in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.skip_lf = b == b'\r';
                    let line = std::mem::take(&mut self.buf);
                    if let Some(event) = self.line(&line)? {
                        events.push(event);
                    }
                }
                _ => {
                    if self.buf.len() >= self.max {
                        return Err(SseError::TooLong(self.max));
                    }
                    self.buf.push(b);
                }
            }
        }
        Ok(events)
    }

    /// The stream ended: returns an event left without its closing blank
    /// line, if any (some servers end that way).
    ///
    /// # Errors
    /// The last line isn't UTF-8.
    pub fn finish(&mut self) -> Result<Option<SseEvent>, SseError> {
        let line = std::mem::take(&mut self.buf);
        let mut last = None;
        if !line.is_empty() {
            last = self.line(&line)?;
        }
        if last.is_none() {
            last = self.dispatch();
        }
        Ok(last)
    }

    fn line(&mut self, line: &[u8]) -> Result<Option<SseEvent>, SseError> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        let line = std::str::from_utf8(line).map_err(|_| SseError::NotUtf8)?;
        if line.starts_with(':') {
            return Ok(Some(SseEvent::Comment));
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        if field == "data" {
            let len = self.data.as_ref().map_or(0, |d| d.len() + 1);
            if len + value.len() > self.max {
                return Err(SseError::TooLong(self.max));
            }
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            }
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let data = self.data.take()?;
        if data == "[DONE]" {
            return Some(SseEvent::Done);
        }
        Some(SseEvent::Data(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(input: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for chunk in input {
            out.extend(p.push(chunk).unwrap());
        }
        out.extend(p.finish().unwrap());
        out
    }

    #[test]
    fn data_events_comments_and_done() {
        let events = all(&[b": OPENROUTER PROCESSING\n\ndata: {\"a\":1}\n\ndata: [DONE]\n\n"]);
        assert_eq!(
            events,
            vec![
                SseEvent::Comment,
                SseEvent::Data("{\"a\":1}".into()),
                SseEvent::Done
            ]
        );
    }

    #[test]
    fn lines_split_anywhere_even_inside_a_character() {
        let text = "data: caf\u{e9}\n\ndata: two\n\n".as_bytes();
        let chunks: Vec<&[u8]> = text.chunks(1).collect();
        assert_eq!(
            all(&chunks),
            vec![
                SseEvent::Data("caf\u{e9}".into()),
                SseEvent::Data("two".into())
            ]
        );
    }

    #[test]
    fn crlf_and_cr_line_endings() {
        assert_eq!(
            all(&[b"data: a\r\n\r\ndata: b\r\rdata: c\r", b"\n\r\n"]),
            vec![
                SseEvent::Data("a".into()),
                SseEvent::Data("b".into()),
                SseEvent::Data("c".into()),
            ]
        );
    }

    #[test]
    fn multi_line_data_joins_with_newlines_and_other_fields_are_ignored() {
        let events = all(&[b"event: x\nid: 7\ndata:one\ndata: two\nretry: 5\n\n"]);
        assert_eq!(events, vec![SseEvent::Data("one\ntwo".into())]);
    }

    #[test]
    fn last_event_without_blank_line_is_kept() {
        assert_eq!(all(&[b"data: tail"]), vec![SseEvent::Data("tail".into())]);
        assert_eq!(all(&[b"data: tail\n"]), vec![SseEvent::Data("tail".into())]);
        assert_eq!(all(&[b""]), Vec::<SseEvent>::new());
    }

    #[test]
    fn empty_data_line_is_an_empty_event() {
        assert_eq!(all(&[b"data:\n\n"]), vec![SseEvent::Data(String::new())]);
        // A blank line with no data dispatches nothing.
        assert_eq!(all(&[b"\n\n\n"]), Vec::<SseEvent>::new());
    }

    #[test]
    fn oversized_lines_and_events_are_refused() {
        let mut p = SseParser::with_limit(8);
        assert_eq!(p.push(b"data: 0123456789"), Err(SseError::TooLong(8)));

        let mut p = SseParser::with_limit(8);
        assert_eq!(
            p.push(b"data: 12345\ndata: 12345\n"),
            Err(SseError::TooLong(8))
        );
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        let mut p = SseParser::new();
        assert_eq!(p.push(b"data: \xff\n"), Err(SseError::NotUtf8));
    }
}
