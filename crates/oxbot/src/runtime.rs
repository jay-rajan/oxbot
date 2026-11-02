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
    /// How to post Mac notifications; `None` picks the default (see
    /// [`crate::notify::pick`]).
    pub notifier: Option<Arc<dyn crate::notify::Notifier>>,
    /// The Gate door's port (0 picks a free one; default `OXBOT_GATE_PORT`
    /// or 7879). `None` leaves the door out: `OXBOT_GATE_PORT=off`.
    pub gate_port: Option<u16>,
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
            notifier: None,
            gate_port: gate_port(std::env::var("OXBOT_GATE_PORT").ok().as_deref()),
        }
    }
}

/// The Gate door's port from `OXBOT_GATE_PORT`: a number, `off` for no
/// door at all, or unset for 7879.
fn gate_port(setting: Option<&str>) -> Option<u16> {
    match setting.map(str::trim) {
        Some("off" | "no" | "none" | "false") => None,
        Some(port) => Some(port.parse().unwrap_or_else(|_| {
            tracing::warn!(port, "OXBOT_GATE_PORT isn't a port or `off`; using 7879");
            crate::door::DEFAULT_GATE_PORT
        })),
        None => Some(crate::door::DEFAULT_GATE_PORT),
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
    /// The Gate door, if this server has one.
    pub door: Option<Arc<crate::door::Door>>,
    farm_key: String,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

impl RunningServer {
    /// The chat page's address with the Farm secret, e.g.
    /// `http://127.0.0.1:7878/#k=…`. Whoever opens it is the Farm: show it
    /// only in oxbot's own window or to the person at this Mac.
    #[must_use]
    pub fn page_url(&self) -> String {
        format!("http://{}/#k={}", self.addr, self.farm_key)
    }

    /// Stops serving, then stops the bots and the browser.
    pub async fn stop(self) {
        tracing::info!("shutting down");
        let _ = self.stop.send(());
        if let Some(door) = &self.door {
            door.stop().await;
        }
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
#[allow(
    clippy::too_many_lines,
    reason = "one straight list of what a server is made of"
)]
pub async fn start(options: ServerOptions) -> anyhow::Result<RunningServer> {
    let home = match options.home {
        Some(home) => home,
        None => Home::from_env().context("no data folder on this system; set OXBOT_HOME")?,
    };
    bot::ensure_default(&home.bots_dir())?;
    let bots = bot::load_all(&home.bots_dir())?;
    // A database a newer oxbot upgraded is refused here, with what to do.
    let store = Store::open(&home.db_path())
        .with_context(|| format!("couldn't open {}", home.db_path().display()))?;
    // Programs started from here (claude, npm, the browser tool) look for
    // their tools on this PATH, which works when oxbot was opened from Finder.
    let path = setup::search_path();
    let path_env = vec![("PATH".to_owned(), path.clone())];
    // End-to-end test mode (`/pr_test`): sign-in windows stay hidden, the
    // person can be set, and test connections may say who they are.
    let e2e = std::env::var_os("OXBOT_E2E").is_some_and(|v| v == "1");
    let built_in = built_in_tool();
    let browser = options.browser.then(|| {
        Tools::new(ToolsConfig {
            env: path_env.clone(),
            built_in: built_in.clone(),
            hidden_windows: e2e,
            ..ToolsConfig::in_home(home.root())
        })
    });
    let wanted = SocketAddr::from((Ipv4Addr::LOCALHOST, options.port));
    let listener = tokio::net::TcpListener::bind(wanted)
        .await
        .with_context(|| format!("couldn't listen on {wanted}; is oxbot already running?"))?;
    let addr = listener.local_addr()?;
    let helper = mac_helper();
    let config = HubConfig {
        claude_program: std::env::var_os("OXBOT_CLAUDE")
            .map_or_else(|| PathBuf::from("claude"), PathBuf::from),
        browser,
        extra_env: path_env,
        mac: oxbot_core::attach::MacTools::with_helper(helper.clone()),
        e2e,
        ..HubConfig::new(home.bots_dir())
    };
    let hub = Hub::new(bots, store, config);

    let farm_key =
        crate::secret::farm_key(home.root()).context("couldn't make or read the Farm secret")?;
    let device = server_name();
    let door = options.gate_port.map(|port| {
        // Tests have no real network: they get a fixed one to allow.
        let probe = if e2e {
            crate::door::network::fixed_probe("e2e-network", "Test network")
        } else {
            crate::door::network::mac_probe()
        };
        crate::door::Door::new(
            home.root().join("gate"),
            port,
            device.clone(),
            Arc::clone(&hub),
            probe,
        )
    });
    // Tests name the Farm's person outright, as an account would.
    let (local_person, from_account) = std::env::var("OXBOT_PERSON")
        .ok()
        .filter(|p| e2e && !p.trim().is_empty())
        .map_or_else(|| setup::local_person_from(&device), |p| (p, true));
    // Chosen once (the first run), then found by id; renamed only to the
    // account's own name.
    if let Err(err) = hub.farm_person_from(&local_person, from_account) {
        tracing::warn!(%err, "couldn't choose the Farm's person yet");
    }
    let state = AppState {
        hub: Arc::clone(&hub),
        server_name: device.clone(),
        started_ms: now_ms(),
        logs: options.logs,
        ui_dir: options.ui_dir,
        data_dir: home.root().to_owned(),
        app: options.app,
        local_person,
        e2e,
        search_path: path,
        setup: Arc::default(),
        tool_built_in: built_in.is_some(),
        setup_running: Arc::default(),
        door: door.clone(),
        farm_key: farm_key.clone(),
        notifier: crate::notify::pick(options.notifier, e2e),
        dictation: Arc::new(crate::dictation::Dictation::new(helper)),
        next_conn: Arc::default(),
    };
    state.refresh_setup().await;
    crate::notify::start(&state);
    // The door opens when the Mac is on an allowed network and a Gate is
    // paired, and keeps checking.
    if let Some(door) = &door {
        door.start(server::door_router(state.clone(), door)).await;
    }
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server::serve(listener, state, async {
        let _ = stopped.await;
    }));
    tracing::info!(home = %home.root().display(), browser = options.browser, "oxbot is running: open http://{addr}");
    Ok(RunningServer {
        addr,
        hub,
        home,
        door,
        farm_key,
        stop,
        task,
    })
}

/// The browser tool shipped inside oxbot.app, logged.
fn built_in_tool() -> Option<BuiltInTool> {
    let built_in = find_built_in_tool();
    if let Some(tool) = &built_in {
        tracing::info!(node = %tool.node.display(), "using the browser tool built into the app");
    } else {
        tracing::info!("no built-in browser tool; it is installed with npm on first use");
    }
    built_in
}

/// The browser tool shipped inside oxbot.app (`Contents/Resources/tools`,
/// next to the program), or in `OXBOT_BUILT_IN_TOOLS` for testing.
/// `oxbot serve` from a source checkout has none and uses npm instead.
fn find_built_in_tool() -> Option<BuiltInTool> {
    if let Some(dir) = std::env::var_os("OXBOT_BUILT_IN_TOOLS") {
        return BuiltInTool::find_in(std::path::Path::new(&dir));
    }
    let exe = std::env::current_exe().ok()?;
    let resources = exe.parent()?.parent()?.join("Resources/tools");
    BuiltInTool::find_in(&resources)
}

/// oxbot's Mac helper (`oxbot-mac`: video frames and dictation): next to
/// the program in oxbot.app, or built by `scripts/build-mac-helper.sh`
/// for `oxbot serve` from a checkout. `OXBOT_MAC_HELPER` names another
/// (tests), and `OXBOT_MAC_HELPER=off` none.
fn mac_helper() -> Option<PathBuf> {
    let found = std::env::var_os("OXBOT_MAC_HELPER").map_or_else(
        || {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            [
                dir.join("oxbot-mac"),
                dir.parent()?.join("mac-helper/oxbot-mac"),
            ]
            .into_iter()
            .find(|p| p.is_file())
        },
        |path| {
            let path = PathBuf::from(path);
            (path.as_os_str() != "off" && path.is_file()).then_some(path)
        },
    );
    if let Some(helper) = &found {
        tracing::info!(helper = %helper.display(), "video frames and dictation through the Mac helper");
    } else {
        tracing::info!("no Mac helper: video frames and dictation aren't available");
    }
    found
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

/// Now, in ms since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_database_from_a_newer_oxbot_stops_the_start_with_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::new(tmp.path());
        drop(Store::open(&home.db_path()).unwrap());
        // As a newer oxbot would leave it.
        rusqlite::Connection::open(home.db_path())
            .unwrap()
            .execute_batch("PRAGMA user_version = 1000")
            .unwrap();
        let options = ServerOptions {
            home: Some(home),
            browser: false,
            gate_port: None,
            ..ServerOptions::new(0)
        };
        let err = start(options).await.unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("oxbot.db"), "{text}");
        assert!(text.contains("newer oxbot"), "{text}");
    }

    #[tokio::test]
    async fn starts_serves_the_page_and_stops() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let tmp = tempfile::tempdir().unwrap();
        let options = ServerOptions {
            home: Some(Home::new(tmp.path())),
            browser: false,
            gate_port: None,
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
