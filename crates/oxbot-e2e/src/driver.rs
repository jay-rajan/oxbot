//! The browsers tests run in, behind one set of actions: Chrome through its
//! `DevTools` link, and Safari (`WebKit`, the engine of the Mac app's
//! window) through its built-in `WebDriver`, `safaridriver`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::http;

/// The page size every test uses, so screenshots compare.
pub const WIDTH: u32 = 1280;
/// See [`WIDTH`].
pub const HEIGHT: u32 = 800;

const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/// Which browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Chrome, hidden.
    Chrome,
    /// Safari, through `safaridriver`.
    Safari,
}

impl Engine {
    /// Its name in folders and the report.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Safari => "safari",
        }
    }
}

/// A browser being driven.
pub enum Driver {
    /// Chrome.
    Chrome(Box<Cdp>),
    /// Safari.
    Safari(WebDriver),
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Chrome(_) => "Driver::Chrome",
            Self::Safari(_) => "Driver::Safari",
        })
    }
}

impl Driver {
    /// Starts `engine`.
    ///
    /// # Errors
    /// Fails if the browser can't start (for Safari, usually because
    /// "Allow Remote Automation" is off).
    pub async fn start(engine: Engine, scratch: &Path) -> anyhow::Result<Self> {
        Ok(match engine {
            Engine::Chrome => Self::Chrome(Box::new(Cdp::start(scratch).await?)),
            Engine::Safari => Self::Safari(WebDriver::start().await?),
        })
    }

    /// Goes to `url` and waits for the page to load.
    ///
    /// # Errors
    /// Fails if the browser does.
    pub async fn open(&mut self, url: &str) -> anyhow::Result<()> {
        match self {
            Self::Chrome(c) => {
                c.call("Page.navigate", json!({ "url": url })).await?;
            }
            Self::Safari(s) => {
                s.call("POST", "/url", json!({ "url": url })).await?;
            }
        }
        for _ in 0..100 {
            if self.eval("document.readyState === 'complete'").await? == json!(true) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("{url} didn't finish loading")
    }

    /// Runs `expression` on the page and returns its value.
    ///
    /// # Errors
    /// Fails if it throws.
    pub async fn eval(&mut self, expression: &str) -> anyhow::Result<Value> {
        match self {
            Self::Chrome(c) => {
                let r = c
                    .call(
                        "Runtime.evaluate",
                        json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
                    )
                    .await?;
                if let Some(e) = r.get("exceptionDetails") {
                    let text = e["exception"]["description"]
                        .as_str()
                        .or_else(|| e["text"].as_str())
                        .unwrap_or("error");
                    bail!("page script failed: {text}");
                }
                Ok(r["result"]["value"].clone())
            }
            Self::Safari(s) => {
                let script = format!("return ({expression});");
                s.call(
                    "POST",
                    "/execute/sync",
                    json!({ "script": script, "args": [] }),
                )
                .await
            }
        }
    }

    /// Clicks at a point on the page, like a person would.
    ///
    /// # Errors
    /// Fails if the browser does.
    pub async fn click_at(&mut self, x: f64, y: f64) -> anyhow::Result<()> {
        match self {
            Self::Chrome(c) => {
                for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
                    c.call(
                        "Input.dispatchMouseEvent",
                        json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }),
                    )
                    .await?;
                }
            }
            Self::Safari(s) => {
                let (x, y) = (x.round(), y.round());
                s.call(
                    "POST",
                    "/actions",
                    json!({ "actions": [{ "type": "pointer", "id": "mouse", "parameters": { "pointerType": "mouse" }, "actions": [
                        { "type": "pointerMove", "x": x, "y": y, "origin": "viewport" },
                        { "type": "pointerDown", "button": 0 },
                        { "type": "pointerUp", "button": 0 }
                    ]}]}),
                )
                .await?;
                let _ = s.call("DELETE", "/actions", json!({})).await;
            }
        }
        Ok(())
    }

    /// Types text where the focus is.
    ///
    /// # Errors
    /// Fails if the browser does.
    pub async fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        match self {
            Self::Chrome(c) => {
                c.call("Input.insertText", json!({ "text": text })).await?;
            }
            Self::Safari(s) => {
                let mut actions = Vec::new();
                for ch in text.chars() {
                    actions.push(json!({ "type": "keyDown", "value": ch.to_string() }));
                    actions.push(json!({ "type": "keyUp", "value": ch.to_string() }));
                }
                s.call(
                    "POST",
                    "/actions",
                    json!({ "actions": [{ "type": "key", "id": "keys", "actions": actions }] }),
                )
                .await?;
                let _ = s.call("DELETE", "/actions", json!({})).await;
            }
        }
        Ok(())
    }

    /// Presses a key: `Enter`, `Escape`, `Tab` or `Backspace`.
    ///
    /// # Errors
    /// Fails on another key, or if the browser does.
    pub async fn key(&mut self, key: &str) -> anyhow::Result<()> {
        let (code, webdriver, text) = match key {
            "Enter" => (13, "\u{E007}", "\r"),
            "Escape" => (27, "\u{E00C}", ""),
            "Tab" => (9, "\u{E004}", ""),
            "Backspace" => (8, "\u{E003}", ""),
            _ => bail!("unknown key {key}; use Enter, Escape, Tab or Backspace"),
        };
        match self {
            Self::Chrome(c) => {
                let mut down = json!({ "type": "keyDown", "key": key, "code": key, "windowsVirtualKeyCode": code, "nativeVirtualKeyCode": code });
                if !text.is_empty() {
                    down["text"] = json!(text);
                }
                c.call("Input.dispatchKeyEvent", down).await?;
                c.call(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyUp", "key": key, "code": key, "windowsVirtualKeyCode": code, "nativeVirtualKeyCode": code }),
                )
                .await?;
            }
            Self::Safari(s) => {
                s.call("POST", "/actions", json!({ "actions": [{ "type": "key", "id": "keys", "actions": [
                    { "type": "keyDown", "value": webdriver }, { "type": "keyUp", "value": webdriver }
                ]}]}))
                .await?;
                let _ = s.call("DELETE", "/actions", json!({})).await;
            }
        }
        Ok(())
    }

    /// A PNG of what's on screen.
    ///
    /// # Errors
    /// Fails if the browser does.
    pub async fn screenshot(&mut self) -> anyhow::Result<Vec<u8>> {
        let data = match self {
            Self::Chrome(c) => c
                .call("Page.captureScreenshot", json!({ "format": "png" }))
                .await?["data"]
                .clone(),
            Self::Safari(s) => s.call("GET", "/screenshot", json!({})).await?,
        };
        http::base64_decode(data.as_str().ok_or_else(|| anyhow!("no screenshot"))?)
    }

    /// Closes the browser.
    pub async fn stop(self) {
        match self {
            Self::Chrome(c) => c.stop().await,
            Self::Safari(s) => s.stop().await,
        }
    }
}

// ---------------------------------------------------------------- Chrome

/// Chrome, hidden, with a throwaway profile, driven over `DevTools`.
pub struct Cdp {
    child: Child,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next: u64,
    profile: PathBuf,
}

impl Cdp {
    async fn start(scratch: &Path) -> anyhow::Result<Self> {
        let profile = scratch.join("e2e-chrome");
        std::fs::create_dir_all(&profile)?;
        let child = Command::new(CHROME)
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--headless=new",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-sync",
                "--hide-scrollbars",
                "--force-color-profile=srgb",
                "--remote-debugging-address=127.0.0.1",
                "--remote-debugging-port=0",
                &format!("--window-size={WIDTH},{HEIGHT}"),
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting Chrome")?;
        // Chrome writes its port to the profile once it's listening.
        let mut port = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(profile.join("DevToolsActivePort"))
                && let Some(p) = text
                    .lines()
                    .next()
                    .and_then(|l| l.trim().parse::<u16>().ok())
            {
                port = Some(p);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let port = port.ok_or_else(|| anyhow!("Chrome didn't open its DevTools port"))?;
        let (_, list) = http::request(port, "GET", "/json/list", None).await?;
        let targets: Value = serde_json::from_str(&list)?;
        let ws_url = targets
            .as_array()
            .and_then(|t| t.iter().find(|t| t["type"] == "page"))
            .and_then(|t| t["webSocketDebuggerUrl"].as_str())
            .ok_or_else(|| anyhow!("Chrome has no page"))?
            .to_owned();
        let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let mut cdp = Self {
            child,
            ws,
            next: 0,
            profile,
        };
        cdp.call(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": WIDTH, "height": HEIGHT, "deviceScaleFactor": 1, "mobile": false }),
        )
        .await?;
        // Same look every run: light theme, no animations.
        cdp.call(
            "Emulation.setEmulatedMedia",
            json!({ "features": [{ "name": "prefers-color-scheme", "value": "light" }, { "name": "prefers-reduced-motion", "value": "reduce" }] }),
        )
        .await?;
        Ok(cdp)
    }

    /// Sends a command and waits for its answer (events are skipped).
    async fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.next += 1;
        let id = self.next;
        self.ws
            .send(Message::Text(
                json!({ "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .await?;
        let answer = tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(msg) = self.ws.next().await {
                let Message::Text(text) = msg? else { continue };
                let v: Value = serde_json::from_str(&text)?;
                if v["id"] == id {
                    return Ok::<Value, anyhow::Error>(v);
                }
            }
            bail!("Chrome closed the connection")
        })
        .await
        .with_context(|| format!("Chrome didn't answer {method}"))??;
        if let Some(err) = answer.get("error") {
            bail!("{method}: {}", err["message"].as_str().unwrap_or("failed"));
        }
        Ok(answer["result"].clone())
    }

    async fn stop(mut self) {
        let _ = self.call("Browser.close", json!({})).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = self.child.kill().await;
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

// ---------------------------------------------------------------- Safari

/// Safari, driven by `safaridriver`.
pub struct WebDriver {
    child: Child,
    port: u16,
    session: String,
}

impl WebDriver {
    async fn start() -> anyhow::Result<Self> {
        let port = crate::serve::free_port()?;
        let child = Command::new("safaridriver")
            .args(["--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting safaridriver")?;
        let mut answer = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Ok(a) = http::request(
                port,
                "POST",
                "/session",
                Some(
                    &json!({ "capabilities": { "alwaysMatch": { "browserName": "safari" } } })
                        .to_string(),
                ),
            )
            .await
            {
                answer = Some(a);
                break;
            }
        }
        let (status, body) = answer.ok_or_else(|| anyhow!("safaridriver didn't start"))?;
        let v: Value = serde_json::from_str(&body).unwrap_or_default();
        let Some(session) = v["value"]["sessionId"].as_str() else {
            bail!(
                "Safari won't be driven ({status}): {}. Turn on Safari → Settings → Advanced → \"Show features for web developers\", then Develop → \"Allow Remote Automation\".",
                v["value"]["message"].as_str().unwrap_or("no session")
            );
        };
        let mut driver = Self {
            child,
            port,
            session: session.to_owned(),
        };
        driver
            .call(
                "POST",
                "/window/rect",
                json!({ "width": WIDTH, "height": HEIGHT + 90 }),
            )
            .await?;
        Ok(driver)
    }

    async fn call(&mut self, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
        let url = format!("/session/{}{path}", self.session);
        let body = (method != "GET").then(|| body.to_string());
        let (status, text) = http::request(self.port, method, &url, body.as_deref()).await?;
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        if status >= 400 {
            bail!(
                "Safari: {}",
                v["value"]["message"].as_str().unwrap_or(&text)
            );
        }
        Ok(v["value"].clone())
    }

    async fn stop(mut self) {
        let path = format!("/session/{}", self.session);
        let _ = http::request(self.port, "DELETE", &path, None).await;
        let _ = self.child.kill().await;
    }
}
