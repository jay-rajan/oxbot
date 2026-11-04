//! One `codex app-server --listen stdio://` process and its JSON-RPC:
//! newline-delimited JSON on stdin and stdout, without the `"jsonrpc"`
//! field (as Codex's generated schema has it). oxbot's requests get
//! numbered ids and wait for their answer; what Codex sends on its own
//! (notifications, and its requests such as `item/tool/call`) goes to the
//! thread it names. Codex's stderr goes to oxbot's log at debug level; the
//! relay token is only in the process's environment, never on its command
//! line or in a file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::config::TOKEN_ENV;

/// Longest wait for Codex to answer one of oxbot's requests.
const ANSWER_LIMIT: Duration = Duration::from_secs(60);
/// Longest line read from Codex (a resumed thread can be long).
const MAX_LINE: usize = 64 * 1024 * 1024;
/// Notifications oxbot doesn't read, which Codex then doesn't send.
const NOT_READ: [&str; 8] = [
    "item/reasoning/textDelta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "account/rateLimits/updated",
    "remoteControl/status/changed",
    "thread/status/changed",
];

/// Something Codex sent about a thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A notification.
    Note {
        /// Its method, e.g. `item/agentMessage/delta`.
        method: String,
        /// Its params.
        params: Value,
    },
    /// A request oxbot must answer ([`Process::respond`]).
    Request {
        /// Its id, to answer with.
        id: Value,
        /// Its method, e.g. `item/tool/call`.
        method: String,
        /// Its params.
        params: Value,
    },
    /// The process stopped.
    Gone,
}

/// Why a request to Codex failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    /// Codex answered with an error.
    #[error("The AI helper said: {0}")]
    Codex(String),
    /// The process stopped, or didn't answer in time.
    #[error("{0}")]
    Gone(String),
}

/// How to start a process.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The native `codex` program.
    pub program: PathBuf,
    /// Its own CODEX_HOME (also its HOME, so nothing of the person's is
    /// read).
    pub home: PathBuf,
    /// The relay token for its environment.
    pub token: Zeroizing<String>,
    /// A proxy that refuses every connection (and logs it), so anything
    /// but the relay on 127.0.0.1 fails at once.
    pub refuse_proxy: String,
}

type Waiting = Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>;

/// A running app-server.
pub struct Process {
    stdin: tokio::sync::Mutex<ChildStdin>,
    child: Mutex<Option<Child>>,
    waiting: Arc<Waiting>,
    threads: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Incoming>>>>,
    next_id: AtomicI64,
    gone: CancellationToken,
}

impl std::fmt::Debug for Process {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Process")
            .field("gone", &self.gone.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// The whole environment of a Codex process: nothing of oxbot's own.
fn environment(launch: &Launch) -> Vec<(String, String)> {
    let home = launch.home.display().to_string();
    let mut env = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("HOME".to_owned(), home.clone()),
        ("CODEX_HOME".to_owned(), home.clone()),
        ("TMPDIR".to_owned(), format!("{home}/tmp")),
        ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
        ("RUST_LOG".to_owned(), "warn".to_owned()),
        ("NO_PROXY".to_owned(), "127.0.0.1,localhost".to_owned()),
        ("no_proxy".to_owned(), "127.0.0.1,localhost".to_owned()),
    ];
    for name in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ] {
        env.push((name.to_owned(), launch.refuse_proxy.clone()));
    }
    env
}

impl Process {
    /// Starts the app-server and says hello (`initialize`, `initialized`).
    ///
    /// # Errors
    /// It couldn't start, or didn't answer.
    pub async fn start(launch: &Launch, bot: &str) -> Result<Arc<Self>, RpcError> {
        std::fs::create_dir_all(launch.home.join("tmp"))
            .map_err(|e| RpcError::Gone(format!("couldn't make the AI helper's folder: {e}")))?;
        let mut cmd = Command::new(&launch.program);
        cmd.args(["app-server", "--listen", "stdio://"])
            .env_clear()
            .envs(environment(launch))
            .env(TOKEN_ENV, launch.token.as_str())
            .current_dir(&launch.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::Gone(format!("couldn't start the AI helper: {e}")))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(RpcError::Gone(
                "The AI helper started without its pipes".to_owned(),
            ));
        };
        let process = Arc::new(Self {
            stdin: tokio::sync::Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            waiting: Arc::new(Mutex::new(HashMap::new())),
            threads: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicI64::new(1),
            gone: CancellationToken::new(),
        });
        let bot_log = bot.to_owned();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(bot = %bot_log, "codex: {line}");
            }
        });
        tokio::spawn(Arc::clone(&process).read(stdout, bot.to_owned()));
        let hello = json!({
            "clientInfo": {"name": "oxbot", "title": "oxbot", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {
                "experimentalApi": true,
                "requestAttestation": false,
                "optOutNotificationMethods": NOT_READ,
            }
        });
        process.request("initialize", hello).await?;
        process.notify("initialized", Value::Null).await?;
        Ok(process)
    }

    /// Whether the process has stopped.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        self.gone.is_cancelled()
    }

    /// Resolves when the process stops.
    pub async fn gone(&self) {
        self.gone.cancelled().await;
    }

    async fn read(self: Arc<Self>, stdout: tokio::process::ChildStdout, bot: String) {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match read_line(&mut reader, &mut line).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(why) => {
                    tracing::warn!(%bot, why, "stopped reading Codex");
                    break;
                }
            }
            let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
                tracing::debug!(%bot, "Codex sent a line that isn't JSON");
                continue;
            };
            self.route(msg, &bot).await;
        }
        self.gone.cancel();
        for (_, tx) in lock(&self.waiting).drain() {
            let _ = tx.send(Err(RpcError::Gone("The AI helper stopped.".to_owned())));
        }
        for (_, tx) in lock(&self.threads).drain() {
            let _ = tx.send(Incoming::Gone);
        }
    }

    async fn route(&self, msg: Value, bot: &str) {
        let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
        let id = msg.get("id").cloned();
        match (method, id) {
            // An answer to one of oxbot's requests.
            (None, Some(id)) => {
                let Some(n) = id.as_i64() else { return };
                let Some(tx) = lock(&self.waiting).remove(&n) else {
                    return;
                };
                let answer = match msg.get("error") {
                    Some(e) => Err(RpcError::Codex(
                        e.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("error")
                            .to_owned(),
                    )),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(answer);
            }
            (Some(method), id) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let incoming = match &id {
                    Some(id) => Incoming::Request {
                        id: id.clone(),
                        method: method.clone(),
                        params,
                    },
                    None => Incoming::Note {
                        method: method.clone(),
                        params,
                    },
                };
                let sent = thread.and_then(|t| {
                    lock(&self.threads)
                        .get(&t)
                        .map(|tx| tx.send(incoming.clone()).is_ok())
                });
                if sent != Some(true) {
                    self.unrouted(incoming, bot).await;
                }
            }
            (None, None) => {}
        }
    }

    /// What no thread takes: requests are refused, warnings logged.
    async fn unrouted(&self, incoming: Incoming, bot: &str) {
        match incoming {
            Incoming::Request { id, method, .. } => {
                tracing::warn!(%bot, method, "refused a request from Codex no thread takes");
                self.respond_error(&id, "oxbot doesn't do that").await;
            }
            Incoming::Note { method, params }
                if matches!(method.as_str(), "configWarning" | "deprecationNotice") =>
            {
                tracing::warn!(%bot, method, %params, "Codex warned");
            }
            Incoming::Note { method, .. } => {
                tracing::debug!(%bot, method, "a Codex notification for no thread");
            }
            Incoming::Gone => {}
        }
    }

    /// Sends what Codex sends about `thread` to `tx` from now on.
    pub fn watch(&self, thread: &str, tx: mpsc::UnboundedSender<Incoming>) {
        lock(&self.threads).insert(thread.to_owned(), tx);
        if self.is_gone() {
            let _ = lock(&self.threads)
                .remove(thread)
                .map(|tx| tx.send(Incoming::Gone));
        }
    }

    /// Stops sending what Codex sends about `thread`.
    pub fn unwatch(&self, thread: &str) {
        lock(&self.threads).remove(thread);
    }

    async fn write(&self, msg: &Value) -> Result<(), RpcError> {
        let mut line = serde_json::to_vec(msg).map_err(|e| RpcError::Gone(e.to_string()))?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&line)
            .await
            .map_err(|_| RpcError::Gone("The AI helper stopped.".to_owned()))?;
        stdin
            .flush()
            .await
            .map_err(|_| RpcError::Gone("The AI helper stopped.".to_owned()))
    }

    /// Sends a request and waits for its answer.
    ///
    /// # Errors
    /// Codex refused it, stopped, or took too long.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.waiting).insert(id, tx);
        if self.is_gone() {
            lock(&self.waiting).remove(&id);
            return Err(RpcError::Gone("The AI helper stopped.".to_owned()));
        }
        let sent = self
            .write(&json!({"id": id, "method": method, "params": params}))
            .await;
        if let Err(err) = sent {
            lock(&self.waiting).remove(&id);
            return Err(err);
        }
        match tokio::time::timeout(ANSWER_LIMIT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(RpcError::Gone("The AI helper stopped.".to_owned())),
            Err(_) => {
                lock(&self.waiting).remove(&id);
                Err(RpcError::Gone(format!(
                    "The AI helper didn't answer {method} in time."
                )))
            }
        }
    }

    /// Sends a notification.
    ///
    /// # Errors
    /// The process stopped.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        let msg = if params.is_null() {
            json!({"method": method})
        } else {
            json!({"method": method, "params": params})
        };
        self.write(&msg).await
    }

    /// Answers one of Codex's requests.
    pub async fn respond(&self, id: &Value, result: Value) {
        let _ = self.write(&json!({"id": id, "result": result})).await;
    }

    /// Refuses one of Codex's requests.
    pub async fn respond_error(&self, id: &Value, message: &str) {
        let _ = self
            .write(&json!({"id": id, "error": {"code": -32601, "message": message}}))
            .await;
    }

    /// Stops the process: a moment to exit on its own, then killed.
    pub async fn stop(&self) {
        let child = lock(&self.child).take();
        let Some(mut child) = child else { return };
        {
            let mut stdin = self.stdin.lock().await;
            let _ = stdin.shutdown().await;
        }
        if tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        self.gone.cancel();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.start_kill();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads one line (without its newline) into `out`. `false` at the end.
async fn read_line(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    out: &mut Vec<u8>,
) -> Result<bool, &'static str> {
    loop {
        let buf = reader.fill_buf().await.map_err(|_| "read error")?;
        if buf.is_empty() {
            return Ok(!out.is_empty());
        }
        if let Some(at) = buf.iter().position(|b| *b == b'\n') {
            out.extend_from_slice(&buf[..at]);
            reader.consume(at + 1);
            return Ok(true);
        }
        let n = buf.len();
        out.extend_from_slice(buf);
        reader.consume(n);
        if out.len() > MAX_LINE {
            return Err("a line from the AI helper was too long");
        }
    }
}

/// The folder a bot's Codex process keeps its home in.
#[must_use]
pub fn home_in(dir: &Path) -> PathBuf {
    dir.join("codex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_has_only_what_codex_needs() {
        let launch = Launch {
            program: PathBuf::from("/x/codex"),
            home: PathBuf::from("/data/bots/scout/codex"),
            token: Zeroizing::new("t".repeat(64)),
            refuse_proxy: "http://127.0.0.1:9".to_owned(),
        };
        let env = environment(&launch);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("HOME"), Some("/data/bots/scout/codex"));
        assert_eq!(get("CODEX_HOME"), Some("/data/bots/scout/codex"));
        assert_eq!(get("HTTPS_PROXY"), Some("http://127.0.0.1:9"));
        assert_eq!(get("ALL_PROXY"), Some("http://127.0.0.1:9"));
        assert_eq!(get("NO_PROXY"), Some("127.0.0.1,localhost"));
        // The token goes in separately, and nothing else of oxbot's.
        assert!(env.iter().all(|(_, v)| !v.contains("tttt")));
        assert!(get("OPENROUTER_API_KEY").is_none());
        assert_eq!(env.len(), 14);
    }
}
