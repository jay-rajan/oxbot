//! Runs shown Chrome (a real window in the Dock) against a real Chrome:
//! it starts minimized and under control; signing in brings the window
//! back and cuts every connection; done puts it back in the Dock. Ignored
//! by default because it puts a window on the screen: run with
//! `cargo test -p oxbot-browser --test shown_chrome -- --ignored`.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::time::Duration;

use oxbot_browser::{Browser, BrowserSpec, BrowserStatus, Tools, ToolsConfig};
use serde_json::{Value, json};

/// Sends `method` through the endpoint as the browser tool would; `None`
/// if the connection is refused or ends.
async fn devtools(endpoint: &str, method: &str, params: Value) -> Option<Value> {
    use futures_util::{SinkExt as _, StreamExt as _};
    let (mut socket, _) = tokio_tungstenite::connect_async(endpoint).await.ok()?;
    let msg = json!({ "id": 1, "method": method, "params": params });
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            msg.to_string().into(),
        ))
        .await
        .ok()?;
    let read = async {
        while let Some(Ok(m)) = socket.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(text) = m {
                let answer: Value = serde_json::from_str(&text).ok()?;
                if answer["id"] == 1 {
                    return answer.get("result").cloned();
                }
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .ok()?
}

/// The window states of every page's window.
async fn window_states(endpoint: &str) -> Vec<String> {
    let list = devtools(endpoint, "Target.getTargets", json!({}))
        .await
        .unwrap();
    let mut states = Vec::new();
    for t in list["targetInfos"].as_array().unwrap() {
        if t["type"] != "page" {
            continue;
        }
        let found = devtools(
            endpoint,
            "Browser.getWindowForTarget",
            json!({ "targetId": t["targetId"] }),
        )
        .await
        .unwrap();
        states.push(found["bounds"]["windowState"].as_str().unwrap().to_owned());
    }
    states
}

/// Opens the window from the Dock (as the person would) and checks it
/// fills the screen, then puts it back.
async fn assert_full_size_when_opened(endpoint: &str) {
    let list = devtools(endpoint, "Target.getTargets", json!({}))
        .await
        .unwrap();
    let tab = list["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == "page")
        .unwrap()["targetId"]
        .clone();
    let window = devtools(
        endpoint,
        "Browser.getWindowForTarget",
        json!({ "targetId": tab }),
    )
    .await
    .unwrap()["windowId"]
        .clone();
    devtools(
        endpoint,
        "Browser.setWindowBounds",
        json!({ "windowId": window, "bounds": { "windowState": "normal" } }),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let size = devtools(
        endpoint,
        "Browser.getWindowBounds",
        json!({ "windowId": window }),
    )
    .await
    .unwrap();
    eprintln!("restored: {size}");
    let width = size["bounds"]["width"].as_i64().unwrap();
    assert!((1000..=8000).contains(&width), "the screen's width: {size}");
    devtools(
        endpoint,
        "Browser.setWindowBounds",
        json!({ "windowId": window, "bounds": { "windowState": "minimized" } }),
    )
    .await;
}

#[tokio::test]
#[ignore = "puts a Chrome window on the screen"]
async fn shown_chrome_lives_in_the_dock_and_comes_up_to_sign_in() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = ToolsConfig::in_home(tmp.path());
    if !config.chrome.exists() {
        eprintln!("Chrome isn't installed; skipping");
        return;
    }
    config.tools_dir = tmp.path().join("no-tools");
    config.show_browser = true;
    let browser = Browser::new(
        Tools::new(config),
        BrowserSpec {
            label: "test".into(),
            profile_dir: tmp.path().join("profile"),
            downloads: tmp.path().join("downloads"),
        },
    )
    .unwrap();
    let endpoint = browser.endpoint().to_owned();

    browser.ensure_running().await.unwrap();
    assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
    assert_eq!(window_states(&endpoint).await, ["minimized"]);
    let agent = devtools(&endpoint, "Browser.getVersion", json!({}))
        .await
        .unwrap();
    assert!(!agent["userAgent"].as_str().unwrap().contains("Headless"));

    // Signing in: the window comes up and nobody can connect.
    browser
        .open_window("data:text/html,<h1>sign in</h1>")
        .await
        .unwrap();
    assert_eq!(*browser.status().borrow(), BrowserStatus::SigningIn);
    assert!(
        devtools(&endpoint, "Target.getTargets", json!({}))
            .await
            .is_none(),
        "connections are cut while the person signs in"
    );

    // Done: back in the Dock, the sign-in tab gone, connections again;
    // opened from the Dock it's the full size of the screen.
    browser.close_window().await;
    assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
    assert_eq!(window_states(&endpoint).await, ["minimized"]);
    assert_full_size_when_opened(&endpoint).await;

    let local_state: Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("profile/Local State")).unwrap())
            .unwrap();
    assert_eq!(
        local_state["profile"]["info_cache"]["Default"]["name"],
        "oxbot"
    );

    // The person closes the window (its last tab): Chrome stops, and
    // starts again in the Dock when a bot next needs it. (The watcher
    // looks every two seconds; give it time to see the window first.)
    tokio::time::sleep(Duration::from_secs(6)).await;
    let list = devtools(&endpoint, "Target.getTargets", json!({}))
        .await
        .unwrap();
    for t in list["targetInfos"].as_array().unwrap() {
        if t["type"] == "page" {
            devtools(
                &endpoint,
                "Target.closeTarget",
                json!({ "targetId": t["targetId"] }),
            )
            .await;
        }
    }
    let mut status = browser.status();
    tokio::time::timeout(
        Duration::from_secs(20),
        status.wait_for(|s| *s == BrowserStatus::Off),
    )
    .await
    .expect("closing the window stops Chrome")
    .unwrap();
    browser.ensure_running().await.unwrap();
    assert_eq!(window_states(&endpoint).await, ["minimized"]);

    browser.shutdown().await;
    assert_eq!(*browser.status().borrow(), BrowserStatus::Off);
}
