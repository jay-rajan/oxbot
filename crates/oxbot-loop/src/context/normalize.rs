//! Text cleaned once, when a message is first saved, so resending it
//! never changes a byte: line ends become `\n`, trailing spaces go, and
//! control characters other than `\n` and `\t` are removed.

use oxbot_llm::{Message, Part};

/// `text` cleaned.
#[must_use]
pub fn normalize(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(unified.len());
    for (i, line) in unified.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let kept: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        out.push_str(kept.trim_end());
    }
    out
}

/// `message` with every text part cleaned. Tool calls are left exactly as
/// the model wrote them.
#[must_use]
pub fn normalize_message(mut message: Message) -> Message {
    for part in &mut message.parts {
        if let Part::Text(t) = part {
            *t = normalize(t);
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ends_spaces_and_controls() {
        assert_eq!(normalize("a \r\nb\t \rc\u{0}d\u{7}\n\te"), "a\nb\ncd\n\te");
        assert_eq!(normalize("x  \n\n  y  "), "x\n\n  y");
        assert_eq!(normalize("日本\u{1b}[0m"), "日本[0m");
        let once = normalize("a\r\n b \u{0}");
        assert_eq!(normalize(&once), once);
    }
}
