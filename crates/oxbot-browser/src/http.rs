//! The few plain HTTP calls oxbot makes to Chrome's control port
//! (`/json/version`, `/json/list`, `/json/new`, `/json/close`).
//!
//! Chrome's control port ignores HTTP/1.0 and keeps connections open, so
//! these speak HTTP/1.1 and read until `Content-Length` is satisfied.

use std::fmt::Write as _;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// `GET path` on `127.0.0.1:port`; the body of a 200 answer, or `None`.
pub(crate) async fn get(port: u16, path: &str) -> Option<String> {
    request(port, "GET", path).await
}
/// `PUT path` on `127.0.0.1:port` (Chrome wants PUT for `/json/new`);
/// whether it answered 200.
pub(crate) async fn put(port: u16, path: &str) -> bool {
    request(port, "PUT", path).await.is_some()
}

async fn request(port: u16, method: &str, path: &str) -> Option<String> {
    let call = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.ok()?;
        let mut response = Vec::new();
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            response.extend_from_slice(&chunk[..n]);
            if is_complete(&response) {
                break;
            }
        }
        ok_body(&String::from_utf8_lossy(&response))
    };
    tokio::time::timeout(Duration::from_secs(3), call)
        .await
        .ok()
        .flatten()
}

/// Whether an HTTP response has all of its body, going by `Content-Length`.
pub(crate) fn is_complete(response: &[u8]) -> bool {
    let text = String::from_utf8_lossy(response);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    head.lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .is_some_and(|len| body.len() >= len)
}

/// The body of a 200 answer; anything else is `None`, never an empty body,
/// so a bad answer can't look like "no tabs".
pub(crate) fn ok_body(response: &str) -> Option<String> {
    let (head, body) = response.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200").then(|| body.to_owned())
}

/// The ids of the open tabs in a `/json/list` body.
pub(crate) fn page_ids(body: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(body)
        .unwrap_or_default()
        .iter()
        .filter(|t| t.get("type").and_then(serde_json::Value::as_str) == Some("page"))
        .filter_map(|t| t.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// Percent-encodes the characters that would break a request line, keeping
/// the rest of the URL (including existing `%xx`) as it is.
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
        let body = r#"[{"id":"A","type":"page"},{"id":"B","type":"service_worker"},{"id":"C","type":"page"}]"#;
        assert_eq!(page_ids(body), ["A", "C"]);
        assert_eq!(page_ids("not json"), Vec::<String>::new());
    }

    #[test]
    fn only_a_good_answer_has_a_body() {
        assert_eq!(
            ok_body("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}").as_deref(),
            Some("{}")
        );
        assert_eq!(ok_body(""), None, "an empty answer isn't zero tabs");
        assert_eq!(ok_body("HTTP/1.1 500 Oops\r\n\r\n[]"), None);
        assert!(is_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n[  ]"
        ));
        assert!(!is_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\n[  ]"
        ));
    }
}
