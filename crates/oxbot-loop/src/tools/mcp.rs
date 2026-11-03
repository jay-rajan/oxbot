//! The browser tool over MCP: `chrome-devtools-mcp` (or the e2e stand-in)
//! as a child process speaking JSON-RPC on stdio, through `rmcp`'s client.
//!
//! - Started on the first browser call of a session (or the first listing
//!   of its tools when none are cached), with a 30 s limit.
//! - Its environment is only what the command names, plus `PATH`, `HOME`
//!   and `TMPDIR`: never anything else of oxbot's (no key).
//! - Its tool list is cached per tool version and kept in the store, so
//!   later sessions don't start a child just to list tools.
//! - If it stops, the next call starts it again once.
//! - It stops with the session (and is killed when dropped).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use oxbot_engine::{BrowserSpec, McpCommand};
use oxbot_llm::ToolSpec;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::{RoleClient, RunningService, ServiceExt as _};
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use tokio::io::AsyncBufReadExt as _;

use super::browser::{BrowserFactory, BrowserOutput, BrowserTools};
use crate::store::LoopStore;

/// Longest wait for the tool to start.
const START_LIMIT: Duration = Duration::from_secs(30);
/// Longest wait for one call.
const CALL_LIMIT: Duration = Duration::from_mins(2);
/// Variables passed through from oxbot's own environment when the command
/// doesn't set them.
const PASSED: [&str; 3] = ["PATH", "HOME", "TMPDIR"];

/// The tool lists of each browser tool version, in memory and in the store.
struct SpecCache {
    key: String,
    store: Arc<dyn LoopStore>,
    specs: Mutex<Option<Vec<ToolSpec>>>,
}

impl std::fmt::Debug for SpecCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpecCache")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl SpecCache {
    fn get(&self) -> Option<Vec<ToolSpec>> {
        let mut specs = self.specs.lock().unwrap_or_else(PoisonError::into_inner);
        if specs.is_none() {
            *specs = self
                .store
                .load_pref(&self.key)
                .ok()
                .flatten()
                .and_then(|json| serde_json::from_str(&json).ok());
        }
        specs.clone()
    }

    fn set(&self, list: &[ToolSpec]) {
        *self.specs.lock().unwrap_or_else(PoisonError::into_inner) = Some(list.to_vec());
        match serde_json::to_string(list) {
            Ok(json) => {
                if let Err(err) = self.store.save_pref(&self.key, &json) {
                    tracing::warn!(%err, "couldn't keep the browser tool's list");
                }
            }
            Err(err) => tracing::warn!(%err, "couldn't keep the browser tool's list"),
        }
    }
}

/// Makes each session's MCP browser tool.
#[derive(Debug)]
pub struct McpBrowserFactory {
    cache: Arc<SpecCache>,
}

impl McpBrowserFactory {
    /// A factory caching tool lists in `store` under `cache_key` (name the
    /// tool's version in it, e.g. `devtools_tools:1.10.1`).
    #[must_use]
    pub fn new(store: Arc<dyn LoopStore>, cache_key: impl Into<String>) -> Self {
        Self {
            cache: Arc::new(SpecCache {
                key: cache_key.into(),
                store,
                specs: Mutex::new(None),
            }),
        }
    }
}

impl BrowserFactory for McpBrowserFactory {
    fn open(&self, spec: &BrowserSpec, env: &[(String, String)]) -> Arc<dyn BrowserTools> {
        Arc::new(McpBrowser {
            command: child_command(&spec.mcp, env),
            cache: Arc::clone(&self.cache),
            client: tokio::sync::Mutex::new(None),
        })
    }
}

/// The program, arguments and whole environment of the child.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildCommand {
    program: std::path::PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

/// `command`'s environment, plus `PATH`, `HOME` and `TMPDIR` from the
/// host's `env` list or oxbot's own environment when it lacks them.
fn child_command(command: &McpCommand, env: &[(String, String)]) -> ChildCommand {
    let mut all = command.env.clone();
    for name in PASSED {
        if all.iter().any(|(k, _)| k == name) {
            continue;
        }
        let value = env
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(name).ok());
        if let Some(value) = value {
            all.push((name.to_owned(), value));
        }
    }
    ChildCommand {
        program: command.program.clone(),
        args: command.args.clone(),
        env: all,
    }
}

type Client = RunningService<RoleClient, ()>;

/// One session's browser tool.
#[derive(Debug)]
pub struct McpBrowser {
    command: ChildCommand,
    cache: Arc<SpecCache>,
    client: tokio::sync::Mutex<Option<Client>>,
}

impl McpBrowser {
    async fn start(&self) -> Result<Client, String> {
        let c = &self.command;
        let mut cmd = tokio::process::Command::new(&c.program);
        cmd.args(&c.args).env_clear().envs(c.env.iter().cloned());
        let (transport, stderr) = TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't run {}: {e}", c.program.display()))?;
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "devtools_stderr", "{line}");
                }
            });
        }
        match tokio::time::timeout(START_LIMIT, ().serve(transport)).await {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("it didn't answer in time".to_owned()),
        }
    }

    /// The running client, started if needed.
    async fn client(&self) -> Result<tokio::sync::MutexGuard<'_, Option<Client>>, String> {
        let mut client = self.client.lock().await;
        if client.is_none() {
            *client = Some(self.start().await.map_err(|err| {
                format!("The browser tool couldn't start: {err}. Wait a few seconds and try again.")
            })?);
        }
        Ok(client)
    }
}

impl BrowserTools for McpBrowser {
    fn specs(&self) -> BoxFuture<'_, Result<Vec<ToolSpec>, String>> {
        Box::pin(async move {
            if let Some(specs) = self.cache.get() {
                return Ok(specs);
            }
            let guard = self.client().await?;
            let Some(client) = guard.as_ref() else {
                return Err("The browser tool isn't running.".to_owned());
            };
            let tools = client
                .list_all_tools()
                .await
                .map_err(|e| format!("The browser tool couldn't list its tools: {e}"))?;
            let specs: Vec<ToolSpec> = tools
                .into_iter()
                .map(|t| ToolSpec {
                    name: t.name.to_string(),
                    description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                    parameters: Value::Object((*t.input_schema).clone()),
                })
                .collect();
            self.cache.set(&specs);
            Ok(specs)
        })
    }

    fn call(&self, name: &str, args: Value) -> BoxFuture<'_, Result<BrowserOutput, String>> {
        let name = name.to_owned();
        Box::pin(async move {
            let mut guard = self.client().await?;
            let Some(client) = guard.as_ref() else {
                return Err("The browser tool isn't running.".to_owned());
            };
            let arguments = match args {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            let params = CallToolRequestParams::new(name).with_arguments(arguments);
            let result = tokio::time::timeout(CALL_LIMIT, client.call_tool(params)).await;
            match result {
                Ok(Ok(result)) => {
                    let mut texts = Vec::new();
                    let mut images = 0;
                    for block in &result.content {
                        match block {
                            ContentBlock::Text(t) => texts.push(t.text.as_str()),
                            ContentBlock::Image(_) => images += 1,
                            _ => {}
                        }
                    }
                    Ok(BrowserOutput {
                        text: texts.join("\n"),
                        images,
                        is_error: result.is_error == Some(true),
                    })
                }
                Ok(Err(err)) => {
                    if matches!(
                        err,
                        rmcp::ServiceError::TransportClosed | rmcp::ServiceError::TransportSend(_)
                    ) {
                        // It stopped: the next call starts it again.
                        *guard = None;
                        return Err(
                            "The browser tool stopped. Wait a few seconds and try again."
                                .to_owned(),
                        );
                    }
                    Err(format!("The browser tool failed: {err}"))
                }
                Err(_) => Err("The browser tool didn't answer in time.".to_owned()),
            }
        })
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let client = self.client.lock().await.take();
            if let Some(client) = client {
                let _ = client.cancel().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryLoopStore;
    use serde_json::json;

    /// A tiny MCP server in `sh`: answers initialize, tools/list (one
    /// tool, `take_snapshot`) and tools/call (echoing the tool name and
    /// whether `OXBOT_SECRET` reached it), one JSON line each.
    const SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"take_snapshot","description":"Snapshot.","inputSchema":{"type":"object","properties":{}}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      case "$line" in
        *crash*) exit 1 ;;
      esac
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"snap secret=%s home=%s"}]}}\n' "$id" "${OXBOT_SECRET:-none}" "${HOME:+yes}" ;;
  esac
done
"#;

    fn spec() -> BrowserSpec {
        BrowserSpec {
            mcp: McpCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), SERVER.into()],
                env: Vec::new(),
            },
            mcp_json: json!({}),
        }
    }

    #[tokio::test]
    async fn lists_calls_and_restarts() {
        let store = Arc::new(MemoryLoopStore::new());
        let factory = McpBrowserFactory::new(store.clone(), "devtools_tools:test");
        let host_env = [
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("OXBOT_SECRET".into(), "leak".into()),
        ];
        let browser = factory.open(&spec(), &host_env);
        let specs = browser.specs().await.unwrap();
        assert_eq!(specs[0].name, "take_snapshot");
        assert!(store.load_pref("devtools_tools:test").unwrap().is_some());
        let out = browser
            .call("take_snapshot", json!({"secret": "x"}))
            .await
            .unwrap();
        // Nothing of oxbot's environment but PATH, HOME and TMPDIR.
        assert!(
            out.text.starts_with("snap secret=none home="),
            "{}",
            out.text
        );
        let err = browser.call("crash", json!({})).await.unwrap_err();
        assert!(err.contains("stopped"), "{err}");
        // Started again.
        let out = browser.call("take_snapshot", json!({})).await.unwrap();
        assert!(out.text.starts_with("snap"));
        browser.shutdown().await;

        // A later session gets the list without starting anything.
        let factory = McpBrowserFactory::new(store, "devtools_tools:test");
        let broken = BrowserSpec {
            mcp: McpCommand {
                program: "/nonexistent/tool".into(),
                ..McpCommand::default()
            },
            mcp_json: json!({}),
        };
        let browser = factory.open(&broken, &[]);
        assert_eq!(browser.specs().await.unwrap()[0].name, "take_snapshot");
        let err = browser.call("take_snapshot", json!({})).await.unwrap_err();
        assert!(err.starts_with("The browser tool couldn't start"), "{err}");
    }

    #[test]
    fn the_child_gets_only_its_own_environment() {
        let command = McpCommand {
            program: "/x".into(),
            args: Vec::new(),
            env: vec![("A".into(), "1".into()), ("PATH".into(), "/mine".into())],
        };
        let child = child_command(&command, &[("PATH".into(), "/host".into())]);
        assert_eq!(child.env[0], ("A".into(), "1".into()));
        assert_eq!(child.env[1], ("PATH".into(), "/mine".into()));
        assert!(
            child
                .env
                .iter()
                .all(|(k, _)| PASSED.contains(&k.as_str()) || k == "A")
        );
    }
}
