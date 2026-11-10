//! One-shot requests to a running `oxbot serve`, for CLI commands such as
//! `oxbot bots new` that should take effect without a restart.
//!
//! The home computer secret goes to a server only after it proved it holds
//! the same one ([`check_oxbot`]): another program listening on oxbot's
//! port never gets it.

use std::time::Duration;

use anyhow::{Context as _, bail};
use futures_util::{SinkExt, StreamExt};
use oxbot_protocol::{ClientMsg, ServerMsg};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::Message;

/// How long the hello check may take.
const HELLO_WAIT: Duration = Duration::from_secs(5);
/// The most of a hello answer that is read.
const HELLO_MAX: u64 = 4096;

/// Whether something is listening on `port` (oxbot or not: see
/// [`check_oxbot`]).
pub async fn server_running(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

/// What to tell the person when something else holds oxbot's port.
#[must_use]
pub fn port_taken(port: u16) -> String {
    format!("Something else is using port {port}; quit it or set a different port.")
}

/// Checks that the server on `port` is the oxbot holding `key`: it must
/// answer a fresh nonce with [`crate::secret::hello_proof`]. The key itself
/// is never sent.
///
/// # Errors
/// [`port_taken`] if the answer is missing or wrong.
pub async fn check_oxbot(port: u16, key: &str) -> anyhow::Result<()> {
    let nonce = crate::secret::random_hex::<16>();
    let answer = tokio::time::timeout(HELLO_WAIT, ask_hello(port, &nonce))
        .await
        .ok()
        .and_then(Result::ok);
    match answer {
        Some(answer) if crate::secret::hello_proof_ok(key, &nonce, &answer) => Ok(()),
        _ => {
            tracing::warn!(port, "the server on oxbot's port isn't this oxbot");
            bail!(port_taken(port))
        }
    }
}

/// `GET /hello?n=<nonce>` on `127.0.0.1:port`: the answer's body if it
/// says 200.
async fn ask_hello(port: u16, nonce: &str) -> anyhow::Result<String> {
    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let path = crate::secret::HELLO_PATH;
    tcp.write_all(
        format!(
            "GET {path}?n={nonce} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await?;
    let mut reply = Vec::new();
    tcp.take(HELLO_MAX).read_to_end(&mut reply).await?;
    let reply = String::from_utf8_lossy(&reply);
    let (head, body) = reply.split_once("\r\n\r\n").context("no answer")?;
    if !head.starts_with("HTTP/1.1 200") {
        bail!("refused");
    }
    Ok(body.to_owned())
}

/// The WebSocket address of the oxbot on this Mac at `port`, with the home
/// computer secret from the data folder (for `oxbot chat` and `oxbot
/// bots`), once that server proved it's this oxbot. Without a secret yet,
/// the address has none.
///
/// # Errors
/// [`port_taken`] if another program is on the port.
pub async fn local_ws_url(port: u16) -> anyhow::Result<String> {
    match crate::secret::local_home_computer_key() {
        Some(key) => {
            check_oxbot(port, &key).await?;
            Ok(format!("ws://127.0.0.1:{port}/ws?k={key}"))
        }
        None => Ok(format!("ws://127.0.0.1:{port}/ws")),
    }
}

/// The page's address on the oxbot already running at `port`, with the
/// home computer secret (for the app's window), once that server proved
/// it's this oxbot.
///
/// # Errors
/// [`port_taken`] if another program is on the port, or there's no secret
/// to check it with.
pub async fn local_page_url(port: u16) -> anyhow::Result<String> {
    let key = crate::secret::local_home_computer_key().with_context(|| port_taken(port))?;
    check_oxbot(port, &key).await?;
    Ok(format!("http://127.0.0.1:{port}/#k={key}"))
}

/// Sends `msg` and returns the server's first direct answer: a created bot,
/// a notice or an error (not the bot lists and events sent to everyone).
///
/// # Errors
/// Fails if the server can't be reached or doesn't answer within 10 seconds.
pub async fn request(port: u16, msg: &ClientMsg) -> anyhow::Result<ServerMsg> {
    request_until(port, msg, Duration::from_secs(10), |m| {
        matches!(
            m,
            ServerMsg::BotCreated(_) | ServerMsg::Notice { .. } | ServerMsg::Error { .. }
        )
    })
    .await
}

/// Sends `msg` and waits up to `wait` for the first reply that `is_answer`
/// accepts.
///
/// # Errors
/// Fails if the server can't be reached, closes, or doesn't answer in time.
pub async fn request_until(
    port: u16,
    msg: &ClientMsg,
    wait: Duration,
    is_answer: impl Fn(&ServerMsg) -> bool,
) -> anyhow::Result<ServerMsg> {
    let url = local_ws_url(port).await?;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("couldn't reach oxbot on port {port}"))?;
    ws.send(Message::Text(serde_json::to_string(msg)?.into()))
        .await?;
    let reply = tokio::time::timeout(wait, async {
        while let Some(frame) = ws.next().await {
            if let Message::Text(text) = frame? {
                let msg = serde_json::from_str::<ServerMsg>(&text)?;
                if is_answer(&msg) {
                    return Ok(msg);
                }
            }
        }
        bail!("oxbot closed the connection")
    })
    .await
    .context("oxbot didn't answer in time")??;
    let _ = ws.close(None).await;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in on a free port that answers every request with `reply`
    /// (what a different program on oxbot's port might say).
    async fn impostor(reply: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = tcp.read(&mut buf).await;
                let _ = tcp.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn another_program_on_the_port_never_gets_the_key() {
        let key = crate::secret::random_hex::<32>();
        for reply in [
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
            "",
        ] {
            let port = impostor(reply).await;
            let err = check_oxbot(port, &key).await.unwrap_err();
            assert_eq!(err.to_string(), port_taken(port), "{reply:?}");
        }
    }

    #[tokio::test]
    async fn the_real_server_proves_itself() {
        use oxbot_core::{Hub, HubConfig, Store};
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            Store::open_in_memory().unwrap(),
            HubConfig::new(tmp.path()),
        );
        let state = crate::server::AppState::new(hub, "test-mac");
        let key = state.home_computer_key.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(crate::server::serve(
            listener,
            state,
            std::future::pending(),
        ));
        check_oxbot(port, &key).await.unwrap();
        // Another oxbot (another data folder) isn't this one.
        assert!(
            check_oxbot(port, &crate::secret::random_hex::<32>())
                .await
                .is_err()
        );
    }
}
