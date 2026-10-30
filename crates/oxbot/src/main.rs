//! The `oxbot` command: runs the server and manages bots, pairing and
//! devices. (Day to day, oxbot is used through its desktop app; the page
//! `oxbot serve --open` shows in a browser is for testing and dev mode.)

use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use oxbot::logs::{LogRelay, RelayLayer};
use oxbot::runtime::{self, ServerOptions};
use oxbot::{DEFAULT_PORT, chat, client};
use oxbot_core::{Bot, BotId, Home, bot};
use oxbot_protocol::{ClientMsg, ServerMsg};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

/// Always-on Claude bots on your Mac, reachable from your phone.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the oxbot server without the app (for testing and dev mode; `--open`
    /// shows the chat page in a browser).
    Serve {
        /// Port to listen on.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Open the chat page in your browser.
        #[arg(long)]
        open: bool,
        /// Don't give bots the browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Chat with a bot from the terminal.
    Chat {
        /// The bot's id, e.g. `assistant`.
        #[arg(default_value = bot::DEFAULT_BOT)]
        bot: String,
        /// Port the server listens on.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// Create or list bots.
    Bots {
        #[command(subcommand)]
        action: BotsAction,
    },
    /// Show a QR code to pair a phone.
    Pair,
    /// List or revoke paired phones.
    Devices,
    /// Check that oxbot's privacy settings are in place.
    Doctor,
}

#[derive(Debug, Subcommand)]
enum BotsAction {
    /// Create a bot. Its id is made from the name ("Daily News" becomes
    /// `daily-news`).
    New {
        /// Display name, e.g. "Daily News".
        name: String,
        /// What the bot should do.
        #[arg(long, default_value = "")]
        instructions: String,
        /// Port of a running server, so the bot works without a restart.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// List bots.
    List,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Only the server logs at info level; other commands keep the terminal tidy.
    let default_level = if matches!(cli.command, Command::Serve { .. }) {
        "info"
    } else {
        "warn"
    };
    // The terminal shows the usual log; the relay copies oxbot's lines to the apps.
    let relay = LogRelay::new();
    // Logs go to stderr, so stdout stays clean (`oxbot mcp` speaks JSON-RPC there).
    let terminal = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| default_level.into()));
    tracing_subscriber::registry()
        .with(terminal)
        .with(RelayLayer(Arc::clone(&relay)))
        .init();

    match cli.command {
        Command::Serve {
            port,
            open,
            no_browser,
        } => serve(port, open, !no_browser, relay).await,
        Command::Chat { bot, port } => chat::run(&format!("ws://127.0.0.1:{port}/ws"), &bot).await,
        Command::Bots { action } => bots(action).await,
        Command::Pair | Command::Devices => not_yet(3),
        Command::Doctor => not_yet(5),
    }
}

fn home() -> anyhow::Result<Home> {
    Home::from_env().context("no data folder on this system; set OXBOT_HOME")
}

async fn serve(
    port: u16,
    open: bool,
    with_browser: bool,
    relay: Arc<LogRelay>,
) -> anyhow::Result<()> {
    let options = ServerOptions {
        browser: with_browser,
        logs: Some(relay),
        ..ServerOptions::new(port)
    };
    let server = runtime::start(options).await?;
    if open {
        let _ = std::process::Command::new("open")
            .arg(server.page_url())
            .status();
    }
    // Ctrl-C in a terminal, or SIGTERM from `kill`, launchd or dev mode.
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = async {
            match term.as_mut() {
                Some(t) => { t.recv().await; }
                None => std::future::pending().await,
            }
        } => {}
    }
    server.stop().await;
    Ok(())
}

#[allow(clippy::print_stdout, reason = "CLI output")]
async fn bots(action: BotsAction) -> anyhow::Result<()> {
    let home = home()?;
    match action {
        BotsAction::New {
            name,
            instructions,
            port,
        } => {
            if client::server_running(port).await {
                let msg = ClientMsg::CreateBot {
                    name,
                    instructions,
                    private: false,
                };
                match client::request(port, &msg).await? {
                    ServerMsg::Error { message } => bail!(message),
                    ServerMsg::BotCreated(info) => {
                        println!("Created {}. Chat with it: oxbot chat {}", info.id, info.id);
                    }
                    _ => {}
                }
            } else {
                bot::ensure_default(&home.bots_dir())?;
                let id = BotId::from_name(&name, |id| home.bots_dir().join(id.as_str()).exists());
                Bot::create(&home.bots_dir(), id.clone(), &name, &instructions)?;
                println!("Created {id}. Start oxbot serve, then: oxbot chat {id}");
            }
        }
        BotsAction::List => {
            bot::ensure_default(&home.bots_dir())?;
            for b in bot::load_all(&home.bots_dir())? {
                let first_line = b.instructions.lines().next().unwrap_or("");
                println!("{:<20} {:<20} {first_line}", b.id, b.name);
            }
        }
    }
    Ok(())
}

fn not_yet(milestone: u8) -> anyhow::Result<()> {
    bail!("not built yet: arrives in milestone {milestone}")
}
