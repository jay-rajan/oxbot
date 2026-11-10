//! A practice run's Chrome against a real Chrome (skipped when Chrome
//! isn't installed): its network guard lets pages read, but stops writes
//! and `WebSocket`s, even from a tab's very first script.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxbot_browser::{Browser, BrowserSpec, NetMode, Tools, ToolsConfig};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The page a tab opens: on its very first script it tries a fetch PUT, a
/// fetch POST, a WebSocket and a beacon, then reads an image and posts a
/// form.
const PAGE: &str = r#"<!doctype html><html><body>
<form id=f method=post action="/form"><input name=t value=hi></form>
<script>
fetch("/put", {method: "PUT", body: "x"}).catch(() => {});
fetch("/xhr-post", {method: "POST", body: "x"}).catch(() => {});
try { new WebSocket("ws://" + location.host + "/ws"); } catch (e) {}
const frame = document.createElement("iframe");
document.body.appendChild(frame);
try { new frame.contentWindow.WebSocket("ws://" + location.host + "/ws-frame"); } catch (e) {}
new Worker(URL.createObjectURL(new Blob(["try { new WebSocket('ws://" + location.host + "/ws-worker'); } catch (e) {}"])));
navigator.sendBeacon("/beacon", "x");
const img = new Image(); img.src = "/img.png";
setTimeout(() => document.getElementById("f").submit(), 400);
</script></body></html>"#;

/// A tiny web server that remembers every request as `METHOD /path`.
async fn server() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let Ok(n) = tcp.read(&mut buf).await else {
                    return;
                };
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let line = head.lines().next().unwrap_or("").to_owned();
                let mut parts = line.split_whitespace();
                let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                log.lock().unwrap().push(format!("{method} {path}"));
                let body = if path == "/page" { PAGE } else { "ok" };
                let kind = if path == "/page" {
                    "text/html"
                } else {
                    "text/plain"
                };
                let _ = tcp
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    (port, seen)
}

/// Opens a new tab at `url` through the browser's endpoint, as the
/// browser tool does.
async fn new_tab(endpoint: &str, url: &str) {
    use futures_util::{SinkExt as _, StreamExt as _};
    let (mut socket, _) = tokio_tungstenite::connect_async(endpoint).await.unwrap();
    let open =
        serde_json::json!({"id": 1, "method": "Target.createTarget", "params": {"url": url}});
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            open.to_string().into(),
        ))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(3), socket.next()).await;
}

async fn trial_browser(dir: &std::path::Path, mode: NetMode) -> Option<Arc<Browser>> {
    let mut config = ToolsConfig::in_home(dir);
    if !config.chrome.exists() {
        eprintln!("Chrome isn't installed; skipping");
        return None;
    }
    config.tools_dir = dir.join("no-tools");
    let browser = Browser::new_trial(
        Tools::new(config),
        BrowserSpec {
            label: "trial".into(),
            profile_dir: dir.join("trial/profile"),
            downloads: dir.join("trial/downloads"),
        },
        mode,
    )
    .unwrap();
    browser.ensure_running().await.unwrap();
    assert!(browser.is_trial());
    assert!(
        browser.net_ready(Duration::from_secs(5)).await,
        "the guard is on in the first tab"
    );
    Some(browser)
}

async fn visit(browser: &Browser, site: u16) {
    new_tab(browser.endpoint(), &format!("http://127.0.0.1:{site}/page")).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
}

#[tokio::test]
async fn strict_mode_lets_pages_read_and_stops_every_write() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(browser) = trial_browser(tmp.path(), NetMode::Strict).await else {
        return;
    };
    let (site, seen) = server().await;
    visit(&browser, site).await;
    let seen = seen.lock().unwrap().clone();
    let (blocked, _) = browser.blocked_requests().await;
    browser.shutdown().await;

    assert!(seen.contains(&"GET /page".to_owned()), "{seen:?}");
    assert!(seen.contains(&"GET /img.png".to_owned()), "{seen:?}");
    for write in [
        "PUT /put",
        "POST /xhr-post",
        "POST /beacon",
        "POST /form",
        "GET /ws",
    ] {
        assert!(
            !seen.contains(&write.to_owned()),
            "{write} went out: {seen:?}"
        );
    }
    let stopped: Vec<String> = blocked
        .iter()
        .map(|b| format!("{} {}", b.method, b.path))
        .collect();
    for write in ["PUT /put", "POST /xhr-post", "POST /beacon"] {
        assert!(stopped.contains(&write.to_owned()), "{write}: {stopped:?}");
    }
    assert!(
        !browser.net_ready(Duration::from_millis(100)).await,
        "no browser tools once its guard is gone"
    );
}

#[tokio::test]
async fn the_guard_remembers_what_it_stopped_and_xhr_mode_lets_script_posts_out() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(browser) = trial_browser(tmp.path(), NetMode::XhrAllowed).await else {
        return;
    };
    let (site, seen) = server().await;
    visit(&browser, site).await;
    let (blocked, count) = browser.blocked_requests().await;
    let seen = seen.lock().unwrap().clone();
    browser.shutdown().await;

    assert!(seen.contains(&"POST /xhr-post".to_owned()), "{seen:?}");
    for write in [
        "PUT /put",
        "POST /beacon",
        "POST /form",
        "GET /ws",
        "GET /ws-frame",
        "GET /ws-worker",
    ] {
        assert!(
            !seen.contains(&write.to_owned()),
            "{write} went out: {seen:?}"
        );
    }
    let stopped: Vec<String> = blocked
        .iter()
        .map(|b| format!("{} {}", b.method, b.path))
        .collect();
    for write in ["PUT /put", "POST /beacon", "POST /form"] {
        assert!(stopped.contains(&write.to_owned()), "{write}: {stopped:?}");
    }
    assert_eq!(count, blocked.len());
    assert!(blocked.iter().all(|b| b.host == "127.0.0.1"));
}
