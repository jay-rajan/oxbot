//! The oxbot app's desktop entry point. Everything lives in the library so
//! the Android build can share it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    oxbot_app::run();
}
