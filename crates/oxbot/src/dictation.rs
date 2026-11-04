//! Dictation with the Mac's own microphone and on-device speech
//! recognition, through oxbot's Mac helper (`oxbot-mac dictate`). The page
//! asks over its WebSocket and gets the text back on that connection only,
//! so it works the same in a browser and in the Mac app. The audio never
//! leaves the Mac: the helper requires on-device recognition, and only its
//! text comes back. One person dictates at a time: starting again stops
//! the last one.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxbot_protocol::ServerMsg;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

/// The longest one dictation runs (the helper stops itself at 120 s).
const LONGEST: Duration = Duration::from_secs(130);
/// How long the helper may take to finish once asked to stop.
const STOP_GRACE: Duration = Duration::from_secs(8);
/// How long the "can this Mac dictate?" check may take.
const CHECK_TIME: Duration = Duration::from_secs(15);

/// The Mac's dictation, for the whole server.
#[derive(Debug)]
pub struct Dictation {
    helper: Option<PathBuf>,
    /// The last check's answer: on-device dictation works here.
    available: AtomicBool,
    /// Why it doesn't, from the last check.
    why_not: Mutex<String>,
    /// The one running now: (its number, the connection, how to stop it).
    running: Mutex<Option<(u64, u64, oneshot::Sender<()>)>>,
    next: AtomicU64,
}

impl Dictation {
    /// Dictation through the helper at `helper` (none: never available).
    #[must_use]
    pub fn new(helper: Option<PathBuf>) -> Self {
        Self {
            helper,
            available: AtomicBool::new(false),
            why_not: Mutex::new("not checked yet".to_owned()),
            running: Mutex::new(None),
            next: AtomicU64::new(1),
        }
    }

    /// The helper, if this oxbot has one.
    #[must_use]
    pub fn helper(&self) -> Option<&PathBuf> {
        self.helper.as_ref()
    }

    /// Whether the last check found on-device dictation.
    #[must_use]
    pub fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    /// Why dictation isn't available, from the last check.
    #[must_use]
    pub fn why_not(&self) -> String {
        lock(&self.why_not).clone()
    }

    /// Asks the helper whether this Mac can dictate on-device now, and
    /// remembers it.
    pub async fn check(&self) -> bool {
        let (ok, why) = match &self.helper {
            None => (
                false,
                "this oxbot was built without its dictation helper".to_owned(),
            ),
            Some(helper) => ask_helper(helper).await,
        };
        if !ok {
            tracing::info!(why, "dictation isn't available");
        }
        *lock(&self.why_not) = why;
        self.available.store(ok, Ordering::SeqCst);
        ok
    }

    /// Starts dictating for connection `conn`, sending what's heard to
    /// `out`. Stops anyone else's first.
    pub fn start(self: &Arc<Self>, conn: u64, out: mpsc::Sender<ServerMsg>) {
        let done = |error: &str| ServerMsg::Dictation {
            text: String::new(),
            done: true,
            error: Some(error.to_owned()),
        };
        let Some(helper) = self.helper.clone().filter(|_| self.available()) else {
            let _ = out.try_send(done("Dictation isn't available on the home computer."));
            return;
        };
        let (stop, stopped) = oneshot::channel();
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        // Starting again stops the one before (dropping its stop sender).
        if let Some((_, _, before)) = lock(&self.running).replace((id, conn, stop)) {
            let _ = before.send(());
        }
        let child = Command::new(&helper)
            .arg("dictate")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        match child {
            Ok(child) => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    run(child, out, stopped).await;
                    me.finished(id);
                });
            }
            Err(err) => {
                tracing::warn!(%err, "couldn't start dictation");
                let _ = out.try_send(done("Dictation couldn't start on the home computer."));
                self.finished(id);
            }
        }
    }

    /// Stops connection `conn`'s dictation, if it has one running.
    pub fn stop(&self, conn: u64) {
        let mut running = lock(&self.running);
        if running.as_ref().is_some_and(|(_, c, _)| *c == conn)
            && let Some((_, _, stop)) = running.take()
        {
            let _ = stop.send(());
        }
    }

    fn finished(&self, id: u64) {
        let mut running = lock(&self.running);
        if running.as_ref().is_some_and(|(i, _, _)| *i == id) {
            running.take();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `oxbot-mac can-dictate`: (whether it can, why not).
async fn ask_helper(helper: &PathBuf) -> (bool, String) {
    let out = Command::new(helper)
        .arg("can-dictate")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(CHECK_TIME, out).await {
        Ok(Ok(out)) => {
            let reply: serde_json::Value =
                serde_json::from_slice(out.stdout.trim_ascii()).unwrap_or_default();
            let ok = reply["ok"].as_bool().unwrap_or(false);
            let why = reply["why"]
                .as_str()
                .unwrap_or(if ok {
                    ""
                } else {
                    "the dictation helper didn't say"
                })
                .to_owned();
            (ok, why)
        }
        Ok(Err(err)) => (false, format!("the dictation helper didn't start: {err}")),
        Err(_) => (false, "the dictation helper didn't answer".to_owned()),
    }
}

/// Relays one dictation's text until it ends; `stop` closes its input,
/// which tells the helper to finish.
async fn run(
    mut child: tokio::process::Child,
    out: mpsc::Sender<ServerMsg>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::sleep(LONGEST);
    tokio::pin!(deadline);
    let mut text = String::new();
    let mut stopping = false;
    let grace = tokio::time::sleep(Duration::MAX);
    tokio::pin!(grace);
    let ending = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    let msg: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                    if let Some(error) = msg["error"].as_str() {
                        break Some(error.to_owned());
                    }
                    if let Some(heard) = msg["text"].as_str() {
                        heard.clone_into(&mut text);
                        if msg["final"].as_bool() == Some(true) {
                            break None;
                        }
                        let _ = out
                            .send(ServerMsg::Dictation { text: text.clone(), done: false, error: None })
                            .await;
                    }
                }
                // It ended without a final line.
                _ => break None,
            },
            _ = &mut stop, if !stopping => {
                stopping = true;
                drop(stdin.take());
                grace.as_mut().reset(tokio::time::Instant::now() + STOP_GRACE);
            }
            () = &mut grace => break None,
            () = &mut deadline => break None,
        }
    };
    let _ = child.start_kill();
    let _ = out
        .send(ServerMsg::Dictation {
            text,
            done: true,
            error: ending,
        })
        .await;
}
