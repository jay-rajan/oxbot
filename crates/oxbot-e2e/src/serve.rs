//! Starts oxbot in test mode on a seeded home, with the stand-ins.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use tokio::io::AsyncBufReadExt as _;
use tokio::process::{Child, Command};

use crate::spec::Config;

/// The stand-in `OpenRouter` for one test, in its own folder (never
/// oxbot's): its canary key, request log, and the stand-in browser tool's
/// log. Stops when dropped.
#[derive(Debug)]
pub struct FakeOpenRouter {
    _child: Child,
    /// Its port on 127.0.0.1.
    pub port: u16,
    /// The key it hands out (a canary: it must never be written by oxbot).
    pub canary: String,
    dir: tempfile::TempDir,
}

impl FakeOpenRouter {
    /// Starts `program` answering from `script`, with `daily` free
    /// requests a day (default 1000).
    ///
    /// # Errors
    /// Fails if it doesn't start within 10 seconds.
    pub async fn start(program: &Path, script: &Path, daily: Option<u32>) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let mut cmd = Command::new(program);
        cmd.arg("--dir")
            .arg(dir.path())
            .env("OXBOT_FAKE_SCRIPT", script)
            .env("OXBOT_FAKE_OR_LOG", dir.path().join("requests.jsonl"))
            .env("OXBOT_FAKE_OR_BODIES", dir.path().join("bodies.jsonl"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(dir.path().join("stderr.txt"))?)
            .kill_on_drop(true);
        if let Some(daily) = daily {
            cmd.env("OXBOT_FAKE_OR_DAILY", daily.to_string());
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}; build it first", program.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no output from the stand-in OpenRouter"))?;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .context("the stand-in OpenRouter didn't start")??
            .ok_or_else(|| anyhow!("the stand-in OpenRouter quit"))?;
        let port = serde_json::from_str::<serde_json::Value>(&first)?["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .ok_or_else(|| anyhow!("the stand-in OpenRouter said {first}"))?;
        let canary = std::fs::read_to_string(dir.path().join("canary"))?;
        Ok(Self {
            _child: child,
            port,
            canary,
            dir,
        })
    }

    /// Its API's address, as oxbot's `OXBOT_OPENROUTER_URL`.
    #[must_use]
    pub fn api_url(&self) -> String {
        format!("http://127.0.0.1:{}/api/v1", self.port)
    }

    /// The file holding its key, for oxbot to start signed in.
    #[must_use]
    pub fn key_file(&self) -> PathBuf {
        self.dir.path().join("canary")
    }

    /// Where the stand-in browser tool logs its calls.
    #[must_use]
    pub fn devtools_log(&self) -> PathBuf {
        self.dir.path().join("devtools.jsonl")
    }

    /// Its request log so far (JSON lines).
    #[must_use]
    pub fn requests(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("requests.jsonl")).unwrap_or_default()
    }

    /// The whole request bodies so far (JSON lines).
    #[must_use]
    pub fn bodies(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("bodies.jsonl")).unwrap_or_default()
    }

    /// The stand-in browser tool's calls so far (JSON lines).
    #[must_use]
    pub fn devtools_calls(&self) -> String {
        std::fs::read_to_string(self.devtools_log()).unwrap_or_default()
    }
}

/// How to start oxbot for one test.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The stand-in Claude's script.
    pub script: PathBuf,
    /// The stand-in `OpenRouter`'s script.
    pub openrouter_script: PathBuf,
    /// oxbot starts signed in to the stand-in `OpenRouter`.
    pub signed_in: bool,
    /// The stand-in `OpenRouter`'s free requests per day.
    pub openrouter_daily: Option<u32>,
    /// The stand-in Mac helper can dictate.
    pub dictation: bool,
}

/// A running oxbot under test. Stops it when dropped.
#[derive(Debug)]
pub struct Server {
    child: Child,
    /// Its port on 127.0.0.1.
    pub port: u16,
    /// Its data folder.
    pub home: PathBuf,
    /// Its log file.
    pub log: PathBuf,
    /// The repo it runs from (test files name fixtures from there).
    pub repo: PathBuf,
    /// The stand-in `OpenRouter`, if the repo has one.
    pub openrouter: Option<FakeOpenRouter>,
    /// How it was started (for a restart).
    config: Config,
    launch: Launch,
}

/// A free local port.
///
/// # Errors
/// Fails if no port can be had.
pub fn free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Points oxbot (test mode only) at the stand-in `OpenRouter` and browser
/// tool. Signed in means oxbot reads the stand-in's key from its file at
/// start (into its in-memory key store): the key itself is never in an
/// environment variable, which every child process would inherit.
pub fn openrouter_env(
    cmd: &mut Command,
    repo: &Path,
    config: &Config,
    fake: &FakeOpenRouter,
    signed_in: bool,
) {
    cmd.env("OXBOT_OPENROUTER_URL", fake.api_url());
    if let Some(devtools) = &config.fake_devtools {
        cmd.env("OXBOT_FAKE_DEVTOOLS", repo.join(devtools))
            .env("OXBOT_FAKE_DT_LOG", fake.devtools_log());
    }
    if signed_in {
        cmd.env("OXBOT_E2E_OPENROUTER_KEY_FILE", fake.key_file());
    }
}

impl Server {
    /// Starts `oxbot serve` on `home` with the stand-ins (Claude,
    /// `OpenRouter`, the browser tool, the Mac helper), and waits until it
    /// answers.
    ///
    /// # Errors
    /// Fails if a stand-in or oxbot doesn't start within 20 seconds.
    pub async fn start(
        repo: &Path,
        config: &Config,
        home: &Path,
        launch: &Launch,
    ) -> anyhow::Result<Self> {
        let openrouter = match &config.fake_openrouter {
            Some(program) => Some(
                FakeOpenRouter::start(
                    &repo.join(program),
                    &launch.openrouter_script,
                    launch.openrouter_daily,
                )
                .await?,
            ),
            None => None,
        };
        let port = free_port()?;
        let log = home.join("oxbot.log");
        let child = spawn_oxbot(repo, config, home, launch, port, openrouter.as_ref(), &log)?;
        let server = Self {
            child,
            port,
            home: home.to_owned(),
            log,
            repo: repo.to_owned(),
            openrouter,
            config: config.clone(),
            launch: launch.clone(),
        };
        server.wait_until_up().await?;
        Ok(server)
    }

    /// Waits until oxbot answers on its port (20 seconds at most).
    async fn wait_until_up(&self) -> anyhow::Result<()> {
        for _ in 0..100 {
            if let Ok((200, _)) = crate::http::request(self.port, "GET", "/", None).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("oxbot didn't start; see {}", self.log.display())
    }

    /// Stops oxbot and starts it again on the same home and port, as after
    /// the Mac restarts; the stand-ins keep running (same key, same
    /// counts). Its log goes on in the same file.
    ///
    /// # Errors
    /// Fails if it doesn't start again within 20 seconds.
    pub async fn restart(&mut self) -> anyhow::Result<()> {
        self.stop_oxbot().await;
        self.child = spawn_oxbot(
            &self.repo,
            &self.config,
            &self.home,
            &self.launch,
            self.port,
            self.openrouter.as_ref(),
            &self.log,
        )?;
        self.wait_until_up().await
    }

    /// The page's address, e.g. `http://127.0.0.1:5123/?as=Ann`. The
    /// Farm's own page (no `as=`) carries the Farm secret, as the app's
    /// window does.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        let base = format!("http://127.0.0.1:{}{path}", self.port);
        if path.contains("as=") {
            return base;
        }
        let key = std::fs::read_to_string(self.home.join("farm-key")).unwrap_or_default();
        format!("{base}#k={}", key.trim())
    }

    /// The last lines of its log.
    #[must_use]
    pub fn log_tail(&self, lines: usize) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }

    /// Stops oxbot and the Chromes it started.
    pub async fn stop(mut self) {
        self.stop_oxbot().await;
    }

    async fn stop_oxbot(&mut self) {
        // A polite stop lets oxbot quit its Chromes.
        if let Some(pid) = self.child.id() {
            let _ = std::process::Command::new("kill")
                .args(["-INT", &pid.to_string()])
                .status();
        }
        if tokio::time::timeout(Duration::from_secs(8), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        // Anything left using this home's Chrome profiles.
        let pattern = format!("user-data-dir={}", self.home.display());
        let _ = std::process::Command::new("pkill")
            .args(["-f", &pattern])
            .status();
    }
}

/// Starts `oxbot serve` on `home` and `port` with the stand-ins, logging
/// to `log` (appended to, so a restart keeps the earlier lines).
fn spawn_oxbot(
    repo: &Path,
    config: &Config,
    home: &Path,
    launch: &Launch,
    port: u16,
    openrouter: Option<&FakeOpenRouter>,
    log: &Path,
) -> anyhow::Result<Child> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let mut cmd = Command::new(repo.join(&config.server));
    cmd.args(["serve", "--port", &port.to_string()])
        .env("OXBOT_HOME", home)
        .env("OXBOT_E2E", "1")
        // Any free port for the Gate door, so tests never clash with a
        // real oxbot (or each other) on 7879.
        .env("OXBOT_GATE_PORT", "0")
        .env("OXBOT_PERSON", &config.person)
        .env("OXBOT_CLAUDE", repo.join(&config.fake_claude))
        .env("OXBOT_FAKE_SCRIPT", &launch.script)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .kill_on_drop(true);
    // Never the Mac's own microphone in tests: a stand-in helper, which
    // passes video to the real one (if built) for real still frames.
    match &config.fake_mac {
        Some(fake) => {
            cmd.env("OXBOT_MAC_HELPER", repo.join(fake)).env(
                "OXBOT_FAKE_DICTATION",
                if launch.dictation { "1" } else { "0" },
            );
        }
        None => {
            cmd.env("OXBOT_MAC_HELPER", "off");
        }
    }
    if let Some(real) = config.mac_helper.as_ref().map(|h| repo.join(h)) {
        cmd.env("OXBOT_REAL_MAC_HELPER", real);
    }
    if let Some(tools) = &config.tools {
        let tools = repo.join(tools);
        if tools.exists() {
            cmd.env("OXBOT_BUILT_IN_TOOLS", tools);
        }
    }
    if let Some(fake) = openrouter {
        openrouter_env(&mut cmd, repo, config, fake, launch.signed_in);
    }
    cmd.spawn()
        .with_context(|| format!("starting {}", config.server.display()))
}
