//! Starts oxbot in test mode on a seeded home.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::process::{Child, Command};

use crate::spec::Config;

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
}

/// A free local port.
///
/// # Errors
/// Fails if no port can be had.
pub fn free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

impl Server {
    /// Starts `oxbot serve` on `home` with the stand-in Claude reading
    /// `script`, and waits until it answers.
    ///
    /// # Errors
    /// Fails if it doesn't start within 20 seconds.
    pub async fn start(
        repo: &Path,
        config: &Config,
        home: &Path,
        script: &Path,
    ) -> anyhow::Result<Self> {
        let port = free_port()?;
        let log = home.join("oxbot.log");
        let file = std::fs::File::create(&log)?;
        let mut cmd = Command::new(repo.join(&config.server));
        cmd.args(["serve", "--port", &port.to_string()])
            .env("OXBOT_HOME", home)
            .env("OXBOT_E2E", "1")
            .env("OXBOT_PERSON", &config.person)
            .env("OXBOT_CLAUDE", repo.join(&config.fake_claude))
            .env("OXBOT_FAKE_SCRIPT", script)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .kill_on_drop(true);
        if let Some(tools) = &config.tools {
            let tools = repo.join(tools);
            if tools.exists() {
                cmd.env("OXBOT_BUILT_IN_TOOLS", tools);
            }
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("starting {}", config.server.display()))?;
        let server = Self {
            child,
            port,
            home: home.to_owned(),
            log,
        };
        for _ in 0..100 {
            if let Ok((200, _)) = crate::http::request(port, "GET", "/", None).await {
                return Ok(server);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("oxbot didn't start; see {}", server.log.display())
    }

    /// The page's address, e.g. `http://127.0.0.1:5123/?as=Ann`.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
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
