//! oxbot's browser: Camoufox (a Firefox build) shipped inside oxbot.app,
//! driven through Playwright by a Node helper oxbot owns (see `camoufox`).
//!
//! - [`Tools`]: the bundled browser runtime and the bots' browser tool.
//! - [`Browser`]: the browser every bot of one owner uses, with a kept
//!   profile: the user signs in to a site once (with the window brought
//!   forward) and every bot stays signed in. Bots take turns on it (see
//!   `oxbot-core`).
//!
//! The browser opens no debugging port. oxbot talks to its helper over
//! private stdio; bots' tools reach it only through a local address with a
//! random secret path that offers the browser tools and nothing else.

mod browser;
mod camoufox;
mod cookies;
pub mod docfetch;
mod element;
pub mod netguard;
pub mod sealed;
pub mod trial;
pub mod visits;
mod web;

use std::path::{Path, PathBuf};

pub use oxbot_engine::McpCommand;

pub use browser::{
    Browser, BrowserSpec, BrowserStatus, NO_PRACTICE_RUNS, NO_SEALED_READS, UidCheck, site_of,
};
pub use camoufox::CamoufoxTool;
pub use cookies::{CookieMarks, site_cookie_params};
pub use element::{ELEMENT_FACTS_FN, ElementFacts};
pub use netguard::{BlockedRequest, NetMode};
pub use visits::{Visit, VisitSink};
pub use web::{is_home_network, normalize_web_url, web_host};

/// The browser tool's server name; its actions are
/// `mcp__chrome-devtools__*` in oxbot's own names. The name is kept from
/// the Chrome tool it replaced so saved approval rules and the tool policy
/// stay as they are.
pub const MCP_SERVER_NAME: &str = "chrome-devtools";

/// The version of the bots' browser tool list (`scripts/camoufox/
/// tool-specs.mjs`). Change it with the list: cached lists are keyed by it.
pub const TOOLS_VERSION: &str = "camoufox-1";

/// Errors from the browser.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// The bundled browser isn't there (or is incomplete).
    #[error(
        "oxbot's browser is missing. Reinstall oxbot, or in a checkout run: node scripts/camoufox/build.mjs"
    )]
    Missing,
    /// The profile folder couldn't be deleted.
    #[error("couldn't delete {path}: {source}")]
    Remove {
        /// The folder.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The browser refused, stopped or didn't answer.
    #[error("browser: {0}")]
    Browser(String),
}

/// Where the browser's programs and profile live.
#[derive(Debug, Clone)]
pub struct ToolsConfig {
    /// The bundled browser runtime. `None`: this oxbot has none, and the
    /// browser reports [`BrowserError::Missing`].
    pub camoufox: Option<CamoufoxTool>,
    /// The shared browser's kept profile.
    pub profile_dir: PathBuf,
    /// Where the throwaway profiles of older oxbot versions were; deleted
    /// if found.
    pub scratch_dir: PathBuf,
    /// Extra environment for the browser helper and tool.
    pub env: Vec<(String, String)>,
    /// End-to-end tests only: the browser tool bots run instead of the real
    /// one (a stand-in).
    pub mcp_override: Option<McpCommand>,
}

impl ToolsConfig {
    /// Defaults under oxbot's data folder: `browser/camoufox-v1/profile`.
    #[must_use]
    pub fn in_home(root: &Path) -> Self {
        Self {
            camoufox: None,
            profile_dir: root.join("browser").join("camoufox-v1").join("profile"),
            scratch_dir: root.join("scratch"),
            env: Vec::new(),
            mcp_override: None,
        }
    }
}

/// The browser runtime and the bots' browser tool. Share it as
/// `Arc<Tools>`.
#[derive(Debug)]
pub struct Tools {
    config: ToolsConfig,
    courtesy: std::sync::Arc<camoufox::Courtesy>,
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
            courtesy: std::sync::Arc::default(),
        })
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &ToolsConfig {
        &self.config
    }

    /// Whether the bundled browser is complete.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.config
            .camoufox
            .as_ref()
            .is_some_and(CamoufoxTool::available)
    }

    /// The browser tool as a command, for the browser at `endpoint` (see
    /// [`Browser::endpoint`]), touching files only in `workspace` (or, in
    /// end-to-end tests, the stand-in set as [`ToolsConfig::mcp_override`]).
    /// Without a bundled browser the command can't start, and the bot is
    /// told it has no browser.
    #[must_use]
    pub fn mcp_command(&self, endpoint: &str, workspace: &Path) -> McpCommand {
        if let Some(stand_in) = &self.config.mcp_override {
            // Which browser and folder, as the real tool is told.
            let mut stand_in = stand_in.clone();
            stand_in.args.push(format!("--endpoint={endpoint}"));
            stand_in
                .args
                .push(format!("--workspace={}", workspace.display()));
            return stand_in;
        }
        let (node, bundle) = self.config.camoufox.as_ref().map_or_else(
            || (PathBuf::from("node"), PathBuf::new()),
            |tool| (tool.node.clone(), tool.bundle.clone()),
        );
        McpCommand {
            program: node,
            args: vec![
                bundle.join("mcp.mjs").display().to_string(),
                "--endpoint".into(),
                endpoint.into(),
                "--workspace".into(),
                workspace.display().to_string(),
            ],
            env: self.config.env.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lives_in_the_oxbot_home() {
        let config = ToolsConfig::in_home(Path::new("/h"));
        assert_eq!(
            config.profile_dir,
            Path::new("/h/browser/camoufox-v1/profile")
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
    fn the_tool_runs_on_the_bundled_node_and_keeps_files_in_the_bot_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = CamoufoxTool {
            node: tmp.path().join("node/bin/node"),
            bundle: tmp.path().join("camoufox"),
            headless: false,
            proxy: None,
        };
        let tools = Tools::new(ToolsConfig {
            camoufox: Some(tool.clone()),
            ..ToolsConfig::in_home(tmp.path())
        });
        assert!(!tools.is_available(), "nothing there yet");
        let command = tools.mcp_command("http://127.0.0.1:9/t0k3n", Path::new("/bots/x/work"));
        assert_eq!(command.program, tool.node);
        assert_eq!(
            command.args,
            [
                tool.bundle.join("mcp.mjs").display().to_string(),
                "--endpoint".into(),
                "http://127.0.0.1:9/t0k3n".into(),
                "--workspace".into(),
                "/bots/x/work".into(),
            ]
        );
    }

    #[test]
    fn a_stand_in_replaces_the_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let stand_in = McpCommand {
            program: PathBuf::from("/e2e/oxbot-fake-devtools"),
            args: Vec::new(),
            env: vec![("OXBOT_FAKE_SCRIPT".into(), "x.toml".into())],
        };
        let tools = Tools::new(ToolsConfig {
            mcp_override: Some(stand_in.clone()),
            ..ToolsConfig::in_home(tmp.path())
        });
        // The stand-in is told which browser and folder, as the real tool is.
        let mut told = stand_in.clone();
        told.args.push("--endpoint=http://127.0.0.1:9/t0k3n".into());
        told.args.push("--workspace=/w".into());
        assert_eq!(
            tools.mcp_command("http://127.0.0.1:9/t0k3n", Path::new("/w")),
            told
        );
    }
}
