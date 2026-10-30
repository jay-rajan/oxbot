//! A tiny HTTP/1.1 client for 127.0.0.1 only (Chrome's `DevTools` list,
//! Safari's driver, oxbot itself), and base64, so the runner needs no
//! web-client crates.

use anyhow::{Context, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Sends one request to `127.0.0.1:port` and returns the status and body.
///
/// # Errors
/// Fails if the connection or the reply fails.
pub async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> anyhow::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connecting to port {port}"))?;
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    // Some servers (Chrome's) keep the connection open: read the head, then
    // exactly the body's length.
    tokio::time::timeout(std::time::Duration::from_secs(30), read_reply(&mut stream))
        .await
        .with_context(|| format!("port {port} didn't answer {path}"))?
}

async fn read_reply(stream: &mut TcpStream) -> anyhow::Result<(u16, String)> {
    let mut raw = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    let head_end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            bail!("the connection closed before a reply");
        }
        raw.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
    let mut body = raw[head_end + 4..].to_vec();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    let chunked = head.contains("transfer-encoding: chunked");
    loop {
        let done = match length {
            Some(len) => body.len() >= len,
            None if chunked => body.windows(5).any(|w| w == b"0\r\n\r\n"),
            None => false,
        };
        if done {
            break;
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    Ok((status, if chunked { unchunk(&text) } else { text }))
}

fn unchunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        body = rest[n..].trim_start_matches("\r\n");
    }
    out
}

/// Decodes standard base64 (padding optional, whitespace ignored).
///
/// # Errors
/// Fails on a character that isn't base64.
pub fn base64_decode(text: &str) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => bail!("not base64: {:?}", c as char),
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buf >> bits) & 0xFF).unwrap_or(0));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
        assert!(base64_decode("a$").is_err());
    }

    #[test]
    fn chunked_bodies_are_joined() {
        assert_eq!(unchunk("3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), "abcde");
    }
}
