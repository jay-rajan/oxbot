//! A proxy that lets nothing through. Every Codex process is told to use
//! it for every address but 127.0.0.1 (`HTTPS_PROXY`, `ALL_PROXY`,
//! `NO_PROXY=127.0.0.1,localhost`), so if Codex ever tried to reach
//! anything but oxbot's relay (an update check, a plugin sync, telemetry),
//! the connection fails at once, and oxbot logs where it wanted to go.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

/// The refusing proxy, running. Stops when dropped.
#[derive(Debug)]
pub struct RefuseProxy {
    url: String,
    tries: Arc<AtomicU64>,
    stop: CancellationToken,
}

impl Drop for RefuseProxy {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl RefuseProxy {
    /// Starts it on 127.0.0.1 on a free port.
    ///
    /// # Errors
    /// No port could be had.
    pub async fn start() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let url = format!("http://{}", listener.local_addr()?);
        let tries = Arc::new(AtomicU64::new(0));
        let stop = CancellationToken::new();
        let (counted, stopped) = (Arc::clone(&tries), stop.clone());
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stopped.cancelled() => return,
                    a = listener.accept() => a,
                };
                let Ok((mut conn, _)) = accepted else {
                    continue;
                };
                counted.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    let n = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        conn.read(&mut buf),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(0);
                    let first = String::from_utf8_lossy(&buf[..n]);
                    let target = first
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("?")
                        .chars()
                        .take(200)
                        .collect::<String>();
                    tracing::warn!(
                        target,
                        "Codex tried to reach something other than oxbot's relay; refused"
                    );
                    let _ = conn
                        .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                });
            }
        });
        Ok(Self { url, tries, stop })
    }

    /// Its address, for `HTTPS_PROXY`.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Connections it refused.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.tries.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_connection_is_refused_and_counted() {
        let proxy = RefuseProxy::start().await.unwrap();
        let addr = proxy.url().trim_start_matches("http://").to_owned();
        let mut conn = tokio::net::TcpStream::connect(&addr).await.unwrap();
        conn.write_all(b"CONNECT api.github.com:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut answer = String::new();
        conn.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert_eq!(proxy.refused(), 1);
    }
}
