//! The oxbot app.
//!
//! - On a Mac it is the Farm (or shows an `oxbot serve` already running):
//!   see [`desktop`].
//! - On a phone it is only ever a Gate: a window onto the Farm. The server,
//!   the bots and Chrome aren't even compiled in. See [`gate`].

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod gate;
mod links;

/// Runs the app until it quits.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(desktop)]
    let result = desktop::run();
    #[cfg(mobile)]
    let result = gate::run();
    if let Err(err) = result {
        tracing::error!("oxbot couldn't start: {err:#}");
        eprintln!("oxbot couldn't start: {err:#}");
        std::process::exit(1);
    }
}
