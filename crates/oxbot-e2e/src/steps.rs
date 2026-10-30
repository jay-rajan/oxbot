//! Runs a test's steps in a browser: each step is done the way a person
//! would (find what's on screen by its words, click it, type), checked, and
//! followed by a screenshot.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use futures_util::SinkExt;

use crate::driver::Driver;
use crate::serve::Server;
use crate::spec::Step;

/// What happened at one step.
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    /// Its number, from 1.
    pub n: usize,
    /// What it did, in words.
    pub text: String,
    /// Why it's there, from the test.
    pub note: Option<String>,
    /// `pass`, `fail` or `not_run`.
    pub status: &'static str,
    /// What went wrong.
    pub error: Option<String>,
    /// How long it took.
    pub ms: u64,
    /// The screenshot after it, relative to the run folder.
    pub shot: Option<String>,
    /// How it compares with the approved baseline.
    pub compare: Option<crate::compare::Comparison>,
}

/// Helpers the steps use on the page: find elements by the words a person
/// sees, the way they'd find them.
const HELPERS: &str = r#"
window.__e2e = window.__e2e || {
  norm(s) { return (s || "").replace(/\s+/g, " ").trim(); },
  shown(el) {
    if (!el || el.closest("[hidden]")) return false;
    const d = el.closest("dialog"); if (d && !d.open) return false;
    const st = getComputedStyle(el); if (st.visibility === "hidden" || st.display === "none") return false;
    const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0;
  },
  // What a person reads on it (strongest first): its text, its label or
  // placeholder, then its hover title.
  words(el) {
    const shown = [this.norm(el.innerText)];
    if (el.tagName === "INPUT" && (el.type === "submit" || el.type === "button")) shown.push(this.norm(el.value));
    return [
      [shown.filter(Boolean), 5],
      [[el.getAttribute("aria-label"), el.getAttribute("placeholder")].map((s) => this.norm(s)).filter(Boolean), 4],
      [[this.norm(el.getAttribute("title"))].filter(Boolean), 3],
    ];
  },
  scope() { const open = [...document.querySelectorAll("dialog[open]")].pop(); return open || document.body; },
  find(text, fieldsOnly) {
    const want = this.norm(text).toLowerCase();
    const sel = fieldsOnly ? "input,textarea,select" : "*";
    let best = null, bestScore = -1;
    for (const el of this.scope().querySelectorAll(sel)) {
      if (!this.shown(el)) continue;
      let score = -1;
      for (const [list, weight] of this.words(el)) {
        for (const w of list.map((x) => x.toLowerCase())) {
          if (w === want) score = Math.max(score, weight);
          else if (w.includes(want)) score = Math.max(score, 1);
        }
      }
      if (!fieldsOnly && el.id && el.id.toLowerCase() === want) score = Math.max(score, 4);
      if (score < 0) continue;
      if (el.matches("button,a,input,textarea,select,label,[role=button],[role=tab],[role=radio],.bot-item,.site")) score += 2;
      const r = el.getBoundingClientRect();
      const tie = best && score === bestScore && r.width * r.height < best.getBoundingClientRect().width * best.getBoundingClientRect().height;
      if (score > bestScore || tie) { best = el; bestScore = score; }
    }
    if (!best && fieldsOnly) {
      for (const label of this.scope().querySelectorAll("label")) {
        if (this.shown(label) && this.norm(label.innerText).toLowerCase().includes(want)) {
          const c = label.control || label.querySelector("input,textarea,select"); if (c) return c;
        }
      }
    }
    return best;
  },
  point(text) {
    const el = this.find(text, false);
    if (!el) return { error: "nothing on screen says “" + text + "”" };
    el.scrollIntoView({ block: "center", inline: "center" });
    const r = el.getBoundingClientRect();
    const x = r.left + r.width / 2, y = r.top + r.height / 2;
    const top = document.elementFromPoint(x, y);
    if (top && top !== el && !el.contains(top) && !top.contains(el)) {
      return { error: "“" + text + "” is covered by " + (this.norm(top.innerText).slice(0, 40) || top.tagName) };
    }
    return { x, y };
  },
  focusField(text, clear) {
    const el = this.find(text, true) || (() => { const e = this.find(text, false); return e && e.matches("input,textarea") ? e : null; })();
    if (!el) return "no field called “" + text + "”";
    if (el.disabled) return "“" + text + "” is disabled";
    el.focus();
    if (clear) { el.value = ""; el.dispatchEvent(new Event("input", { bubbles: true })); }
    return "";
  },
  text() { return this.norm(document.body.innerText); },
  has(text) { return this.text().includes(this.norm(text)); },
  disabled(text) {
    const el = this.find(text, false) || this.find(text, true);
    if (!el) return null;
    return !!(el.disabled || el.getAttribute("aria-disabled") === "true");
  },
};
true
"#;

async fn js(driver: &mut Driver, expression: &str) -> anyhow::Result<Value> {
    driver.eval(HELPERS).await?;
    driver.eval(expression).await
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// Runs one step. Returns `Ok` if it passed.
async fn run_step(driver: &mut Driver, server: &Server, step: &Step) -> anyhow::Result<()> {
    // `{server}` in a step means this oxbot's own address (a local page to
    // sign in to, never a real site).
    let base = server.url("");
    let fill = |text: &str| text.replace("{server}", &base);
    if let Some(path) = &step.open {
        driver.open(&server.url(path)).await?;
        // The page connects to oxbot and draws the bots.
        wait_until(
            driver,
            "document.querySelectorAll('.bot-item').length > 0",
            10_000,
        )
        .await
        .context("the page didn't show any bots")?;
    }
    if let Some(text) = &step.click {
        let p = js(driver, &format!("__e2e.point({})", quoted(text))).await?;
        if let Some(e) = p["error"].as_str() {
            bail!("{e}");
        }
        let (x, y) = (
            p["x"].as_f64().unwrap_or(0.0),
            p["y"].as_f64().unwrap_or(0.0),
        );
        driver.click_at(x, y).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Some(t) = &step.type_ {
        let err = js(
            driver,
            &format!("__e2e.focusField({}, {})", quoted(&t.into), t.clear),
        )
        .await?;
        if let Some(e) = err.as_str().filter(|e| !e.is_empty()) {
            bail!("{e}");
        }
        driver.type_text(&fill(&t.text)).await?;
    }
    if let Some(text) = &step.send {
        let err = js(driver, "__e2e.focusField('Message', true)").await?;
        if let Some(e) = err.as_str().filter(|e| !e.is_empty()) {
            bail!("{e}");
        }
        driver.type_text(&fill(text)).await?;
        driver.key("Enter").await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Some(key) = &step.key {
        driver.key(key).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if let Some(text) = &step.wait_for.as_deref().map(fill) {
        let timeout = step.timeout_ms.unwrap_or(10_000);
        wait_until(driver, &format!("__e2e.has({})", quoted(text)), timeout)
            .await
            .map_err(|_| {
                anyhow!(
                    "\u{201c}{text}\u{201d} didn't show within {} s",
                    timeout / 1000
                )
            })?;
    }
    if let Some(text) = &step.expect.as_deref().map(fill) {
        // A short grace period: the page may still be drawing.
        if wait_until(driver, &format!("__e2e.has({})", quoted(text)), 2_000)
            .await
            .is_err()
        {
            bail!("expected \u{201c}{text}\u{201d} on screen; it isn't there");
        }
    }
    if let Some(text) = &step.expect_not.as_deref().map(fill)
        && js(driver, &format!("__e2e.has({})", quoted(text))).await? == json!(true)
    {
        bail!("\u{201c}{text}\u{201d} is on screen, and shouldn't be");
    }
    if let Some(text) = &step.expect_disabled {
        match js(driver, &format!("__e2e.disabled({})", quoted(text))).await? {
            Value::Bool(true) => {}
            Value::Bool(false) => bail!("\u{201c}{text}\u{201d} should be disabled, but it works"),
            _ => bail!("nothing on screen says \u{201c}{text}\u{201d}"),
        }
    }
    if let Some(expr) = &step.expect_js
        && wait_until(driver, &format!("!!({expr})"), 2_000)
            .await
            .is_err()
    {
        bail!("this isn't true on the page: {expr}");
    }
    if let Some(ms) = step.wait_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    if step.close_sign_in_window == Some(true) {
        close_sign_in_window(&server.home).await?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    Ok(())
}

async fn wait_until(driver: &mut Driver, expression: &str, timeout_ms: u64) -> anyhow::Result<()> {
    let start = Instant::now();
    loop {
        if js(driver, expression).await.ok() == Some(json!(true)) {
            return Ok(());
        }
        if start.elapsed() > Duration::from_millis(timeout_ms) {
            bail!("timed out");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Closes oxbot's sign-in window in this home, as a person would after
/// signing in. The window is the Chrome for this home that was started on a
/// web page (hidden Chrome starts on a blank one); its `DevTools` port is
/// read from its command line.
async fn close_sign_in_window(home: &Path) -> anyhow::Result<()> {
    let out = tokio::process::Command::new("ps")
        .args(["-axo", "args"])
        .output()
        .await?;
    let marker = format!("--user-data-dir={}", home.display());
    let port = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&marker) && !l.contains("Helper") && !l.contains("about:blank"))
        .find_map(|l| {
            l.split_whitespace()
                .find_map(|a| a.strip_prefix("--remote-debugging-port="))
                .and_then(|p| p.parse::<u16>().ok())
        })
        .ok_or_else(|| anyhow!("no sign-in window is open"))?;
    let (_, version) = crate::http::request(port, "GET", "/json/version", None).await?;
    let v: Value = serde_json::from_str(&version)?;
    let ws = v["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| anyhow!("no browser link"))?;
    let (mut socket, _) = tokio_tungstenite::connect_async(ws).await?;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"id": 1, "method": "Browser.close"})
                .to_string()
                .into(),
        ))
        .await?;
    Ok(())
}

/// Runs `steps`, saving a screenshot after each into `shots_dir`. Stops at
/// the first failure (the rest are marked not run).
pub async fn run(
    driver: &mut Driver,
    server: &Server,
    steps: &[Step],
    shots_dir: &Path,
    shots_rel: &str,
) -> Vec<StepResult> {
    let mut results = Vec::new();
    let mut failed = false;
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        if failed {
            results.push(StepResult {
                n,
                text: step.describe(),
                note: step.note.clone(),
                status: "not_run",
                error: None,
                ms: 0,
                shot: None,
                compare: None,
            });
            continue;
        }
        let start = Instant::now();
        let outcome = run_step(driver, server, step).await;
        let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        // Let the page settle, then take the picture.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let file = format!("{n:02}.png");
        let shot = match driver.screenshot().await {
            Ok(png) => std::fs::write(shots_dir.join(&file), png)
                .ok()
                .map(|()| format!("{shots_rel}/{file}")),
            Err(_) => None,
        };
        failed = outcome.is_err();
        results.push(StepResult {
            n,
            text: step.describe(),
            note: step.note.clone(),
            status: if failed { "fail" } else { "pass" },
            error: outcome.err().map(|e| format!("{e:#}")),
            ms,
            shot,
            compare: None,
        });
    }
    results
}
