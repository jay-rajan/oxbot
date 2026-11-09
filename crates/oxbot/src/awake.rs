//! "Keep the Mac awake while plugged in": while the setting is on, oxbot
//! keeps a child `caffeinate -s -w <oxbot's pid>` running, so the Mac
//! doesn't sleep on power and schedules run on time. `-s` holds off system
//! sleep only on AC power (the display may still sleep); `-w` ends it when
//! oxbot ends, however oxbot ends.
//!
//! [`KeepAwake`] watches the setting: it starts the child when it's on,
//! starts it again if it exits while still on, and stops it when it's
//! turned off or oxbot stops. The program is a [`Holder`], so tests use a
//! stand-in instead of the real `caffeinate`; end-to-end tests (`OXBOT_E2E`)
//! get [`Holder::Pretend`], which only notes the state.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::watch;

/// macOS's own program that holds off sleep.
pub const CAFFEINATE: &str = "/usr/bin/caffeinate";

/// How long to wait before starting the program again after it exits on
/// its own (so a program that can't run doesn't spin).
const RESTART_AFTER: Duration = Duration::from_secs(5);

/// What keeps the Mac awake.
#[derive(Debug, Clone)]
pub enum Holder {
    /// A program run with arguments, e.g. `caffeinate -s -w <pid>`.
    Program {
        /// The program.
        program: PathBuf,
        /// Its arguments.
        args: Vec<String>,
    },
    /// Nothing runs; only the state is kept (end-to-end tests).
    Pretend,
}

impl Holder {
    /// `caffeinate -s -w <pid>`: the Mac stays awake on power while `pid`
    /// (oxbot) runs.
    #[must_use]
    pub fn caffeinate(pid: u32) -> Self {
        Self::Program {
            program: PathBuf::from(CAFFEINATE),
            args: caffeinate_args(pid),
        }
    }
}

/// The arguments for `caffeinate`: `-s` (no system sleep on AC power; the
/// display may sleep) and `-w <pid>` (until `pid` exits).
#[must_use]
pub fn caffeinate_args(pid: u32) -> Vec<String> {
    vec!["-s".to_owned(), "-w".to_owned(), pid.to_string()]
}

/// Keeps the Mac awake while its setting is on. Dropping it stops the
/// program.
#[derive(Debug)]
pub struct KeepAwake {
    on: watch::Sender<bool>,
    state: Arc<State>,
}

/// What tests and logs read.
#[derive(Debug, Default)]
struct State {
    /// The program (or the pretend one) is running now.
    holding: AtomicBool,
    /// How many times it was started.
    starts: AtomicU32,
}

impl KeepAwake {
    /// The Mac's own: `caffeinate` for this process, or a pretend one in
    /// end-to-end tests. `None` where it doesn't apply (not macOS).
    #[must_use]
    pub fn for_this_mac(on: bool, e2e: bool) -> Option<Self> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let holder = if e2e {
            Holder::Pretend
        } else {
            Holder::caffeinate(std::process::id())
        };
        Some(Self::start(holder, on, RESTART_AFTER))
    }

    /// Starts watching the setting (`on` now), holding with `holder`, and
    /// waiting `restart_after` before starting it again after it exits.
    /// Needs a Tokio runtime.
    #[must_use]
    pub fn start(holder: Holder, on: bool, restart_after: Duration) -> Self {
        let (tx, rx) = watch::channel(on);
        let state = Arc::new(State::default());
        tokio::spawn(supervise(holder, rx, Arc::clone(&state), restart_after));
        Self { on: tx, state }
    }

    /// Turns it on or off.
    pub fn set(&self, on: bool) {
        self.on.send_replace(on);
    }

    /// Whether the setting is on.
    #[must_use]
    pub fn is_on(&self) -> bool {
        *self.on.borrow()
    }

    /// Whether it holds the Mac awake now.
    #[must_use]
    pub fn holding(&self) -> bool {
        self.state.holding.load(Ordering::Relaxed)
    }

    /// How many times the program was started.
    #[must_use]
    pub fn starts(&self) -> u32 {
        self.state.starts.load(Ordering::Relaxed)
    }
}

/// Runs the program while the setting is on, until the [`KeepAwake`] goes.
async fn supervise(
    holder: Holder,
    mut on: watch::Receiver<bool>,
    state: Arc<State>,
    restart_after: Duration,
) {
    loop {
        if !*on.borrow_and_update() {
            // Off: wait for it to come on (or for oxbot to stop).
            if on.changed().await.is_err() {
                return;
            }
            continue;
        }
        let Holder::Program { program, args } = &holder else {
            state.starts.fetch_add(1, Ordering::Relaxed);
            state.holding.store(true, Ordering::Relaxed);
            tracing::info!("keeping the Mac awake while plugged in (test stand-in)");
            let gone = wait_until_off(&mut on).await;
            state.holding.store(false, Ordering::Relaxed);
            tracing::info!("no longer keeping the Mac awake");
            if gone {
                return;
            }
            continue;
        };
        let child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(%err, program = %program.display(), "couldn't keep the Mac awake");
                if wait_or_off(&mut on, restart_after).await {
                    return;
                }
                continue;
            }
        };
        state.starts.fetch_add(1, Ordering::Relaxed);
        state.holding.store(true, Ordering::Relaxed);
        tracing::info!("keeping the Mac awake while plugged in");
        tokio::select! {
            status = child.wait() => {
                state.holding.store(false, Ordering::Relaxed);
                tracing::warn!(?status, "the program keeping the Mac awake stopped; starting it again");
                if wait_or_off(&mut on, restart_after).await {
                    return;
                }
            }
            gone = wait_until_off(&mut on) => {
                let _ = child.kill().await;
                state.holding.store(false, Ordering::Relaxed);
                tracing::info!("no longer keeping the Mac awake");
                if gone {
                    return;
                }
            }
        }
    }
}

/// Waits until the setting is off. Returns `true` if the [`KeepAwake`]
/// went instead.
async fn wait_until_off(on: &mut watch::Receiver<bool>) -> bool {
    loop {
        if on.changed().await.is_err() {
            return true;
        }
        if !*on.borrow_and_update() {
            return false;
        }
    }
}

/// Waits `delay` (or less, if the setting changes). Returns `true` if the
/// [`KeepAwake`] went.
async fn wait_or_off(on: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(delay) => false,
        changed = on.changed() => changed.is_err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits up to two seconds for `ok`.
    async fn soon(ok: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if ok() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ok()
    }

    fn stand_in(program: &str, args: &[&str]) -> Holder {
        Holder::Program {
            program: PathBuf::from(program),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn caffeinate_holds_off_sleep_on_power_until_oxbot_ends() {
        assert_eq!(caffeinate_args(4321), ["-s", "-w", "4321"]);
        let Holder::Program { program, args } = Holder::caffeinate(7) else {
            panic!("a program");
        };
        assert_eq!(program, PathBuf::from("/usr/bin/caffeinate"));
        assert_eq!(args, ["-s", "-w", "7"]);
    }

    #[tokio::test]
    async fn it_starts_and_stops_with_the_setting() {
        let awake = KeepAwake::start(
            stand_in("/bin/sleep", &["30"]),
            true,
            Duration::from_millis(10),
        );
        assert!(soon(|| awake.holding()).await);
        awake.set(false);
        assert!(soon(|| !awake.holding()).await);
        assert!(!awake.is_on());
        awake.set(true);
        assert!(soon(|| awake.holding()).await);
        assert_eq!(awake.starts(), 2);
    }

    #[tokio::test]
    async fn off_at_first_starts_nothing() {
        let awake = KeepAwake::start(
            stand_in("/bin/sleep", &["30"]),
            false,
            Duration::from_millis(10),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!awake.holding());
        assert_eq!(awake.starts(), 0);
    }

    #[tokio::test]
    async fn a_program_that_exits_is_started_again_while_on() {
        let awake = KeepAwake::start(
            stand_in("/usr/bin/true", &[]),
            true,
            Duration::from_millis(10),
        );
        assert!(soon(|| awake.starts() >= 3).await, "restarted");
        awake.set(false);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let after = awake.starts();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(awake.starts(), after, "not while off");
    }

    #[tokio::test]
    async fn a_missing_program_is_tried_again_without_holding() {
        let awake = KeepAwake::start(
            stand_in("/no/such/caffeinate", &[]),
            true,
            Duration::from_millis(10),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!awake.holding());
        assert_eq!(awake.starts(), 0);
    }

    #[tokio::test]
    async fn the_test_stand_in_only_notes_the_state() {
        let awake = KeepAwake::start(Holder::Pretend, true, Duration::from_millis(10));
        assert!(soon(|| awake.holding()).await);
        awake.set(false);
        assert!(soon(|| !awake.holding()).await);
    }

    #[tokio::test]
    async fn dropping_it_stops_the_program() {
        let awake = KeepAwake::start(
            stand_in("/bin/sleep", &["30"]),
            true,
            Duration::from_millis(10),
        );
        assert!(soon(|| awake.holding()).await);
        let state = Arc::clone(&awake.state);
        drop(awake);
        assert!(soon(|| !state.holding.load(Ordering::Relaxed)).await);
    }
}
