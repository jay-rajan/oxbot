//! Native adapter, real browser, local fixtures only. Set
//! `OXBOT_CAMOUFOX_BUNDLE` to the generated package to run these tests.
#![allow(clippy::unwrap_used, reason = "test fixtures may panic")]
#![allow(clippy::too_many_lines, reason = "one walk through a browser session")]
use axum::{
    Router,
    response::Html,
    routing::{get, post},
};
use oxbot_browser::{Browser, BrowserSpec, BrowserStatus, CamoufoxTool, Tools, ToolsConfig};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[tokio::test]
async fn native_tools_use_the_broker_and_keep_private_reads_paused() {
    let Some(mut tool) = CamoufoxTool::in_checkout() else {
        eprintln!("SKIPPED: build the browser first: node scripts/camoufox/build.mjs");
        return;
    };
    tool.headless = std::env::var_os("OXBOT_CAMOUFOX_VISIBLE_TEST").is_none();
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    tokio::fs::create_dir_all(&work).await.unwrap();
    let cart = Arc::new(AtomicUsize::new(0));
    let added = Arc::clone(&cart);
    let html = r#"<!doctype html><title>Native local shop</title><label>Product <input type="search"></label><button onclick="fetch('/cart',{method:'POST'}).then(()=>document.querySelector('p').textContent='Added once')">Add milk</button><p>Ready</p>"#;
    let router = Router::new()
        .route("/", get(move || async move { Html(html) }))
        .route(
            "/cart",
            post(move || {
                let added = Arc::clone(&added);
                async move {
                    added.fetch_add(1, Ordering::SeqCst);
                    "ok"
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    // The shop answers as a named site through the test proxy, as the
    // end-to-end tests' site does.
    tool.proxy = Some(format!("http://{}", listener.local_addr().unwrap()));
    let origin = "http://shop.example.test/".to_owned();
    let fixture = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let tools = Tools::new(ToolsConfig {
        camoufox: Some(tool),
        ..ToolsConfig::in_home(tmp.path())
    });
    assert!(tools.is_available());
    let browser = Browser::new(
        &tools,
        BrowserSpec {
            label: "native test".into(),
            profile_dir: tmp.path().join("profile"),
            downloads: work.join("downloads"),
        },
    )
    .unwrap();
    let visits = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&visits);
    browser.count_visits(Arc::new(move |v| seen.lock().unwrap().push(v)));
    browser.set_user(Some("bot-1".into()));
    browser.ensure_running().await.unwrap();
    browser.set_downloads(&work.join("downloads")).await;
    assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::new();
    let endpoint = browser.endpoint();
    let send = |name: &str, args: Value| {
        client
            .post(endpoint)
            .json(&json!({"name":name,"args":args,"workspace":work,"operation":uuid()}))
            .send()
    };
    let text = send("navigate_page", json!({"url":origin}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        text["text"].as_str().is_some_and(|t| t.contains("Native local shop")),
        "{text}"
    );
    let snapshot = send("take_snapshot", json!({}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let text = snapshot["text"].as_str().unwrap();
    let uid = |label: &str| {
        text.lines()
            .find(|line| line.contains(label))
            .unwrap()
            
            .split_whitespace()
            .next()
            .unwrap()
            .trim_start_matches("uid=")
            .to_owned()
    };
    let input = uid("searchbox \"Product\"");
    let button = uid("button \"Add milk\"");
    let start = Instant::now();
    let filled = send("fill", json!({"uid":input,"value":"milk"}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(filled.get("error").is_none(), "{filled}");
    let clicked = send("click", json!({"uid":button}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(clicked.get("error").is_none(), "{clicked}");
    assert!(
        start.elapsed() >= Duration::from_secs(8),
        "actions are deliberately spaced"
    );
    assert_eq!(cart.load(Ordering::SeqCst), 1);
    assert!(
        browser
            .inspect(Some(&origin), "document.querySelector('input').value")
            .await
            .is_some()
    );
    assert_eq!(
        browser.count_named(&origin, "button", "Add milk").await,
        Some(1)
    );
    assert!(
        browser
            .screenshot(Some(&origin))
            .await
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    let denied = client
        .post(endpoint)
        .header("Origin", "https://evil.example")
        .json(&json!({"name":"take_snapshot","workspace":work,"operation":uuid()}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let admin = send("inspect", json!({"expression":"document.cookie"}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(admin.get("error").is_some());
    browser.open_window(&origin).await.unwrap();
    assert_eq!(*browser.status().borrow(), BrowserStatus::SigningIn);
    assert!(
        browser
            .inspect(None, "document.body.innerText")
            .await
            .is_none()
    );
    assert!(browser.cookie_marks().await.is_err());
    let paused = send("take_snapshot", json!({}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(paused.get("error").is_some());
    browser.close_window().await;
    assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
    let stale = send("click", json!({"uid":button}))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(stale.get("error").is_some());
    // One site signs out; the other stays signed in.
    let cookie = |domain: &str| {
        json!({"name": "sid", "value": "v", "domain": domain, "path": "/",
               "secure": true, "expires": 2_000_000_000})
    };
    assert_eq!(
        browser
            .add_cookies(vec![cookie(".bank.example"), cookie(".other.example")])
            .await
            .unwrap(),
        2
    );
    assert_eq!(browser.sign_out_site("www.bank.example").await.unwrap(), 1);
    assert_eq!(browser.cookie_sites().await.unwrap(), ["other.example"]);
    browser.shutdown().await;
    assert_eq!(*browser.status().borrow(), BrowserStatus::Off);
    // Closing ended the visit to the local shop, counted for the bot.
    let counted = visits.lock().unwrap().clone();
    let shop = counted
        .iter()
        .find(|v| v.site == "example.test")
        .unwrap_or_else(|| panic!("{counted:?}"));
    assert_eq!(shop.user.as_deref(), Some("bot-1"));
    assert!(shop.pages >= 1 && shop.bytes_down > 0, "{shop:?}");
    // A helper killed without cleaning up leaves its lock: the next start
    // clears it, since its owner is gone.
    let lock = tmp.path().join("profile/.oxbot-runtime-lock");
    std::fs::create_dir_all(&lock).unwrap();
    std::fs::write(lock.join("owner.json"), r#"{"pid":999999}"#).unwrap();
    browser.ensure_running().await.unwrap();
    assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
    browser.shutdown().await;
    fixture.abort();
}
fn uuid() -> String {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("test-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}
