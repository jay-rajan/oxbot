//! A run's results (`results.json`) and the report page made from them.

use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::steps::StepResult;

/// One test in one browser.
#[derive(Debug, Clone, Serialize)]
pub struct TestRun {
    /// The test's id (its file name).
    pub id: String,
    /// Its file.
    pub file: String,
    /// Its name.
    pub name: String,
    /// What it checks.
    pub about: String,
    /// Why it ran.
    pub why: String,
    /// `chrome` or `safari`.
    pub engine: String,
    /// Who it ran as.
    pub as_person: String,
    /// `pass`, `broken` or `changed`.
    pub status: String,
    /// How long it took.
    pub ms: u64,
    /// Its steps.
    pub steps: Vec<StepResult>,
    /// oxbot's log, if something broke.
    pub log: Option<String>,
}

/// Reads `results.json` in `run_dir` (empty if there's none yet).
///
/// # Errors
/// Fails if it exists but can't be read.
pub fn load(run_dir: &Path) -> anyhow::Result<serde_json::Value> {
    let path = run_dir.join("results.json");
    if !path.exists() {
        return Ok(serde_json::json!({ "started_ms": crate::now_ms(), "tests": [] }));
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

/// Writes `results.json`.
///
/// # Errors
/// Fails if it can't be written.
pub fn save(run_dir: &Path, results: &serde_json::Value) -> anyhow::Result<()> {
    std::fs::write(
        run_dir.join("results.json"),
        serde_json::to_string_pretty(results)?,
    )?;
    Ok(())
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Standard base64, for embedding pictures in the page.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn img(run_dir: &Path, rel: Option<&str>, alt: &str) -> String {
    let Some(rel) = rel else { return String::new() };
    match std::fs::read(run_dir.join(rel)) {
        Ok(bytes) => format!(
            "<img loading=\"lazy\" alt=\"{}\" src=\"data:image/png;base64,{}\">",
            esc(alt),
            base64(&bytes)
        ),
        Err(_) => String::new(),
    }
}

/// Text with `- ` lines as a list and blank lines as paragraph breaks.
fn prose(text: &str) -> String {
    let mut html = String::new();
    let mut in_list = false;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(item) = line.strip_prefix("- ") {
            if !in_list {
                html.push_str("<ul>");
                in_list = true;
            }
            let _ = write!(html, "<li>{}</li>", code_spans(&esc(item)));
        } else {
            if in_list {
                html.push_str("</ul>");
                in_list = false;
            }
            if !line.is_empty() {
                let _ = write!(html, "<p>{}</p>", code_spans(&esc(line)));
            }
        }
    }
    if in_list {
        html.push_str("</ul>");
    }
    html
}

fn code_spans(text: &str) -> String {
    let mut out = String::new();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            let _ = write!(out, "<code>{part}</code>");
        } else {
            out.push_str(part);
        }
    }
    out
}

/// Makes `index.html` in `run_dir` from `results.json`.
///
/// # Errors
/// Fails if the results can't be read or the page written.
pub fn write(
    run_dir: &Path,
    intent: &str,
    fixes: &str,
    notes: &str,
    skipped: &[String],
) -> anyhow::Result<()> {
    let results = load(run_dir)?;
    let tests = results["tests"].as_array().cloned().unwrap_or_default();
    let count = |s: &str| tests.iter().filter(|t| t["status"] == s).count();
    let (broken, changed, passed) = (count("broken"), count("changed"), count("pass"));
    let app = results.get("app").filter(|a| !a.is_null());
    let app_failed = app.is_some_and(|a| {
        a["checks"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["status"] == "fail"))
    });
    let (verdict, tone) = if broken > 0 || app_failed {
        (
            format!("{} broken", broken + usize::from(app_failed)),
            "bad",
        )
    } else if changed > 0 {
        (
            format!(
                "All passed · {changed} screen{} changed",
                if changed == 1 { "" } else { "s" }
            ),
            "warn",
        )
    } else {
        ("All passed".to_owned(), "good")
    };
    let when = results["started_ms"].as_i64().unwrap_or(0);

    let mut body = String::new();
    let _ = write!(
        body,
        "<header><p class=\"eyebrow\">oxbot · /pr_test</p><h1>Test run</h1>\
         <p class=\"lede\">{}</p><div class=\"meta\"><span class=\"chip {tone}\">{}</span>\
         <span>{} test runs · {passed} passed · {changed} changed · {broken} broken</span>\
         <span class=\"when\" data-ms=\"{when}\"></span></div></header>",
        if intent.is_empty() {
            "End-to-end tests against a seeded copy of oxbot.".to_owned()
        } else {
            esc(intent)
        },
        esc(&verdict),
        tests.len()
    );
    if !fixes.trim().is_empty() {
        let _ = write!(
            body,
            "<section class=\"box\"><h2>What I fixed</h2>{}</section>",
            prose(fixes)
        );
    }
    for skip in results["skipped_engines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let _ = write!(
            body,
            "<section class=\"box\"><h2>Not run in {}</h2><p>{}</p></section>",
            if skip["engine"] == "safari" {
                "Safari"
            } else {
                "Chrome"
            },
            esc(skip["why"].as_str().unwrap_or(""))
        );
    }
    if !notes.trim().is_empty() {
        let _ = write!(
            body,
            "<section class=\"box\"><h2>Notes</h2>{}</section>",
            prose(notes)
        );
    }

    body.push_str(&summary(&tests, skipped));
    if let Some(app) = app {
        body.push_str(&app_section(run_dir, app));
    }
    for t in &tests {
        body.push_str(&test_section(run_dir, t));
    }

    let page = TEMPLATE.replace("{{BODY}}", &body);
    std::fs::write(run_dir.join("index.html"), page)?;
    Ok(())
}

/// The table of tests, and the ones not needed.
fn summary(tests: &[serde_json::Value], skipped: &[String]) -> String {
    let mut body = String::new();
    body.push_str("<section><h2>Tests</h2><div class=\"tbl\"><table><thead><tr><th>Test</th><th>Browser</th><th>Why it ran</th><th>Result</th><th class=\"num\">Steps</th><th class=\"num\">Time</th></tr></thead><tbody>");
    for t in tests {
        let steps = t["steps"].as_array().map_or(0, Vec::len);
        let _ = write!(
            body,
            "<tr><td><a href=\"#{id}-{eng}\">{name}</a></td><td>{eng}</td><td>{why}</td><td><span class=\"chip {tone}\">{status}</span></td><td class=\"num\">{steps}</td><td class=\"num\">{secs}</td></tr>",
            id = esc(t["id"].as_str().unwrap_or("")),
            eng = esc(t["engine"].as_str().unwrap_or("")),
            name = esc(t["name"].as_str().unwrap_or("")),
            why = esc(t["why"].as_str().unwrap_or("")),
            tone = tone_of(t["status"].as_str().unwrap_or("")),
            status = label_of(t["status"].as_str().unwrap_or("")),
            secs = seconds(t["ms"].as_u64().unwrap_or(0)),
        );
    }
    body.push_str("</tbody></table></div>");
    if !skipped.is_empty() {
        let _ = write!(
            body,
            "<details class=\"skipped\"><summary>{} test{} not needed for this change</summary><ul>{}</ul></details>",
            skipped.len(),
            if skipped.len() == 1 { "" } else { "s" },
            skipped.iter().fold(String::new(), |mut list, s| {
                let _ = write!(list, "<li><code>{}</code></li>", esc(s));
                list
            })
        );
    }
    body.push_str("</section>");

    body
}

/// The Mac app check, as a section.
fn app_section(run_dir: &Path, app: &serde_json::Value) -> String {
    let mut body = String::new();
    body.push_str("<section class=\"test\"><h2>The Mac app</h2><p class=\"about\">The built oxbot.app on the seed data: does it start, open its window and show its menu-bar icon?</p><ol class=\"steps\">");
    for c in app["checks"].as_array().cloned().unwrap_or_default() {
        let s = c["status"].as_str().unwrap_or("");
        let _ = write!(
            body,
            "<li class=\"step\"><div class=\"row\"><span class=\"chip {}\">{}</span><span class=\"what\">{}</span></div><p class=\"detail\">{}</p></li>",
            tone_of(s),
            label_of(s),
            esc(c["what"].as_str().unwrap_or("")),
            esc(c["detail"].as_str().unwrap_or(""))
        );
    }
    body.push_str("</ol>");
    let shot = img(run_dir, app["shot"].as_str(), "The oxbot app's window");
    if !shot.is_empty() {
        let _ = write!(
            body,
            "<figure class=\"shot\">{shot}<figcaption>The app's window</figcaption></figure>"
        );
    }
    body.push_str("</section>");
    body
}

/// One test run, as a section: its steps and screens.
fn test_section(run_dir: &Path, t: &serde_json::Value) -> String {
    let mut body = String::new();
    let id = esc(t["id"].as_str().unwrap_or(""));
    let eng = esc(t["engine"].as_str().unwrap_or(""));
    let status = t["status"].as_str().unwrap_or("");
    let _ = write!(
        body,
        "<section class=\"test\" id=\"{id}-{eng}\"><div class=\"test-head\"><h2>{}</h2><span class=\"chip {}\">{}</span></div>\
         <p class=\"about\">{}</p><p class=\"facts\"><code>{}</code> · {eng} · as {} · {}</p><ol class=\"steps\">",
        esc(t["name"].as_str().unwrap_or("")),
        tone_of(status),
        label_of(status),
        esc(t["about"].as_str().unwrap_or("")),
        esc(t["file"].as_str().unwrap_or("")),
        esc(t["as_person"].as_str().unwrap_or("")),
        esc(t["why"].as_str().unwrap_or("")),
    );
    for s in t["steps"].as_array().cloned().unwrap_or_default() {
        let st = s["status"].as_str().unwrap_or("");
        let cmp = &s["compare"];
        let cmp_status = cmp["status"].as_str().unwrap_or("");
        let flag = match (st, cmp_status) {
            ("pass", "changed") => "changed",
            (other, _) => other,
        };
        let _ = write!(
            body,
            "<li class=\"step\"><div class=\"row\"><span class=\"n\">{}</span><span class=\"what\">{}</span><span class=\"chip {}\">{}</span><span class=\"ms\">{} ms</span></div>",
            s["n"],
            esc(s["text"].as_str().unwrap_or("")),
            tone_of(flag),
            label_of(flag),
            s["ms"].as_u64().unwrap_or(0)
        );
        if let Some(note) = s["note"].as_str() {
            let _ = write!(body, "<p class=\"detail\">{}</p>", esc(note));
        }
        if let Some(err) = s["error"].as_str() {
            let _ = write!(body, "<p class=\"error\">{}</p>", esc(err));
        }
        let now = img(run_dir, s["shot"].as_str(), &format!("Step {} now", s["n"]));
        if cmp_status == "changed" {
            let base = img(run_dir, cmp["baseline"].as_str(), "Approved before");
            let diff = img(run_dir, cmp["diff"].as_str(), "What changed, in red");
            let _ = write!(
                body,
                "<div class=\"compare\"><figure>{base}<figcaption>Approved</figcaption></figure><figure>{now}<figcaption>Now</figcaption></figure><figure>{diff}<figcaption>Changed ({:.1}% of the screen)</figcaption></figure></div>",
                cmp["percent"].as_f64().unwrap_or(0.0)
            );
        } else if !now.is_empty() {
            let cap = match cmp_status {
                "new" => "No approved picture yet",
                "same" => "Same as approved",
                _ => "Screen",
            };
            let _ = write!(
                body,
                "<figure class=\"thumb\">{now}<figcaption>{cap} · click to enlarge</figcaption></figure>"
            );
        }
        body.push_str("</li>");
    }
    body.push_str("</ol>");
    if let Some(log) = t["log"].as_str().filter(|l| !l.is_empty()) {
        let _ = write!(
            body,
            "<details><summary>oxbot's log at the end</summary><pre>{}</pre></details>",
            esc(log)
        );
    }
    body.push_str("</section>");
    body
}

/// `4812` ms → `4.8 s`.
fn seconds(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, ms % 1000 / 100)
}

fn tone_of(status: &str) -> &'static str {
    match status {
        "pass" | "same" => "good",
        "broken" | "fail" => "bad",
        "changed" | "new" => "warn",
        _ => "muted",
    }
}

fn label_of(status: &str) -> &'static str {
    match status {
        "pass" => "Pass",
        "broken" | "fail" => "Broken",
        "changed" => "Changed",
        "new" => "New",
        "not_run" => "Not run",
        "skipped" => "Skipped",
        _ => "—",
    }
}

const TEMPLATE: &str = r#"<meta charset="utf-8">
<title>pr_test report</title>
<style>
:root { --bg:#FBFBFA; --surface:#FFFFFF; --ink:#16171A; --muted:#5D6068; --line:#E3E3DF; --good:#1F7A46; --good-soft:#E3F3E9; --bad:#C2241D; --bad-soft:#FCE6E4; --warn:#9A5B00; --warn-soft:#FFF1D9; --accent:#FF3737; --code:#F2F2EF; }
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { --bg:#15171B; --surface:#1D2026; --ink:#EDEDEA; --muted:#A3A7AF; --line:#2D3139; --good:#5FCB8C; --good-soft:#18301F; --bad:#FF8A80; --bad-soft:#3A1C1A; --warn:#F2B04A; --warn-soft:#33270F; --accent:#FF5A5A; --code:#121418; color-scheme: dark; } }
:root[data-theme="dark"] { --bg:#15171B; --surface:#1D2026; --ink:#EDEDEA; --muted:#A3A7AF; --line:#2D3139; --good:#5FCB8C; --good-soft:#18301F; --bad:#FF8A80; --bad-soft:#3A1C1A; --warn:#F2B04A; --warn-soft:#33270F; --accent:#FF5A5A; --code:#121418; color-scheme: dark; }
* { box-sizing: border-box; }
body { background: var(--bg); color: var(--ink); margin: 0; font: 15px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; }
main { max-width: 1080px; margin: 0 auto; padding-inline: 16px; padding-block: 28px 64px; display: grid; gap: 28px; }
h1 { font-size: 1.9rem; margin: 0; text-wrap: balance; } h2 { font-size: 1.15rem; margin: 0; text-wrap: balance; }
.eyebrow { margin: 0; font-size: .72rem; letter-spacing: .08em; text-transform: uppercase; color: var(--muted); }
.lede { margin: 6px 0 10px; color: var(--muted); max-width: 70ch; }
.meta { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; font-size: .88rem; color: var(--muted); font-variant-numeric: tabular-nums; }
.chip { display: inline-block; border-radius: 99px; padding: 1px 10px; font-size: .78rem; font-weight: 600; white-space: nowrap; }
.chip.good { background: var(--good-soft); color: var(--good); } .chip.bad { background: var(--bad-soft); color: var(--bad); }
.chip.warn { background: var(--warn-soft); color: var(--warn); } .chip.muted { background: var(--code); color: var(--muted); }
section { display: grid; gap: 10px; min-width: 0; }
.box { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 16px 18px; }
.box p, .box ul { margin: 0; } .box ul { padding-left: 20px; }
code { font: .85em ui-monospace, "SF Mono", Menlo, monospace; background: var(--code); padding: 1px 5px; border-radius: 4px; overflow-wrap: anywhere; }
.tbl { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: .88rem; }
th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--line); vertical-align: top; }
th { color: var(--muted); font-weight: 600; font-size: .78rem; } .num { text-align: right; font-variant-numeric: tabular-nums; }
td a { color: var(--ink); text-decoration-color: var(--accent); }
.skipped summary { color: var(--muted); font-size: .88rem; cursor: pointer; }
.test { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 18px; }
.test-head { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
.about { margin: 0; color: var(--ink); max-width: 75ch; } .facts { margin: 0; color: var(--muted); font-size: .82rem; }
.steps { list-style: none; margin: 6px 0 0; padding: 0; display: grid; gap: 4px; }
.step { border-top: 1px solid var(--line); padding: 8px 0; display: grid; gap: 6px; min-width: 0; }
.row { display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
.n { color: var(--muted); font-variant-numeric: tabular-nums; min-width: 1.6em; } .what { flex: 1 1 260px; min-width: 0; overflow-wrap: anywhere; }
.ms { color: var(--muted); font-size: .78rem; font-variant-numeric: tabular-nums; }
.detail { margin: 0 0 0 2.2em; color: var(--muted); font-size: .85rem; }
.error { margin: 0 0 0 2.2em; color: var(--bad); font-size: .88rem; font-weight: 500; }
.thumb { margin-left: 2.2em; width: min(380px, calc(100% - 2.2em)); }
.thumb img { cursor: zoom-in; } .thumb.big { width: calc(100% - 2.2em); } .thumb.big img { cursor: zoom-out; }
img { max-width: 100%; height: auto; border: 1px solid var(--line); border-radius: 8px; display: block; margin-top: 6px; }
.compare { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px; margin-left: 2.2em; }
figure { margin: 0; min-width: 0; } figcaption { font-size: .78rem; color: var(--muted); margin-top: 4px; }
pre { background: var(--code); padding: 10px; border-radius: 8px; overflow-x: auto; font: 12px/1.45 ui-monospace, Menlo, monospace; max-height: 320px; }
</style>
<main>{{BODY}}</main>
<script>document.addEventListener("click", (e) => { const f = e.target.closest(".thumb"); if (f) f.classList.toggle("big"); });
for (const el of document.querySelectorAll(".when")) { const ms = +el.dataset.ms; if (ms) el.textContent = new Date(ms).toLocaleString(); }</script>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(
            crate::http::base64_decode(&base64(b"oxbot!")).unwrap(),
            b"oxbot!"
        );
    }

    #[test]
    fn prose_makes_lists_and_code() {
        assert_eq!(
            prose("Fixed:\n- `hub.rs` lock"),
            "<p>Fixed:</p><ul><li><code>hub.rs</code> lock</li></ul>"
        );
    }
}
