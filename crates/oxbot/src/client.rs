//! One-shot requests to a running `oxbot serve`, for CLI commands such as
//! `oxbot bots new` that should take effect without a restart.

use std::time::Duration;

use anyhow::{Context as _, bail};
use futures_util::{SinkExt, StreamExt};
use oxbot_protocol::{ClientMsg, ServerMsg};
use tokio_tungstenite::tungstenite::Message;

/// Whether `oxbot serve` is listening on `port`.
pub async fn server_running(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
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
    let url = format!("ws://127.0.0.1:{port}/ws");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("couldn't reach oxbot at {url}"))?;
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
