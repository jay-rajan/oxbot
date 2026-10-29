//! Starting and stopping a whole oxbot server: bots, history, browser and the
//! WebSocket. Shared by `oxbot serve` and the desktop app.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use oxbot_browser::{BuiltInTool, Tools, ToolsConfig};
use oxbot_core::{Home, Hub, HubConfig, Store, bot};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::logs::LogRelay;
use crate::server::{self, AppControls, AppState};
use crate::setup;

/// How to run the server.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Port on `127.0.0.1` (0 picks a free one).
    pub port: u16,
    /// Data folder; `None` uses `OXBOT_HOME` or the default.
    pub home: Option<Home>,
    /// Give bots a browser.
    pub browser: bool,
    /// The log relay to show in the apps.
    pub logs: Option<Arc<LogRelay>>,
    /// Dev mode: serve the page from this folder (default: `OXBOT_UI_DIR`).
    pub ui_dir: Option<PathBuf>,
    /// What the desktop app lets the page control.
    pub app: Option<Arc<dyn AppControls>>,
}

impl ServerOptions {
    /// Defaults: the given port, browser on, the page from `OXBOT_UI_DIR`
    /// if set, otherwise built in.
    #[must_use]
    pub fn new(port: u16) -> Self {
        Self {
            port,
            home: None,
            browser: true,
            logs: None,
            ui_dir: std::env::var_os("OXBOT_UI_DIR").map(PathBuf::from),
            app: None,
        }
    }
}

/// A running server. Call [`RunningServer::stop`] to shut it down cleanly
/// (it stops every `claude` and the shared Chrome; sign-ins stay).
#[derive(Debug)]
pub struct RunningServer {
    /// Where it listens.
    pub addr: SocketAddr,
    /// The bots.
    pub hub: Arc<Hub>,
    /// Its data folder.
    pub home: Home,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

impl RunningServer {
    /// The chat page's address, e.g. `http://127.0.0.1:7878`.
    #[must_use]
    pub fn page_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops serving, then stops the bots and the browser.
    pub async fn stop(self) {
        tracing::info!("shutting down");
        let _ = self.stop.send(());
        if let Ok(Err(err)) = self.task.await {
            tracing::warn!(%err, "server stopped with an error");
        }
        self.hub.shutdown().await;
    }
}

/// Loads the bots from `OXBOT_HOME` (or the default data folder) and starts
/// serving on `127.0.0.1:<port>`.
///
/// # Errors
/// Fails if the data folder can't be read or the port is taken.
pub async fn start(options: ServerOptions) -> anyhow::Result<RunningServer> {
    let home = match options.home {
        Some(home) => home,
        None => Home::from_env().context("no data folder on this system; set OXBOT_HOME")?,
    };
    bot::ensure_default(&home.bots_dir())?;
    let bots = bot::load_all(&home.bots_dir())?;
    let store = Store::open(&home.db_path())?;
    // Programs started from here (claude, npm, the browser tool) look for
    // their tools on this PATH, which works when oxbot was opened from Finder.
    let path = setup::search_path();
    let path_env = vec![("PATH".to_owned(), path.clone())];
    let built_in = built_in_tool();
    if let Some(tool) = &built_in {
        tracing::info!(node = %tool.node.display(), "using the browser tool built into the app");
    } else {
        tracing::info!("no built-in browser tool; it is installed with npm on first use");
    }
    let browser = options.browser.then(|| {
        Tools::new(ToolsConfig {
            env: path_env.clone(),
            built_in: built_in.clone(),
            ..ToolsConfig::in_home(home.root())
        })
    });
    let wanted = SocketAddr::from((Ipv4Addr::LOCALHOST, options.port));
    let listener = tokio::net::TcpListener::bind(wanted)
        .await
        .with_context(|| format!("couldn't listen on {wanted}; is oxbot already running?"))?;
    let addr = listener.local_addr()?;
    let config = HubConfig {
        claude_program: std::env::var_os("OXBOT_CLAUDE")
            .map_or_else(|| PathBuf::from("claude"), PathBuf::from),
        browser,
        extra_env: path_env,
        ..HubConfig::new(home.bots_dir())
    };
    let hub = Hub::new(bots, store, config);

    let device = server_name();
    let state = AppState {
        hub: Arc::clone(&hub),
        server_name: device.clone(),
        started_ms: now_ms(),
        logs: options.logs,
        ui_dir: options.ui_dir,
        data_dir: home.root().to_owned(),
        app: options.app,
        local_person: setup::local_person(&device),
        search_path: path,
        setup: Arc::default(),
        tool_built_in: built_in.is_some(),
        setup_running: Arc::default(),
    };
    state.refresh_setup().await;
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server::serve(listener, state, async {
        let _ = stopped.await;
    }));
    tracing::info!(home = %home.root().display(), browser = options.browser, "oxbot is running: open http://{addr}");
    Ok(RunningServer {
        addr,
        hub,
        home,
        stop,
        task,
    })
}

/// The browser tool shipped inside oxbot.app (`Contents/Resources/tools`,
/// next to the program), or in `OXBOT_BUILT_IN_TOOLS` for testing.
/// `oxbot serve` from a source checkout has none and uses npm instead.
fn built_in_tool() -> Option<BuiltInTool> {
    if let Some(dir) = std::env::var_os("OXBOT_BUILT_IN_TOOLS") {
        return BuiltInTool::find_in(std::path::Path::new(&dir));
    }
    let exe = std::env::current_exe().ok()?;
    let resources = exe.parent()?.parent()?.join("Resources/tools");
    BuiltInTool::find_in(&resources)
}

/// The Mac's name, for showing in the apps.
#[must_use]
pub fn server_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "oxbot".to_owned())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_serves_the_page_and_stops() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let tmp = tempfile::tempdir().unwrap();
        let options = ServerOptions {
            home: Some(Home::new(tmp.path())),
            browser: false,
            ..ServerOptions::new(0)
        };
        let server = start(options).await.unwrap();
        assert!(
            tmp.path().join("bots/assistant").is_dir(),
            "default bot created"
        );

        let mut tcp = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        tcp.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut page = String::new();
        tcp.read_to_string(&mut page).await.unwrap();
        assert!(page.contains("<title>oxbot</title>"));

        let addr = server.addr;
        server.stop().await;
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "stopped listening"
        );
    }
}
