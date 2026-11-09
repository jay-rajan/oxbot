//! The oxbot app.
//!
//! - On a Mac it is the home computer (or shows an `oxbot serve` already running):
//!   see [`desktop`].
//! - On a phone it is only ever a device: a window onto the home computer. The server,
//!   the bots and Chrome aren't even compiled in. See [`device`].

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod device;
mod links;

/// Runs the app until it quits.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(desktop)]
    let result = desktop::run();
    #[cfg(mobile)]
    let result = device::run();
    if let Err(err) = result {
        tracing::error!("oxbot couldn't start: {err:#}");
        eprintln!("oxbot couldn't start: {err:#}");
        std::process::exit(1);
    }
}
