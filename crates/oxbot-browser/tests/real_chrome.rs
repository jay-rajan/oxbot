//! Runs the shared browser against a real Chrome (skipped when Chrome isn't
//! installed): it starts hidden, takes a screenshot for an approval card,
//! reads a page for oxbot's checks, resets its tabs for the next bot, keeps
//! its profile when it stops, and deletes it on sign-out.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::sync::Arc;
use std::time::Duration;

use oxbot_browser::{Browser, BrowserSpec, BrowserStatus, Tools, ToolsConfig};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Sends one `DevTools` message to the browser through its endpoint (as
/// the browser tool does) and returns Chrome's answer to it.
async fn devtools(endpoint: &str, message: serde_json::Value) -> serde_json::Value {
    use futures_util::{SinkExt as _, StreamExt as _};
    let (mut socket, _) = tokio_tungstenite::connect_async(endpoint).await.unwrap();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            message.to_string().into(),
        ))
        .await
        .unwrap();
    while let Some(Ok(msg)) = socket.next().await {
        if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
            let answer: serde_json::Value = serde_json::from_str(&text).unwrap();
            if answer.get("id") == message.get("id") {
                return answer;
            }
        }
    }
    panic!("no answer to {message}");
}

async fn tabs(endpoint: &str) -> usize {
    let list = devtools(endpoint, json!({"id": 1, "method": "Target.getTargets"})).await;
    list["result"]["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["type"] == "page")
        .count()
}

/// The local TCP ports processes `pids` listen on (`lsof`).
fn listening(pids: &[String]) -> String {
    let out = std::process::Command::new("lsof")
        .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN", "-p", &pids.join(",")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every Chrome process (main and helpers) on `profile`.
fn chrome_pids(profile: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .args([
            "-f",
            "--",
            &format!("--user-data-dir={}", profile.display()),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
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
    devtools(
        browser.endpoint(),
        json!({"id": 1, "method": "Target.createTarget", "params": {"url": page}}),
    )
    .await;
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
    assert_eq!(tabs(browser.endpoint()).await, 1);

    // Signing out of one site keeps the others.
    let cookies: serde_json::Value = serde_json::from_str(
        r#"{"id":1,"method":"Storage.setCookies","params":{"cookies":[
            {"name":"sid","value":"1","domain":".substack.com","path":"/","secure":true,"expires":2000000000},
            {"name":"a","value":"1","domain":"jay.substack.com","path":"/","secure":true},
            {"name":"g","value":"1","domain":".google.com","path":"/","secure":true,"expires":2000000000}]}}"#,
    )
    .unwrap();
    devtools(browser.endpoint(), cookies).await;
    assert_eq!(
        browser.cookie_sites().await.unwrap(),
        ["google.com", "substack.com"]
    );
    assert_eq!(browser.sign_out_site("substack.com").await.unwrap(), 2);
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

/// Hidden Chrome opens no control port: the only way in is oxbot's own
/// proxy, at the secret endpoint, which `Target.getTargets` works through.
#[tokio::test]
async fn hidden_chrome_listens_on_no_port() {
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

    let pids = chrome_pids(&profile_dir);
    assert!(!pids.is_empty(), "Chrome runs");
    let chrome_ports = listening(&pids);
    assert!(
        chrome_ports.trim().is_empty(),
        "Chrome listens: {chrome_ports}"
    );

    // The proxy's port is oxbot's (this test's process), not Chrome's.
    let endpoint = browser.endpoint().to_owned();
    let port = endpoint
        .trim_start_matches("ws://127.0.0.1:")
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let ours = listening(&[std::process::id().to_string()]);
    assert!(ours.contains(&format!("127.0.0.1:{port}")), "{ours}");

    // Chrome's usual way in is shut.
    let mut tcp = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tcp.write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut answer = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), tcp.read_to_string(&mut answer)).await;
    assert!(!answer.contains("webSocketDebuggerUrl"), "{answer}");
    let guessed = format!("ws://127.0.0.1:{port}/devtools/browser/{}", "0".repeat(64));
    assert!(
        tokio_tungstenite::connect_async(guessed.as_str())
            .await
            .is_err()
    );

    // The secret endpoint works.
    assert!(tabs(&endpoint).await >= 1);
    browser.shutdown().await;
}

/// The real browser tool (`chrome-devtools-mcp`, from the app's bundle in
/// `target/tools-bundle`; skipped if it isn't built) drives hidden Chrome
/// through the endpoint: it opens a page and reads it.
#[tokio::test]
async fn the_browser_tool_drives_chrome_through_the_endpoint() {
    use tokio::io::AsyncBufReadExt as _;
    let bundle = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tools-bundle");
    let tool = oxbot_browser::BuiltInTool::find_in(&bundle);
    let tmp = tempfile::tempdir().unwrap();
    let mut config = ToolsConfig::in_home(tmp.path());
    let Some(tool) = tool.filter(|_| config.chrome.exists()) else {
        eprintln!("Chrome or the built browser tool is missing; skipping");
        return;
    };
    config.tools_dir = tmp.path().join("no-tools");
    config.built_in = Some(tool);
    let tools = Tools::new(config);
    let browser = Browser::new(
        Arc::clone(&tools),
        BrowserSpec {
            label: "test".into(),
            profile_dir: tmp.path().join("profile"),
            downloads: tmp.path().join("downloads"),
        },
    )
    .unwrap();
    browser.ensure_running().await.unwrap();
    let command = tools.mcp_command(browser.endpoint(), tmp.path());
    let mut child = tokio::process::Command::new(&command.program)
        .args(&command.args)
        .envs(command.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut ask = async |id: u64, method: &str, params: serde_json::Value| {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    };
    ask(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    )
    .await;
    let page = "data:text/html,<title>Hello pipe</title><button>Press me</button>";
    let opened = ask(
        2,
        "tools/call",
        json!({"name": "new_page", "arguments": {"url": page}}),
    )
    .await;
    assert!(opened.get("error").is_none(), "{opened}");
    assert!(opened.to_string().contains("Hello pipe"), "{opened}");
    let snap = ask(
        3,
        "tools/call",
        json!({"name": "take_snapshot", "arguments": {"pageId": 2}}),
    )
    .await;
    let text = snap.to_string();
    assert!(text.contains("Press me"), "{text}");
    drop(child);
    browser.shutdown().await;
}
