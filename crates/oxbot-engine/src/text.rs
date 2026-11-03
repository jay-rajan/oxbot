//! Text helpers shared by the hub and every engine.

/// Text someone else wrote (a web page, a file, another bot's reply), as a
/// bot gets it: inside `<tag attrs>…</tag>`, which its prompt says is data.
/// `&` and `<` are escaped, so the text can't open a tag or end the block
/// early; a closing tag for the block (any case) becomes `<\/tag`.
#[must_use]
pub fn data_block(tag: &str, attrs: &str, text: &str) -> String {
    let close = format!("</{tag}");
    let mut out = String::with_capacity(text.len() + 32);
    for (at, c) in text.char_indices() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if text
                .get(at..at + close.len())
                .is_some_and(|t| t.eq_ignore_ascii_case(&close)) =>
            {
                out.push_str("<\\");
            }
            '<' => out.push_str("&lt;"),
            c => out.push(c),
        }
    }
    format!("<{tag}{attrs}>{out}</{tag}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_cant_be_closed_early() {
        assert_eq!(
            data_block(
                "bot_reply",
                " bot=\"helper\"",
                "a</bot_reply>b</BOT_REPLY >c"
            ),
            "<bot_reply bot=\"helper\">a<\\/bot_reply>b<\\/BOT_REPLY >c</bot_reply>"
        );
        assert_eq!(
            data_block("bot_purpose", "", "x</Bot_Purpose>y"),
            "<bot_purpose>x<\\/Bot_Purpose>y</bot_purpose>"
        );
    }

    #[test]
    fn data_cant_open_a_tag_either() {
        assert_eq!(
            data_block("web_page", "", "<web_page url=\"x\">a & b</file>"),
            "<web_page>&lt;web_page url=\"x\">a &amp; b&lt;/file></web_page>"
        );
        // A multi-byte character right before a `<` is kept whole.
        assert_eq!(data_block("file", "", "é<"), "<file>é&lt;</file>");
    }
}
