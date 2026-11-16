//! A stand-in for the browser tool (`chrome-devtools-mcp` 1.10.1), which
//! bots run through oxbot's tool service: an MCP
//! server over stdio (one JSON-RPC message per line) that offers the
//! browser tools oxbot lists, with the real tool's schemas, and answers
//! from the same reply scripts as the stand-in `OpenRouter`.
//!
//! - `navigate_page` / `new_page` open a page (remembered per process);
//! - `take_snapshot` gives the script's snapshot whose selected page is the
//!   open one (the first such snapshot in the script), else a blank page;
//! - `list_pages` lists the open page; `take_screenshot` gives a tiny PNG;
//! - every input tool (`click`, `fill`, …) says "done".
//!
//! oxbot's real browser (Camoufox, headless) still runs for leases,
//! screenshots and page facts. A page on the test site (a `.test` host,
//! reached through the test site as the browser's proxy) is also opened in
//! it, through oxbot's browser address (the real tool's `--endpoint`), so
//! its network can be checked; no other page is ever loaded. Each call is logged (tool and arguments) to
//! `OXBOT_FAKE_DT_LOG` if set, so tests can check what the loop did.

use std::io::Write as _;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::script::{Page, Script};

/// The real tool's schemas for the tools the loop uses, captured from
/// `chrome-devtools-mcp` 1.10.1's `tools/list`.
const TOOLS: &str = include_str!("devtools/tools-1.10.1.json");

/// The MCP version spoken when the client doesn't say.
const PROTOCOL: &str = "2025-06-18";

/// A 1×1 PNG, for screenshots.
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// The browser tool's tools (name, description, `inputSchema`, annotations).
#[must_use]
pub fn tools() -> Vec<Value> {
    serde_json::from_str(TOOLS).unwrap_or_default()
}

/// One stand-in browser: the script and the page it has open.
#[derive(Debug)]
pub struct Devtools {
    script: Script,
    tools: Vec<Value>,
    open: Option<String>,
    log: Option<PathBuf>,
    /// oxbot's browser address and the bot's folder, from `--endpoint` and
    /// `--workspace`.
    browser: Option<(String, String)>,
}

impl Devtools {
    /// A stand-in answering from `script`, logging calls to `log`.
    #[must_use]
    pub fn new(script: Script, log: Option<PathBuf>) -> Self {
        Self {
            script,
            tools: tools(),
            open: None,
            log,
            browser: None,
        }
    }

    /// Opens test-site pages in oxbot's browser at `endpoint` (the real
    /// tool's `--endpoint`, oxbot's `http://127.0.0.1:…/<token>`), as the
    /// bot working in `workspace` (its `--workspace`).
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: &str, workspace: Option<&str>) -> Self {
        self.browser = Some((endpoint.to_owned(), workspace.unwrap_or_default().to_owned()));
        self
    }

    /// Opens `url` in oxbot's browser, if it's on the test site: one
    /// `navigate_page` through oxbot's browser address, as the real tool
    /// sends it. The answer comes once the page has loaded.
    fn load_in_browser(&self, url: &str) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let Some((endpoint, workspace)) = &self.browser else {
            return;
        };
        let host = url
            .strip_prefix("http://")
            .and_then(|rest| rest.split(['/', ':', '?', '#']).next())
            .unwrap_or("");
        if !host.to_ascii_lowercase().ends_with(".test") || url.contains(char::is_whitespace) {
            return;
        }
        let operation = format!(
            "fake-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let body = json!({
            "name": "navigate_page",
            "args": { "url": url },
            "workspace": workspace,
            "operation": operation,
        });
        let _ = post_local(endpoint, &body.to_string());
    }

    /// Answers one JSON-RPC message; `None` for notifications.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id")?.clone();
        let params = &msg["params"];
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "chrome_devtools",
                    "title": "Chrome DevTools MCP server (e2e stand-in)",
                    "version": "1.10.1"
                }
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tools })),
            "tools/call" => Ok(self.call(
                params["name"].as_str().unwrap_or(""),
                params.get("arguments").unwrap_or(&Value::Null),
            )),
            other => {
                Err(json!({ "code": -32601, "message": format!("Method not found: {other}") }))
            }
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        })
    }

    fn call(&mut self, name: &str, args: &Value) -> Value {
        self.log_call(name, args);
        let Some(tool) = self.tools.iter().find(|t| t["name"] == name) else {
            return failed(&format!("Unknown tool: {name}"));
        };
        let missing: Vec<&str> = tool["inputSchema"]["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|k| args.get(*k).is_none_or(Value::is_null))
            .collect();
        if !missing.is_empty() {
            return failed(&format!(
                "Invalid arguments for tool {name}: missing {}",
                missing.join(", ")
            ));
        }
        match name {
            "navigate_page" => match args["type"].as_str().unwrap_or("url") {
                "url" => match args["url"].as_str() {
                    Some(url) => {
                        self.load_in_browser(url);
                        self.open = Some(url.to_owned());
                        text(&format!(
                            "Successfully navigated to {url}.\n{}",
                            self.pages()
                        ))
                    }
                    None => failed("A url is needed to navigate."),
                },
                "reload" => text(&format!(
                    "Successfully reloaded the page.\n{}",
                    self.pages()
                )),
                _ => text(&format!("done\n{}", self.pages())),
            },
            "new_page" => {
                self.open = args["url"].as_str().map(str::to_owned);
                if let Some(url) = &self.open {
                    self.load_in_browser(url);
                }
                text(&self.pages())
            }
            "list_pages" | "select_page" => text(&self.pages()),
            "close_page" => {
                self.open = None;
                text(&self.pages())
            }
            "take_snapshot" => text(&self.snapshot()),
            "take_screenshot" => json!({
                "content": [
                    { "type": "text", "text": "Took a screenshot of the current page's viewport." },
                    { "type": "image", "data": PNG_1X1, "mimeType": "image/png" }
                ],
                "isError": false
            }),
            _ => text("done"),
        }
    }

    fn page(&self) -> Page {
        let url = self
            .open
            .clone()
            .unwrap_or_else(|| "about:blank".to_owned());
        self.script.page_for(&url).unwrap_or(Page {
            id: 1,
            title: String::new(),
            url,
        })
    }

    fn pages(&self) -> String {
        format!("## Pages\n{}", self.page().line())
    }

    fn snapshot(&self) -> String {
        if let Some(found) = self
            .open
            .as_deref()
            .and_then(|u| self.script.snapshot_for(u))
        {
            return found.to_owned();
        }
        "## Pages\n1: about:blank [selected]\n## Latest page snapshot\nuid=1_0 RootWebArea \"\""
            .to_owned()
    }

    fn log_call(&self, name: &str, args: &Value) {
        let Some(path) = &self.log else { return };
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{}", json!({ "tool": name, "args": args }));
        }
    }
}

fn text(t: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": t }], "isError": false })
}

fn failed(t: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": t }], "isError": true })
}

/// POSTs `body` (JSON) to `url` on 127.0.0.1 and returns the reply's body.
/// Plain HTTP/1.1 with no keep-alive: all the stand-in needs.
fn post_local(url: &str, body: &str) -> Option<String> {
    use std::io::{Read as _, Write as _};
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if !host.starts_with("127.0.0.1:") {
        return None;
    }
    let mut tcp = std::net::TcpStream::connect(host).ok()?;
    // Pacing and a page load can take a while; never forever.
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(90)))
        .ok()?;
    let path = if path.is_empty() { "/" } else { path };
    write!(
        tcp,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut reply = String::new();
    tcp.read_to_string(&mut reply).ok()?;
    reply.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devtools() -> Devtools {
        let script: Script = toml::from_str(
            r###"
[[reply]]
when = "search jobs"
tool = "mcp__chrome-devtools__click"
args = { uid = "2_4" }
snapshot = "## Pages\n1: Jobs (https://jobs.example.com/) [selected]\n## Latest page snapshot\nuid=2_1 RootWebArea \"Jobs\"\n  uid=2_4 button \"Search\""
"###,
        )
        .unwrap();
        Devtools::new(script, None)
    }

    fn call(d: &mut Devtools, name: &str, args: &Value) -> Value {
        let reply = d
            .handle(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}}))
            .unwrap();
        assert_eq!(reply["id"], 7);
        reply["result"].clone()
    }

    #[test]
    fn it_lists_the_loops_browser_tools_with_real_schemas() {
        let mut d = devtools();
        let init = d
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}))
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["version"], "1.10.1");
        assert!(
            d.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .is_none()
        );
        let list = d
            .handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 17);
        for n in [
            "navigate_page",
            "take_snapshot",
            "click",
            "fill",
            "upload_file",
        ] {
            assert!(names.contains(&n), "{n}");
        }
        assert!(!names.contains(&"evaluate_script"));
        let click = &list["result"]["tools"][names.iter().position(|n| *n == "click").unwrap()];
        assert_eq!(click["inputSchema"]["required"], json!(["pageId", "uid"]));
        let unknown = d
            .handle(&json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}))
            .unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn snapshots_follow_the_open_page() {
        let mut d = devtools();
        let blank = call(&mut d, "take_snapshot", &json!({"pageId":1}));
        assert!(
            blank["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("about:blank")
        );
        let nav = call(
            &mut d,
            "navigate_page",
            &json!({"pageId":1,"type":"url","url":"https://jobs.example.com/"}),
        );
        assert_eq!(
            nav["content"][0]["text"],
            "Successfully navigated to https://jobs.example.com/.\n## Pages\n1: Jobs (https://jobs.example.com/) [selected]"
        );
        let snap = call(&mut d, "take_snapshot", &json!({"pageId":1}));
        assert!(
            snap["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("uid=2_4 button \"Search\"")
        );
        let pages = call(&mut d, "list_pages", &json!({}));
        assert!(
            pages["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("[selected]")
        );
        assert_eq!(
            call(&mut d, "click", &json!({"pageId":1,"uid":"2_4"}))["content"][0]["text"],
            "done"
        );
        let shot = call(&mut d, "take_screenshot", &json!({"pageId":1}));
        assert_eq!(shot["content"][1]["mimeType"], "image/png");
    }

    #[test]
    fn bad_calls_are_tool_errors() {
        let mut d = devtools();
        let r = call(&mut d, "click", &json!({"uid":"2_4"}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("pageId"));
        let r = call(
            &mut d,
            "evaluate_script",
            &json!({"pageId":1,"function":"()=>1"}),
        );
        assert_eq!(r["isError"], true);
    }
}

