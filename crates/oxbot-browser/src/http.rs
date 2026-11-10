//! Small helpers for Chrome's tab list and for test pages' addresses.

#[cfg(test)]
use std::fmt::Write as _;

/// The ids of the open tabs in `Target.getTargets`' `targetInfos`.
pub(crate) fn page_ids(infos: &serde_json::Value) -> Vec<String> {
    infos
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|t| t.get("type").and_then(serde_json::Value::as_str) == Some("page"))
        .filter_map(|t| t.get("targetId").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// Percent-encodes the characters that would break a request line, keeping
/// the rest of the URL (including existing `%xx`) as it is. (Tests only
/// since the sign-in window stopped taking tabs over `DevTools`.)
#[cfg(test)]
pub(crate) fn encode_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~:/?&=+%@!$'()*,;[]".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_made_safe_for_a_request_line() {
        assert_eq!(
            encode_url("https://mail.google.com/mail/u/0/#search/a b"),
            "https://mail.google.com/mail/u/0/%23search/a%20b"
        );
        assert_eq!(
            encode_url("https://x.com/?q=%22hi%22"),
            "https://x.com/?q=%22hi%22"
        );
    }

    #[test]
    fn only_tabs_are_listed() {
        let infos = serde_json::json!([
            {"targetId":"A","type":"page"},
            {"targetId":"B","type":"service_worker"},
            {"targetId":"C","type":"page"}
        ]);
        assert_eq!(page_ids(&infos), ["A", "C"]);
        assert_eq!(page_ids(&serde_json::Value::Null), Vec::<String>::new());
    }
}
