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

/// Where on the page the element showing `text` is, once it's in view and
/// nothing covers it.
async fn point(driver: &mut Driver, text: &str) -> anyhow::Result<(f64, f64)> {
    let p = js(driver, &format!("__e2e.point({})", quoted(text))).await?;
    if let Some(e) = p["error"].as_str() {
        bail!("{e}");
    }
    Ok((
        p["x"].as_f64().unwrap_or(0.0),
        p["y"].as_f64().unwrap_or(0.0),
    ))
}

/// Runs one step. Returns `Ok` if it passed.
async fn run_step(driver: &mut Driver, server: &mut Server, step: &Step) -> anyhow::Result<()> {
    if step.restart == Some(true) {
        server.restart().await.context("restarting oxbot")?;
    }
    page_step(driver, server, step).await?;
    if let Some(ms) = step.wait_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    if step.close_sign_in_window == Some(true) {
        close_sign_in_window(&server.home, step.timeout_ms.unwrap_or(10_000)).await?;
    }
    loop_steps(driver, server, step).await
}

/// A step's actions on the page: open, click, type, send, wait and check.
async fn page_step(driver: &mut Driver, server: &Server, step: &Step) -> anyhow::Result<()> {
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
        let (x, y) = point(driver, text).await?;
        driver.click_at(x, y).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Some(text) = &step.right_click {
        let (x, y) = point(driver, text).await?;
        driver.right_click_at(x, y).await?;
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
    if let Some(files) = &step.attach {
        attach(driver, server, files).await?;
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
        && wait_until(
            driver,
            &format!("!!({expr})"),
            step.timeout_ms.unwrap_or(2_000),
        )
        .await
        .is_err()
    {
        bail!("this isn't true on the page: {expr}");
    }
    Ok(())
}

/// The steps with the stand-ins: following a link (the sign-in page) and
/// checking what the stand-in `OpenRouter` was asked.
async fn loop_steps(driver: &mut Driver, server: &Server, step: &Step) -> anyhow::Result<()> {
    if let Some(text) = &step.follow {
        follow(driver, text).await?;
    }
    if let Some(text) = &step.expect_openrouter {
        let requests = openrouter_requests(server)?;
        let start = Instant::now();
        while !requests().lines().any(|l| l.contains(text.as_str())) {
            if start.elapsed() > Duration::from_secs(3) {
                bail!("the stand-in OpenRouter got no request with {text}");
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    if let Some(want) = step.expect_site_writes {
        let got = server.site.writes();
        if got != want {
            bail!("the test site got {got} writes, not {want}");
        }
    }
    if let Some(want) = step.expect_site_pages {
        let start = Instant::now();
        while server.site.pages() < want {
            if start.elapsed() > Duration::from_secs(5) {
                bail!(
                    "the test site's page was loaded {} times, not {want}",
                    server.site.pages()
                );
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    if let Some(text) = &step.expect_openrouter_not {
        let fake = stand_in(server)?;
        let (log, bodies) = (fake.requests(), fake.bodies());
        if let Some(line) = log.lines().find(|l| l.contains(text.as_str())) {
            bail!("the stand-in OpenRouter got a request with {text}: {line}");
        }
        if bodies.lines().any(|l| l.contains(text.as_str())) {
            bail!("a request body sent to the stand-in OpenRouter has {text}");
        }
    }
    if let Some(check) = &step.expect_db {
        let db = server.home.join("oxbot.db");
        let start = Instant::now();
        loop {
            let got = first_value(&db, &check.query)?;
            if got.as_deref() == Some(check.equals.as_str()) {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                bail!("{} gave {got:?}, not {:?}", check.query, check.equals);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    if let Some(text) = &step.expect_openrouter_body {
        let fake = stand_in(server)?;
        let start = Instant::now();
        while !fake.bodies().lines().any(|l| l.contains(text.as_str())) {
            if start.elapsed() > Duration::from_secs(3) {
                bail!("no request body sent to the stand-in OpenRouter has {text}");
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    Ok(())
}

/// The first value `query` gives on database `db` (opened read-only), as
/// text; `None` when there's no row.
fn first_value(db: &Path, query: &str) -> anyhow::Result<Option<String>> {
    let trimmed = query.trim_start().to_ascii_lowercase();
    if !trimmed.starts_with("select") {
        bail!("expect_db takes a SELECT");
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("couldn't open {}", db.display()))?;
    let mut stmt = conn.prepare(query).context("expect_db's query")?;
    let mut rows = stmt.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let value: rusqlite::types::Value = row.get(0)?;
    Ok(Some(match value {
        rusqlite::types::Value::Null => "null".to_owned(),
        rusqlite::types::Value::Integer(n) => n.to_string(),
        rusqlite::types::Value::Real(x) => x.to_string(),
        rusqlite::types::Value::Text(t) => t,
        rusqlite::types::Value::Blob(b) => format!("{} bytes", b.len()),
    }))
}

/// The test's stand-in `OpenRouter`.
fn stand_in(server: &Server) -> anyhow::Result<&crate::serve::FakeOpenRouter> {
    server
        .openrouter
        .as_ref()
        .ok_or_else(|| anyhow!("no stand-in OpenRouter runs (see e2e/pr_test.toml)"))
}

/// Reads the stand-in `OpenRouter`'s request log.
fn openrouter_requests(server: &Server) -> anyhow::Result<impl Fn() -> String + '_> {
    let fake = stand_in(server)?;
    Ok(|| fake.requests())
}

/// Whether `url` is plain http to this Mac (127.0.0.1 or localhost, with a
/// port), with no user part that could hide another host.
#[must_use]
pub fn is_local_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    !authority.contains('@')
        && matches!(host, "127.0.0.1" | "localhost")
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
}

/// Goes where the link showing `text` points. Only local addresses: tests
/// never leave this Mac.
async fn follow(driver: &mut Driver, text: &str) -> anyhow::Result<()> {
    let href = js(
        driver,
        &format!(
            "(() => {{ const el = __e2e.find({}, false); \
             const a = el && (el.closest('a[href]') || el.querySelector('a[href]')); \
             return a ? a.href : null; }})()",
            quoted(text)
        ),
    )
    .await?;
    let href = href
        .as_str()
        .ok_or_else(|| anyhow!("no link says \u{201c}{text}\u{201d}"))?;
    if !is_local_url(href) {
        bail!(
            "the link \u{201c}{text}\u{201d} leaves this Mac ({href}); tests only follow local links"
        );
    }
    driver.open(href).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    Ok(())
}

/// Chooses `files` (from the repo) in the composer's file picker.
async fn attach(driver: &mut Driver, server: &Server, files: &[String]) -> anyhow::Result<()> {
    let paths: Vec<String> = files
        .iter()
        .map(|f| server.repo.join(f).display().to_string())
        .collect();
    driver.set_files("#fileInput", &paths).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
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
/// signing in, once it's open (waiting up to `timeout_ms` for it: Chrome
/// can take a while to start on a busy Mac). The window is the Chrome for
/// this home that was started on a web page (hidden Chrome starts on a
/// blank one); its `DevTools` port is read from its command line.
async fn close_sign_in_window(home: &Path, timeout_ms: u64) -> anyhow::Result<()> {
    let start = Instant::now();
    let version = loop {
        if let Some(port) = sign_in_window_port(home).await?
            && let Ok((200, version)) =
                crate::http::request(port, "GET", "/json/version", None).await
        {
            break version;
        }
        if start.elapsed() > Duration::from_millis(timeout_ms) {
            bail!("no sign-in window opened within {} s", timeout_ms / 1000);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
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

/// The `DevTools` port of this home's sign-in window, if one runs.
async fn sign_in_window_port(home: &Path) -> anyhow::Result<Option<u16>> {
    let out = tokio::process::Command::new("ps")
        .args(["-axo", "args"])
        .output()
        .await?;
    let marker = format!("--user-data-dir={}", home.display());
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&marker) && !l.contains("Helper") && !l.contains("about:blank"))
        .find_map(|l| {
            l.split_whitespace()
                .find_map(|a| a.strip_prefix("--remote-debugging-port="))
                .and_then(|p| p.parse::<u16>().ok())
        }))
}

/// Runs `steps`, saving a screenshot after each into `shots_dir`. Stops at
/// the first failure (the rest are marked not run).
pub async fn run(
    driver: &mut Driver,
    server: &mut Server,
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

#[cfg(test)]
mod tests {
    use super::is_local_url;

    #[test]
    fn only_local_links_are_followed() {
        assert!(is_local_url("http://127.0.0.1:5000/auth?callback_url=x"));
        assert!(is_local_url("http://localhost:7878/openrouter/callback"));
        assert!(!is_local_url("https://openrouter.ai/auth"));
        assert!(!is_local_url("http://127.0.0.1:80@evil.example/"));
        assert!(!is_local_url("http://127.0.0.1.evil.example:80/"));
        assert!(!is_local_url("http://127.0.0.1/"));
        assert!(!is_local_url("file:///etc/passwd"));
    }
}
