//! Runs the shared browser against a real Chrome (skipped when Chrome isn't
//! installed): it starts hidden, takes a screenshot for an approval card,
//! reads a page for oxbot's checks, resets its tabs for the next bot, keeps
//! its profile when it stops, and deletes it on sign-out.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::time::Duration;

use oxbot_browser::{Browser, BrowserSpec, BrowserStatus, Tools, ToolsConfig};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

async fn http(port: u16, request: &str) -> String {
    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    tcp.write_all(
        format!("{request} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    // Chrome may keep the connection open: keep what arrives until it pauses.
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(800), tcp.read(&mut chunk)).await
    {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Sends one `DevTools` message to the browser and waits for its answer.
async fn devtools(port: u16, message: &str) {
    use futures_util::{SinkExt as _, StreamExt as _};
    let version = http(port, "GET /json/version").await;
    let body = &version[version.find('{').unwrap()..];
    let url: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    // Chrome echoes the Host header, which this test sends without a port.
    let path = url["webSocketDebuggerUrl"].as_str().unwrap();
    let path = &path[path.find("/devtools").unwrap()..];
    let ws = format!("ws://127.0.0.1:{port}{path}");
    let (mut socket, _) = tokio_tungstenite::connect_async(ws.as_str()).await.unwrap();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            message.into(),
        ))
        .await
        .unwrap();
    let _ = socket.next().await;
}

fn tabs(list: &str) -> usize {
    list.matches("\"type\": \"page\"").count() + list.matches("\"type\":\"page\"").count()
}

#[tokio::test]
async fn the_shared_browser_keeps_its_profile_until_sign_out() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = ToolsConfig::in_home(tmp.path());
    if !config.chrome.exists() {
        eprintln!("Chrome isn't installed; skipping");
        return;
    }
    config.tools_dir = tmp.path().join("no-tools");
    let profile_dir = config.profile_dir.clone();
    let browser = Browser::new(
        Tools::new(config),
        BrowserSpec {
            label: "test".into(),
            profile_dir: profile_dir.clone(),
            downloads: tmp.path().join("downloads"),
        },
    )
    .unwrap();
    browser.ensure_running().await.unwrap();
    assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
    assert!(profile_dir.exists());
    assert!(tmp.path().join("downloads").is_dir());

    // Open a page with a password field, like the browser tool would.
    let page = "data:text/html,%3Cinput%20type%3Dpassword%20aria-label%3DSecret%20autofocus%3E";
    http(browser.port(), &format!("PUT /json/new?{page}")).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let shot = browser.screenshot(None).await.expect("a screenshot");
    assert!(shot.starts_with("data:image/jpeg;base64,"));
    let found = browser
        .inspect(
            None,
            "[...document.querySelectorAll('input[type=password]')].map(e => e.getAttribute('aria-label'))",
        )
        .await
        .expect("the page can be read");
    assert_eq!(found, serde_json::json!(["Secret"]));

    // Sites see normal Chrome, not "HeadlessChrome".
    let agent = browser
        .inspect(None, "navigator.userAgent")
        .await
        .expect("the user agent");
    let agent = agent.as_str().unwrap_or_default();
    assert!(agent.contains(" Chrome/"), "{agent}");
    assert!(!agent.contains("Headless"), "{agent}");

    // The next bot starts on one blank tab.
    browser.reset_tabs().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(tabs(&http(browser.port(), "GET /json/list").await), 1);

    // Signing out of one site keeps the others.
    devtools(
        browser.port(),
        r#"{"id":1,"method":"Storage.setCookies","params":{"cookies":[
            {"name":"sid","value":"1","domain":".substack.com","path":"/","secure":true,"expires":2000000000},
            {"name":"a","value":"1","domain":"jay.substack.com","path":"/","secure":true},
            {"name":"g","value":"1","domain":".google.com","path":"/","secure":true,"expires":2000000000}]}}"#,
    )
    .await;
    assert_eq!(
        browser.cookie_sites().await.unwrap(),
        ["google.com", "substack.com"]
    );
    assert_eq!(browser.forget_site("substack.com").await.unwrap(), 2);
    assert_eq!(browser.cookie_sites().await.unwrap(), ["google.com"]);

    let other = tmp.path().join("bot2/downloads");
    browser.set_downloads(&other).await;
    assert!(other.is_dir());

    browser.shutdown().await;
    assert_eq!(*browser.status().borrow(), BrowserStatus::Off);
    assert!(profile_dir.exists(), "sign-ins are kept");

    browser.sign_out().await.unwrap();
    assert!(!profile_dir.exists(), "signing out deletes the profile");
}
