//! Small helpers for web addresses.

use url::Url;

/// The host of an `http(s)` address, lowercase, or `None` for anything else
/// (`file:`, `chrome:`, `javascript:`, `data:`, garbage).
#[must_use]
pub fn web_host(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.host_str().map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_have_a_host() {
        assert_eq!(
            web_host("https://Mail.Google.com/x").as_deref(),
            Some("mail.google.com")
        );
        assert_eq!(
            web_host("http://localhost:8790/").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            web_host("https://a.com@evil.io/").as_deref(),
            Some("evil.io")
        );
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "chrome://settings",
            "data:text/html,hi",
            "nope",
        ] {
            assert_eq!(web_host(bad), None, "{bad}");
        }
    }
}
