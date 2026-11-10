//! oxbot's own read of a Google file's whole text (`read_document`): one
//! GET, run by oxbot (never the bot) in its isolated world of a tab that
//! shows the file's own site, so the person's sign-in applies and the
//! request goes nowhere but that site (and where it redirects a download).
//!
//! The JavaScript is fixed and built here from plain strings (pure,
//! unit tested); it checks it runs on the expected site before it fetches,
//! so a tab that moved elsewhere reads nothing. Which addresses may be read
//! is decided by the caller (the hub, with `oxbot_loop`'s rules).

use std::time::Duration;

use serde_json::Value;

/// The most of a file's text oxbot reads (characters).
pub const MAX_TEXT_CHARS: usize = 4_000_000;
/// How long one read may take.
pub const FETCH_LIMIT: Duration = Duration::from_secs(30);

/// A string as a JavaScript string literal.
fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

/// The expression that reads `url` from a tab on `origin` (e.g.
/// `https://docs.google.com`): a GET with the site's own cookies (none to
/// anywhere it redirects), at most `max` characters of text, within `ms`.
#[must_use]
pub fn fetch_expression(origin: &str, url: &str, max: usize, ms: u128) -> String {
    format!(
        "(async () => {{ \
         const origin = {origin}, url = {url}; \
         if (String(location.origin) !== origin) {{ return {{ error: 'the tab is not on ' + origin, at: String(location.href) }}; }} \
         const stop = new AbortController(); const timer = setTimeout(() => stop.abort(), {ms}); \
         try {{ \
           const r = await fetch(url, {{ method: 'GET', credentials: 'same-origin', redirect: 'follow', cache: 'no-store', signal: stop.signal }}); \
           const text = await r.text(); \
           return {{ status: r.status, type: r.headers.get('content-type') || '', url: String(r.url), at: String(location.href), \
             text: text.length > {max} ? text.slice(0, {max}) : text, cut: text.length > {max} }}; \
         }} catch (e) {{ return {{ error: String((e && e.message) || e), at: String(location.href) }}; }} \
         finally {{ clearTimeout(timer); }} \
         }})()",
        origin = js_string(origin),
        url = js_string(url),
    )
}

/// What a read got back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetched {
    /// The HTTP status.
    pub status: u16,
    /// Its content type.
    pub content_type: String,
    /// The address it ended at (after redirects).
    pub url: String,
    /// The tab's address, as the page itself says.
    pub at: String,
    /// The text (at most [`MAX_TEXT_CHARS`]).
    pub text: String,
    /// It was longer: only the start is here.
    pub cut: bool,
}

/// Reads what [`fetch_expression`] returned.
///
/// # Errors
/// Why nothing was read, in plain words.
pub fn read_fetched(v: &Value) -> Result<Fetched, String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    if let Some(err) = v.get("error").and_then(Value::as_str) {
        return Err(err.to_owned());
    }
    let status = v
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or("the read gave nothing back")?;
    Ok(Fetched {
        status,
        content_type: s("type").to_owned(),
        url: s("url").to_owned(),
        at: s("at").to_owned(),
        text: s("text").to_owned(),
        cut: v.get("cut").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_read_is_one_get_on_the_expected_site_only() {
        let e = fetch_expression(
            "https://docs.google.com",
            "https://docs.google.com/document/d/abc/export?format=txt",
            100,
            30_000,
        );
        assert!(
            e.contains("const origin = \"https://docs.google.com\""),
            "{e}"
        );
        assert!(e.contains("String(location.origin) !== origin"), "{e}");
        assert!(e.contains("method: 'GET'"));
        assert!(e.contains("credentials: 'same-origin'"));
        assert!(e.contains("setTimeout(() => stop.abort(), 30000)"));
        assert!(e.contains("text.length > 100 ? text.slice(0, 100)"));
        // An address can't break out of its string.
        let e = fetch_expression("https://a", "x\"; alert(1); \"", 1, 1);
        assert!(e.contains(r#"url = "x\"; alert(1); \"""#), "{e}");
    }

    #[test]
    fn its_answer_is_read_or_says_why_not() {
        let ok = read_fetched(&json!({
            "status": 200, "type": "text/plain", "url": "https://x/1", "at": "https://x/e",
            "text": "hi", "cut": true
        }))
        .unwrap();
        assert_eq!(ok.status, 200);
        assert_eq!(ok.text, "hi");
        assert!(ok.cut);
        assert_eq!(
            read_fetched(&json!({"error": "the tab is not on https://x"})).unwrap_err(),
            "the tab is not on https://x"
        );
        assert!(read_fetched(&json!({})).is_err());
    }
}
