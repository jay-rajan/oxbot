//! oxbot as a Mac app.
//!
//! - The oxbot server runs inside the app (the same code as `oxbot serve`).
//!   If an `oxbot serve` is already running, the app shows that one instead.
//! - The window is the chat page. Closing it only hides it: oxbot keeps
//!   running in the menu bar.
//! - The menu bar icon has Open oxbot, Launch at login and Quit. Quitting
//!   stops every bot and its Chrome cleanly.
//! - The page's Settings panel can switch Launch at login too
//!   ([`DesktopControls`]).

use std::sync::Arc;

use anyhow::Context as _;
use oxbot::logs::{LogRelay, RelayLayer};
use oxbot::runtime::{self, RunningServer, ServerOptions};
use oxbot::server::AppControls;
use oxbot::{DEFAULT_PORT, client};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager as _, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

const MAIN_WINDOW: &str = "main";

/// The port to use: `OXBOT_PORT` if set (handy for a second, test copy),
/// otherwise oxbot's usual one.
fn port() -> u16 {
    std::env::var("OXBOT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// What the app holds on to.
struct Oxbot {
    /// Our own server, or `None` when showing an `oxbot serve` that was
    /// already running.
    server: tokio::sync::Mutex<Option<RunningServer>>,
}

/// The menu bar's "Launch at login" checkbox, once it exists.
struct AutostartItem(std::sync::Mutex<Option<CheckMenuItem<tauri::Wry>>>);

/// Lets the page's Settings panel switch Launch at login, keeping the menu
/// bar checkbox in step.
struct DesktopControls(AppHandle);

impl std::fmt::Debug for DesktopControls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DesktopControls")
    }
}

impl AppControls for DesktopControls {
    fn launch_at_login(&self) -> Option<bool> {
        self.0.autolaunch().is_enabled().ok()
    }

    fn set_launch_at_login(&self, on: bool) -> Result<(), String> {
        set_autostart(&self.0, on)
    }
}

/// Turns Launch at login on or off and updates the menu bar checkbox.
fn set_autostart(app: &AppHandle, on: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    let result = if on {
        launcher.enable()
    } else {
        launcher.disable()
    };
    let now = launcher.is_enabled().unwrap_or(false);
    if let Some(item) = app
        .state::<AutostartItem>()
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        let _ = item.set_checked(now);
    }
    result.map_err(|e| e.to_string())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("oxbot couldn't start: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let relay = LogRelay::new();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())),
        )
        .with(RelayLayer(Arc::clone(&relay)))
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            let port = port();
            app.manage(AutostartItem(std::sync::Mutex::new(None)));
            let controls: Arc<dyn AppControls> = Arc::new(DesktopControls(app.handle().clone()));
            let (server, url) =
                tauri::async_runtime::block_on(start_or_attach(relay, port, controls))?;
            app.manage(Oxbot {
                server: tokio::sync::Mutex::new(server),
            });
            // A normal quit signal (e.g. `kill`) quits cleanly too, so no Chrome is left behind.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(mut term) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    term.recv().await;
                    handle.exit(0);
                }
            });
            WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::External(url.parse()?))
                .title("oxbot")
                .inner_size(1100.0, 760.0)
                .min_inner_size(420.0, 480.0)
                .build()?;
            build_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it; oxbot keeps running in the menu bar.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .context("couldn't build the app")?;

    app.run(|app, event| match event {
        // Clicking the Dock icon brings the window back.
        RunEvent::Reopen { .. } => show_main(app),
        RunEvent::Exit => {
            let state = app.state::<Oxbot>();
            tauri::async_runtime::block_on(async {
                if let Some(server) = state.server.lock().await.take() {
                    server.stop().await;
                }
            });
        }
        _ => {}
    });
    Ok(())
}

/// Starts our own server, or uses an `oxbot serve` that's already running.
async fn start_or_attach(
    relay: Arc<LogRelay>,
    port: u16,
    controls: Arc<dyn AppControls>,
) -> anyhow::Result<(Option<RunningServer>, String)> {
    if client::server_running(port).await {
        tracing::info!("using the oxbot server that's already running on port {port}");
        return Ok((None, format!("http://127.0.0.1:{port}")));
    }
    let options = ServerOptions {
        logs: Some(relay),
        app: Some(controls),
        ..ServerOptions::new(port)
    };
    let server = runtime::start(options).await?;
    let url = server.page_url();
    Ok((Some(server), url))
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open oxbot", true, None::<&str>)?;
    let at_login = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Launch at login",
        true,
        at_login,
        None::<&str>,
    )?;
    *app.state::<AutostartItem>()
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(autostart.clone());
    let quit = MenuItem::with_id(app, "quit", "Quit oxbot", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id("oxbot")
        .icon(Image::from_bytes(include_bytes!("../icons/tray@2x.png"))?)
        .icon_as_template(true)
        .tooltip("oxbot")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "autostart" => {
                let on = !app.autolaunch().is_enabled().unwrap_or(false);
                if let Err(err) = set_autostart(app, on) {
                    tracing::warn!(%err, "couldn't change Launch at login");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
