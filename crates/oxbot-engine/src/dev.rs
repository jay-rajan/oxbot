//! Test and developer settings read from the environment (`OXBOT_E2E`,
//! `OXBOT_CODEX`, `OXBOT_FAKE_*`…): the one place oxbot reads them.
//!
//! They take effect only in debug builds. The app people install is a
//! release build, where [`dev_env`] always answers `None`, so nothing in
//! its environment can turn on test mode, swap in another Codex or browser
//! tool, or point oxbot at a stand-in `OpenRouter`.
//!
//! Real settings (`OXBOT_HOME`, `OXBOT_DEVICE_PORT`, `OXBOT_PORT`) are read
//! where they're used, in every build.

use std::ffi::OsString;

/// End-to-end test mode, when set to `1`.
pub const E2E: &str = "OXBOT_E2E";

/// The test or developer setting `name`, in a debug build only.
#[must_use]
pub fn dev_env(name: &str) -> Option<OsString> {
    dev_env_in(cfg!(debug_assertions), name)
}

/// Like [`dev_env`], as text (`None` if it isn't Unicode).
#[must_use]
pub fn dev_env_string(name: &str) -> Option<String> {
    dev_env(name).and_then(|v| v.into_string().ok())
}

/// Whether end-to-end test mode is on (`OXBOT_E2E=1`, debug builds only).
#[must_use]
pub fn e2e() -> bool {
    dev_env(E2E).is_some_and(|v| v == "1")
}

/// [`dev_env`] for a build that is (`debug`) or isn't a debug build.
fn dev_env_in(debug: bool, name: &str) -> Option<OsString> {
    if debug { std::env::var_os(name) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_build_ignores_every_test_setting() {
        // `PATH` is always set, so only the build decides.
        assert!(dev_env_in(true, "PATH").is_some());
        assert_eq!(dev_env_in(false, "PATH"), None);
    }

    #[test]
    fn tests_run_as_a_debug_build() {
        assert_eq!(dev_env("PATH").is_some(), cfg!(debug_assertions));
    }
}
