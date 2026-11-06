//! The sealed tab: where a bot's `read_pages` script runs. Never on the
//! live page: oxbot copies the page's HTML (in its own isolated world) and
//! the script reads the copy, in a separate blank tab that can't reach
//! anything:
//!
//! - its own browser context: none of the profile's cookies or storage;
//! - that context's proxy is a closed local port, loopback included, so no
//!   request of any kind (page loads, fetch, WebSocket) leaves it;
//! - the tab's network is emulated offline, and every address blocked;
//! - its page has a Content-Security-Policy that allows nothing to load,
//!   connect, frame or submit (only `eval`, for the bot's function);
//! - the copy is parsed with `DOMParser`: its scripts never run and its
//!   images, styles and frames never load;
//! - fetch, XHR, WebSocket, WebRTC, workers and beacons are removed from
//!   the tab's globals before the script runs (and Chrome itself starts
//!   with WebRTC limited to the proxy, see `Browser`);
//! - the script gets a time limit per page, and its result a size limit.
//!
//! A script written after reading web pages could have been steered by
//! them; here it can only read the copy and return data.
//!
//! The JavaScript is built here from plain strings (pure helpers, unit
//! tested); [`crate::Browser`] runs it over oxbot's own `DevTools`
//! connection.

use std::time::Duration;

use serde_json::{Value, json};

/// The most of a page's HTML oxbot copies.
pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// The most a script may return for one page (as JSON).
pub const MAX_RESULT_BYTES: usize = 512 * 1024;
/// How long a script may run on one page.
pub const SCRIPT_LIMIT: Duration = Duration::from_secs(5);
/// The longest script oxbot runs.
pub const MAX_SCRIPT_CHARS: usize = 20_000;

/// The sealed tab's own page: nothing may load, connect, frame or submit.
/// `unsafe-eval` only so the bot's function can be compiled.
pub const SEALED_CSP: &str = "default-src 'none'; script-src 'unsafe-eval'; style-src 'none'; \
    img-src 'none'; font-src 'none'; media-src 'none'; connect-src 'none'; frame-src 'none'; \
    child-src 'none'; worker-src 'none'; object-src 'none'; manifest-src 'none'; \
    form-action 'none'";

/// A proxy nothing listens on (the discard port on this Mac): every
/// request from the sealed context fails before it leaves.
pub const DEAD_PROXY: &str = "http://127.0.0.1:9";
/// No bypass at all, loopback included (Chrome bypasses it by default).
pub const NO_BYPASS: &str = "<-loopback>";

/// The sealed tab's document.
#[must_use]
pub fn sealed_document() -> String {
    format!(
        "<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\" \
         content=\"{SEALED_CSP}\"><title>oxbot sealed copy</title></head><body></body></html>"
    )
}

/// The parameters that create the sealed browser context.
#[must_use]
pub fn context_params() -> Value {
    json!({
        "disposeOnDetach": true,
        "proxyServer": DEAD_PROXY,
        "proxyBypassList": NO_BYPASS,
    })
}

/// The network emulation for the sealed tab: offline.
#[must_use]
pub fn offline_params() -> Value {
    json!({
        "offline": true,
        "latency": 0,
        "downloadThroughput": 0,
        "uploadThroughput": 0,
    })
}

/// The expression that copies the live page (run in oxbot's isolated
/// world): its HTML, cut at `max` characters, whether it was cut, and the
/// address Chrome says it shows.
#[must_use]
pub fn copy_expression(max: usize) -> String {
    format!(
        "(() => {{ const e = document.documentElement; const h = e ? e.outerHTML : ''; \
         return {{ html: h.length > {max} ? h.slice(0, {max}) : h, cut: h.length > {max}, \
         url: String(location.href) }}; }})()"
    )
}

/// A page as oxbot copied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// Its HTML (at most [`MAX_PAGE_BYTES`] characters).
    pub html: String,
    /// It was longer: only the start was copied.
    pub cut: bool,
    /// The address Chrome says the tab shows.
    pub url: String,
}

/// Reads what [`copy_expression`] returned.
#[must_use]
pub fn read_copy(v: &Value) -> Option<Copied> {
    Some(Copied {
        html: v.get("html")?.as_str()?.to_owned(),
        cut: v.get("cut").and_then(Value::as_bool).unwrap_or(false),
        url: v
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// A string as a JavaScript string literal (JSON's escaping is valid
/// JavaScript, ` ` and ` ` included).
fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Globals the sealed tab loses before the bot's script runs: everything
/// that could reach out, should the other walls fail.
const REMOVED: [&str; 14] = [
    "fetch",
    "XMLHttpRequest",
    "WebSocket",
    "WebTransport",
    "EventSource",
    "RTCPeerConnection",
    "webkitRTCPeerConnection",
    "RTCDataChannel",
    "Worker",
    "SharedWorker",
    "BroadcastChannel",
    "open",
    "Request",
    "importScripts",
];

/// The expression the sealed tab runs for one page: it parses `html` with
/// `DOMParser` (with `url` as its base, so links resolve), finds the next
/// page's link when `follow` is given (before the script runs, so the
/// script can't change it), compiles `script` on its own as an async function
/// body of `(doc, url)` (so it may `await`) (it can't break out of this wrapper), runs it within
/// `limit`, and returns `{ok, json, next}` or `{ok: false, error}`.
#[must_use]
pub fn sealed_expression(
    html: &str,
    url: &str,
    script: &str,
    follow: Option<&str>,
    limit: Duration,
) -> String {
    let removed = REMOVED
        .iter()
        .map(|k| js_string(k))
        .collect::<Vec<_>>()
        .join(", ");
    let follow = follow.map_or_else(|| "null".to_owned(), js_string);
    let limit_ms = limit.as_millis();
    format!(
        r#"(async () => {{
  "use strict";
  const html = {html}, url = {url}, body = {script}, follow = {follow};
  for (const k of [{removed}]) {{ try {{ delete globalThis[k]; }} catch (e) {{}} try {{ globalThis[k] = undefined; }} catch (e) {{}} }}
  try {{ Object.defineProperty(navigator, "sendBeacon", {{ value: undefined }}); }} catch (e) {{}}
  const fail = (e) => ({{ ok: false, error: String((e && e.message) || e).slice(0, 300) }});
  let doc;
  try {{
    doc = new DOMParser().parseFromString(html, "text/html");
    const base = doc.createElement("base");
    base.setAttribute("href", url);
    (doc.head || doc.documentElement).prepend(base);
  }} catch (e) {{ return fail(e); }}
  let next = null;
  if (follow) {{
    let a = null;
    try {{ a = doc.querySelector(follow); }} catch (e) {{}}
    if (!a) {{
      const want = follow.trim().toLowerCase();
      const links = [...doc.querySelectorAll("a[href], link[rel~=next][href]")];
      const words = (e) => (e.textContent || e.getAttribute("aria-label") || e.getAttribute("title") || "").replace(/\s+/g, " ").trim().toLowerCase();
      a = links.find((e) => words(e) === want) || links.find((e) => words(e).includes(want)) || null;
    }}
    if (a && !a.hasAttribute("href") && a.closest) a = a.closest("a[href]");
    if (a && a.href) next = String(a.href);
  }}
  let fn;
  try {{ fn = new (async () => {{}}).constructor("doc", "url", body); }} catch (e) {{ return fail(e); }}
  let out;
  try {{
    out = await Promise.race([
      Promise.resolve().then(() => fn(doc, url)),
      new Promise((_, no) => setTimeout(() => no(new Error("the script took longer than {limit_ms} ms")), {limit_ms})),
    ]);
  }} catch (e) {{ return fail(e); }}
  let json;
  try {{ json = JSON.stringify(out === undefined ? null : out); }} catch (e) {{ return fail(e); }}
  if (json === undefined) json = "null";
  if (json.length > {max}) return {{ ok: false, error: "the script returned more than {max} characters for this page; return fewer fields or rows" }};
  return {{ ok: true, json, next }};
}})()"#,
        html = js_string(html),
        url = js_string(url),
        script = js_string(script),
        max = MAX_RESULT_BYTES,
    )
}

/// What the script made of one page.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    /// What it returned.
    pub value: Value,
    /// The next page's address, when following and the copy has one.
    pub next: Option<String>,
}

/// Reads what [`sealed_expression`] returned (Chrome's `Runtime.evaluate`
/// result, by value).
///
/// # Errors
/// What went wrong, in plain words: the script threw, took too long or
/// returned too much, or Chrome stopped it.
pub fn read_outcome(reply: &Value) -> Result<Ran, String> {
    if let Some(ex) = reply.get("exceptionDetails") {
        let text = ex
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .or_else(|| ex.get("text").and_then(Value::as_str))
            .unwrap_or("the script failed");
        let first = text.lines().next().unwrap_or(text);
        return Err(if first.contains("Execution was terminated") {
            format!(
                "the script took longer than {} seconds",
                SCRIPT_LIMIT.as_secs()
            )
        } else {
            first.chars().take(300).collect()
        });
    }
    let v = reply.pointer("/result/value").unwrap_or(&Value::Null);
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the script didn't finish")
            .to_owned());
    }
    let json = v.get("json").and_then(Value::as_str).unwrap_or("null");
    let value = serde_json::from_str(json).map_err(|e| format!("its result isn't JSON: {e}"))?;
    let next = v
        .get("next")
        .and_then(Value::as_str)
        .filter(|n| n.starts_with("http://") || n.starts_with("https://"))
        .map(str::to_owned);
    Ok(Ran { value, next })
}

/// A sealed tab, while it's open: its context, tab and `DevTools` session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SealedTab {
    pub(crate) context: String,
    pub(crate) session: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped_as_javascript_literals() {
        assert_eq!(js_string("a\"b\\c\n</script>"), r#""a\"b\\c\n</script>""#);
        // Line and paragraph separators stay valid inside a JS string.
        let s = js_string("x\u{2028}y");
        assert!(s.starts_with('"') && s.ends_with('"'));
    }

    #[test]
    fn the_script_is_compiled_on_its_own_and_cannot_break_out() {
        let evil = "return 1; }); fetch('https://evil.example/'); (() => {";
        let expr = sealed_expression("<p>hi</p>", "https://a.example/", evil, None, SCRIPT_LIMIT);
        // The body only ever appears as a quoted string for `new Function`.
        assert!(expr.contains(&js_string(evil)));
        assert_eq!(expr.matches("return 1; });").count(), 1);
        assert!(expr.contains(r#".constructor("doc", "url", body)"#));
        assert!(expr.contains("new DOMParser().parseFromString(html, \"text/html\")"));
        assert!(expr.contains("\"fetch\", \"XMLHttpRequest\", \"WebSocket\""));
        assert!(expr.contains("RTCPeerConnection"));
        assert!(expr.contains("5000"));
        assert!(expr.contains("follow = null"));
    }

    #[test]
    fn following_names_the_link_before_the_script_runs() {
        let expr = sealed_expression(
            "",
            "https://a.example/",
            "return 1",
            Some("Next ›"),
            SCRIPT_LIMIT,
        );
        assert!(expr.contains("follow = \"Next ›\""));
        let at_follow = expr.find("let next = null").unwrap();
        let at_script = expr.find("fn = new (async").unwrap();
        assert!(at_follow < at_script);
    }

    #[test]
    fn the_sealed_page_lets_nothing_load_or_connect() {
        let doc = sealed_document();
        for d in [
            "default-src 'none'",
            "connect-src 'none'",
            "frame-src 'none'",
            "worker-src 'none'",
            "form-action 'none'",
            "img-src 'none'",
        ] {
            assert!(doc.contains(d), "{d}");
        }
        assert!(!doc.contains("unsafe-inline"));
        let ctx = context_params();
        assert_eq!(ctx["proxyServer"], DEAD_PROXY);
        assert_eq!(ctx["proxyBypassList"], "<-loopback>");
        assert_eq!(offline_params()["offline"], true);
    }

    #[test]
    fn copies_are_read_back() {
        assert!(copy_expression(10).contains("h.slice(0, 10)"));
        let c =
            read_copy(&json!({"html": "<html></html>", "cut": true, "url": "https://a.example/"}))
                .unwrap();
        assert!(c.cut);
        assert_eq!(c.url, "https://a.example/");
        assert!(read_copy(&json!({"cut": false})).is_none());
    }

    #[test]
    fn outcomes_are_read_in_plain_words() {
        let ok = read_outcome(&json!({"result": {"type": "object", "value": {
            "ok": true, "json": "[{\"name\":\"Lamp\"}]", "next": "https://a.example/?page=2"}}}))
        .unwrap();
        assert_eq!(ok.value, json!([{"name": "Lamp"}]));
        assert_eq!(ok.next.as_deref(), Some("https://a.example/?page=2"));
        // A next that isn't a web page is dropped.
        let js = read_outcome(
            &json!({"result": {"value": {"ok": true, "json": "1", "next": "javascript:go()"}}}),
        )
        .unwrap();
        assert_eq!(js.next, None);
        assert_eq!(
            read_outcome(&json!({"result": {"value": {"ok": false, "error": "x is not defined"}}}))
                .unwrap_err(),
            "x is not defined"
        );
        assert_eq!(
            read_outcome(&json!({"exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: Execution was terminated\n at x"}}}))
                .unwrap_err(),
            "the script took longer than 5 seconds"
        );
        assert!(read_outcome(&json!({})).is_err());
    }
}
