//! A stand-in for `OpenRouter` in end-to-end tests; see
//! [`oxbot_e2e::fake_openrouter`].
//!
//! ```text
//! oxbot-fake-openrouter --dir DIR
//! ```
//!
//! Listens on a free port on 127.0.0.1, writes its canary key to
//! `DIR/canary` (its own folder, never oxbot's), then prints one line,
//! `{"port":N}`, and serves until it's stopped. Reads `OXBOT_FAKE_SCRIPT`,
//! `OXBOT_FAKE_OR_DAILY` and `OXBOT_FAKE_OR_LOG`.

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use oxbot_e2e::fake_openrouter::{Config, router};

#[derive(Debug, Parser)]
#[command(name = "oxbot-fake-openrouter", about = "A stand-in for OpenRouter")]
struct Cli {
    /// Its own folder, for the canary key.
    #[arg(long)]
    dir: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env()?;
    std::fs::create_dir_all(&cli.dir)?;
    std::fs::write(cli.dir.join("canary"), &config.canary).context("writing the canary")?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{{\"port\":{port}}}")?;
        out.flush()?;
    }
    axum::serve(listener, router(config)).await?;
    Ok(())
}
