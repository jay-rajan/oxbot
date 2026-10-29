//! Chrome for oxbot's bots: one shared, hidden Chrome, driven by Claude
//! through Google's `chrome-devtools-mcp`.
//!
//! - [`Tools`]: the Chrome program and the installed browser tool.
//! - [`Browser`]: the Chrome every bot uses, on one control port, with a
//!   kept profile: the user signs in to a site once (in a visible window)
//!   and every bot stays signed in. Bots take turns on it (see `oxbot-core`).
//!
//! The control port listens on `127.0.0.1` only.

mod browser;
mod guard;
mod http;
mod web;

use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::process::Command;
use tokio::sync::Mutex;

pub use browser::{Browser, BrowserSpec, BrowserStatus, site_of};
pub use web::web_host;

/// npm package for the browser tool.
pub const MCP_PACKAGE: &str = "chrome-devtools-mcp";
/// Pinned version: older than two weeks when chosen (released 2026-09-23).
pub const MCP_VERSION: &str = "1.10.1";
/// Name Claude sees the tool under; its actions are `mcp__chrome-devtools__*`.
pub const MCP_SERVER_NAME: &str = "chrome-devtools";
/// Chrome on macOS.
pub const DEFAULT_CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/// Environment for the `chrome-devtools-mcp` process: no update checks or
/// usage statistics.
pub const MCP_ENV: &[(&str, &str)] = &[
    ("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1"),
    ("CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS", "1"),
];

/// Arguments for `chrome-devtools-mcp` that connect it to a bot's Chrome at
/// `browser_url` (e.g. `http://127.0.0.1:9333`):
///
/// - files it saves (screenshots, snapshots, network dumps) may only go in
///   `workspace`, the bot's folder;
/// - Google's usage statistics and performance lookups are off;
/// - request headers (cookies, tokens) are redacted before the bot sees them;
/// - tool groups bots don't need stay off (extensions and apps are off by
///   default and stay that way).
#[must_use]
pub fn mcp_args(browser_url: &str, workspace: &Path) -> Vec<String> {
    vec![
        format!("--browserUrl={browser_url}"),
        format!("--workspace={}", workspace.display()),
        "--usageStatistics=false".to_owned(),
        "--performanceCrux=false".to_owned(),
        "--redactNetworkHeaders=true".to_owned(),
        "--categoryPerformance=false".to_owned(),
        "--categoryEmulation=false".to_owned(),
        "--categoryMemory=false".to_owned(),
        "--categoryExtensions=false".to_owned(),
    ]
}

/// Errors from the browser.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// Chrome isn't installed where expected.
    #[error("Chrome isn't installed at {0}")]
    ChromeMissing(PathBuf),
    /// The profile folder couldn't be deleted.
    #[error("couldn't delete {path}: {source}")]
    Remove {
        /// The folder.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// Chrome or npm couldn't be started.
    #[error("couldn't start {program}: {source}")]
    Spawn {
        /// The program.
        program: String,
        /// Why.
        source: std::io::Error,
    },
    /// Chrome started but never opened its control port.
    #[error("Chrome didn't start in time")]
    NotReady,
    /// Installing the browser tool failed.
    #[error("couldn't install {MCP_PACKAGE}@{MCP_VERSION}: {0}")]
    Install(String),
    /// Chrome refused or didn't answer a `DevTools` call.
    #[error("Chrome: {0}")]
    Chrome(String),
}

/// Where the browser's programs and profile live.
#[derive(Debug, Clone)]
pub struct ToolsConfig {
    /// The Chrome executable.
    pub chrome: PathBuf,
    /// Where `chrome-devtools-mcp` is installed.
    pub tools_dir: PathBuf,
    /// The `npm` program, used once to install the tool.
    pub npm: PathBuf,
    /// The shared browser's kept profile.
    pub profile_dir: PathBuf,
    /// Where the throwaway profiles of older oxbot versions were; deleted
    /// if found.
    pub scratch_dir: PathBuf,
    /// Extra environment for npm and the browser tool, e.g. a `PATH` that
    /// finds Node.js when oxbot was opened from Finder.
    pub env: Vec<(String, String)>,
    /// The browser tool and Node.js shipped inside the app. When set, oxbot
    /// uses them and never runs npm.
    pub built_in: Option<BuiltInTool>,
}

/// The browser tool shipped inside oxbot.app: a Node.js runtime and the
/// pinned `chrome-devtools-mcp`, installed when the app was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInTool {
    /// The `node` program.
    pub node: PathBuf,
    /// `chrome-devtools-mcp`'s start script.
    pub entry: PathBuf,
}

impl BuiltInTool {
    /// The tool inside `dir` (the app's `Resources/tools`), if both parts
    /// are there: `node/bin/node` and
    /// `mcp/node_modules/chrome-devtools-mcp/build/src/bin/chrome-devtools-mcp.js`.
    #[must_use]
    pub fn find_in(dir: &Path) -> Option<Self> {
        let tool = Self {
            node: dir.join("node/bin/node"),
            entry: dir
                .join("mcp/node_modules")
                .join(MCP_PACKAGE)
                .join("build/src/bin/chrome-devtools-mcp.js"),
        };
        (tool.node.is_file() && tool.entry.is_file()).then_some(tool)
    }
}

impl ToolsConfig {
    /// Defaults under oxbot's data folder: `tools/` and `browser/profile`.
    #[must_use]
    pub fn in_home(root: &Path) -> Self {
        Self {
            chrome: PathBuf::from(DEFAULT_CHROME),
            tools_dir: root.join("tools"),
            npm: PathBuf::from("npm"),
            profile_dir: root.join("browser").join("profile"),
            scratch_dir: root.join("scratch"),
            env: Vec::new(),
            built_in: None,
        }
    }

    /// The installed `chrome-devtools-mcp` program.
    #[must_use]
    pub fn mcp_program(&self) -> PathBuf {
        self.tools_dir.join("node_modules/.bin").join(MCP_PACKAGE)
    }
}

/// The Chrome program and the browser tool. Share it as `Arc<Tools>`.
#[derive(Debug)]
pub struct Tools {
    config: ToolsConfig,
    /// Held while installing, so two bots don't run npm at once.
    installing: Mutex<()>,
}

impl Tools {
    /// Tools from `config`. Deletes throwaway profiles left behind by older
    /// oxbot versions.
    #[must_use]
    pub fn new(config: ToolsConfig) -> std::sync::Arc<Self> {
        if config.scratch_dir.exists() {
            match std::fs::remove_dir_all(&config.scratch_dir) {
                Ok(()) => tracing::info!("deleted leftover throwaway browser profiles"),
                Err(err) => tracing::warn!(%err, "couldn't delete leftover throwaway profiles"),
            }
        }
        std::sync::Arc::new(Self {
            config,
            installing: Mutex::new(()),
        })
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &ToolsConfig {
        &self.config
    }

    /// Whether the pinned browser tool is already installed.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.config.built_in.is_some() || self.config.mcp_program().exists()
    }

    /// Installs the pinned `chrome-devtools-mcp` into the tools folder if it
    /// isn't there yet, and returns its path.
    ///
    /// # Errors
    /// Fails if npm is missing or the install fails.
    pub async fn ensure_installed(&self) -> Result<PathBuf, BrowserError> {
        if let Some(tool) = &self.config.built_in {
            return Ok(tool.entry.clone());
        }
        let _one_at_a_time = self.installing.lock().await;
        let program = self.config.mcp_program();
        let package_json = self
            .config
            .tools_dir
            .join("node_modules")
            .join(MCP_PACKAGE)
            .join("package.json");
        let installed = tokio::fs::read_to_string(&package_json)
            .await
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|v| {
                v.get("version")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        if installed.as_deref() == Some(MCP_VERSION) && program.exists() {
            return Ok(program);
        }
        tracing::info!("installing {MCP_PACKAGE}@{MCP_VERSION}");
        tokio::fs::create_dir_all(&self.config.tools_dir)
            .await
            .map_err(|e| BrowserError::Install(e.to_string()))?;
        let output = Command::new(&self.config.npm)
            .arg("install")
            .arg("--prefix")
            .arg(&self.config.tools_dir)
            .arg(format!("{MCP_PACKAGE}@{MCP_VERSION}"))
            .args(["--no-audit", "--no-fund", "--loglevel=error"])
            .envs(
                self.config
                    .env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
            )
            .output()
            .await
            .map_err(|source| BrowserError::Spawn {
                program: self.config.npm.display().to_string(),
                source,
            })?;
        if !output.status.success() {
            return Err(BrowserError::Install(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(program)
    }

    /// The browser tool's entry for Claude's `--mcp-config` `mcpServers`
    /// (named [`MCP_SERVER_NAME`]), for the Chrome on `port`, saving files
    /// only in `workspace`.
    #[must_use]
    pub fn mcp_server(&self, port: u16, workspace: &Path) -> serde_json::Value {
        let env: serde_json::Map<_, _> = MCP_ENV
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .chain(self.config.env.iter().map(|(k, v)| (k.clone(), json!(v))))
            .collect();
        let args = mcp_args(&format!("http://127.0.0.1:{port}"), workspace);
        match &self.config.built_in {
            // The built-in Node.js runs the built-in tool.
            Some(tool) => json!({
                "command": tool.node,
                "args": std::iter::once(tool.entry.display().to_string()).chain(args).collect::<Vec<_>>(),
                "env": env,
            }),
            None => json!({
                "command": self.config.mcp_program(),
                "args": args,
                "env": env,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_keep_files_in_the_bot_folder_and_stats_off() {
        let args = mcp_args("http://127.0.0.1:9333", Path::new("/bots/x/work"));
        assert_eq!(args[0], "--browserUrl=http://127.0.0.1:9333");
        for wanted in [
            "--workspace=/bots/x/work",
            "--usageStatistics=false",
            "--performanceCrux=false",
            "--redactNetworkHeaders=true",
            "--categoryExtensions=false",
        ] {
            assert!(args.iter().any(|a| a == wanted), "missing {wanted}");
        }
    }

    #[test]
    fn config_lives_in_the_oxbot_home() {
        let config = ToolsConfig::in_home(Path::new("/h"));
        assert_eq!(config.profile_dir, Path::new("/h/browser/profile"));
        assert_eq!(
            config.mcp_program(),
            Path::new("/h/tools/node_modules/.bin/chrome-devtools-mcp")
        );
    }

    #[test]
    fn leftover_throwaway_profiles_are_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ToolsConfig::in_home(tmp.path());
        std::fs::create_dir_all(config.scratch_dir.join("old-123/Default")).unwrap();
        let tools = Tools::new(config);
        assert!(!tools.config().scratch_dir.exists());
    }

    #[test]
    fn the_built_in_tool_runs_on_the_built_in_node() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(BuiltInTool::find_in(tmp.path()), None, "nothing there yet");
        let entry = tmp
            .path()
            .join("mcp/node_modules/chrome-devtools-mcp/build/src/bin");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("chrome-devtools-mcp.js"), "").unwrap();
        std::fs::create_dir_all(tmp.path().join("node/bin")).unwrap();
        std::fs::write(tmp.path().join("node/bin/node"), "").unwrap();
        let tool = BuiltInTool::find_in(tmp.path()).expect("found");
        let tools = Tools::new(ToolsConfig {
            built_in: Some(tool.clone()),
            ..ToolsConfig::in_home(tmp.path())
        });
        assert!(tools.is_installed());
        let entry = tools.mcp_server(9333, Path::new("/bots/x/work"));
        assert_eq!(entry["command"], json!(tool.node));
        assert_eq!(entry["args"][0], json!(tool.entry.display().to_string()));
        assert_eq!(entry["args"][1], "--browserUrl=http://127.0.0.1:9333");
    }
}
