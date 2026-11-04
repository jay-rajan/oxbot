//! Mac notifications: the hub's usage alerts, and (in the desktop app,
//! while its window is hidden) a bot waiting for an OK or a sign-in.
//!
//! One task per server listens to the hub's updates and hands what's
//! worth a notification to a [`Notifier`]. It's the only part of the
//! server that shows anything outside its own window.
//!
//! Notifications can be read on a locked screen, so they say little: a
//! usage alert has the limit, the percentage, the reset time and the
//! models; a waiting bot has only the bot's name, and not even that for
//! a bot that's private to someone other than the Mac's owner.

use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::time::Duration;

use oxbot_core::Hub;
use oxbot_protocol::ServerMsg;
use tokio::sync::broadcast;

use crate::server::{AppControls, AppState};

/// The title on every notification.
const TITLE: &str = "oxbot";
/// How long `osascript` may take to post one.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);

/// Shows a notification on this Mac.
pub trait Notifier: Send + Sync + std::fmt::Debug {
    /// Posts `body` under `title`. It never waits for the user, and a
    /// failure is only logged: the page shows the same thing.
    fn notify(&self, title: &str, body: &str);
}

/// Writes notifications to the log instead (end-to-end tests read them
/// in Settings → Activity).
#[derive(Debug, Default)]
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, title: &str, body: &str) {
        tracing::info!(target: "oxbot::notify", "Mac notification: {title}: {body}");
    }
}

/// `oxbot serve` on a Mac: posts through `osascript` (it shows as Script
/// Editor). The text goes in as arguments, never into the script, so no
/// text can change what the script does.
#[derive(Debug, Default)]
pub struct ScriptNotifier;

/// The script: shows its first two arguments as title and text.
const SCRIPT: [&str; 6] = [
    "-e",
    "on run argv",
    "-e",
    "display notification (item 2 of argv) with title (item 1 of argv)",
    "-e",
    "end run",
];

impl Notifier for ScriptNotifier {
    fn notify(&self, title: &str, body: &str) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut cmd = tokio::process::Command::new("/usr/bin/osascript");
        cmd.args(SCRIPT)
            .arg(title)
            .arg(body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        runtime.spawn(async move {
            let run = async { cmd.status().await };
            match tokio::time::timeout(SCRIPT_TIMEOUT, run).await {
                Ok(Ok(status)) if status.success() => {}
                Ok(Ok(status)) => tracing::warn!(%status, "couldn't post a Mac notification"),
                Ok(Err(err)) => tracing::warn!(%err, "couldn't post a Mac notification"),
                Err(_) => tracing::warn!("posting a Mac notification took too long"),
            }
        });
    }
}

/// The notifier a server uses: the log in tests (never a real
/// notification), else `given` (the desktop app's), else `osascript` on a
/// Mac, else none.
#[must_use]
pub fn pick(given: Option<Arc<dyn Notifier>>, e2e: bool) -> Option<Arc<dyn Notifier>> {
    if e2e {
        Some(Arc::new(LogNotifier))
    } else if given.is_some() {
        given
    } else if cfg!(target_os = "macos") {
        Some(Arc::new(ScriptNotifier))
    } else {
        None
    }
}

/// Starts the task that posts `state`'s notifications, if it has a
/// notifier. Call once per server; it ends when the hub does.
pub fn start(state: &AppState) {
    let Some(notifier) = state.notifier.clone() else {
        return;
    };
    let updates = state.hub.subscribe();
    let hub = Arc::downgrade(&state.hub);
    tokio::spawn(run(updates, hub, state.app.clone(), notifier));
}

async fn run(
    mut updates: broadcast::Receiver<ServerMsg>,
    hub: Weak<Hub>,
    app: Option<Arc<dyn AppControls>>,
    notifier: Arc<dyn Notifier>,
) {
    let mut waiting = Waiting::default();
    loop {
        match updates.recv().await {
            Ok(msg) => {
                let Some(hub) = hub.upgrade() else { return };
                if let Some(body) = waiting.body(&msg, &hub, app.as_deref()) {
                    notifier.notify(TITLE, &body);
                }
            }
            // Alerts are recorded by the hub, so skipping ahead never sends
            // one twice; a missed card just goes unannounced.
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!(n, "notifications skipped some updates");
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// The most cards remembered; past it the list starts over (a resolved
/// card missed while lagging would otherwise stay forever).
const MAX_REMEMBERED: usize = 512;

/// The cards already announced, so a card sent again (a sign-in window
/// opening) isn't announced twice.
#[derive(Debug, Default)]
struct Waiting {
    approvals: HashSet<u64>,
    sign_ins: HashSet<u64>,
}

impl Waiting {
    /// What to post for `msg`, if anything.
    fn body(
        &mut self,
        msg: &ServerMsg,
        hub: &Hub,
        app: Option<&dyn AppControls>,
    ) -> Option<String> {
        for seen in [&mut self.approvals, &mut self.sign_ins] {
            if seen.len() >= MAX_REMEMBERED {
                seen.clear();
            }
        }
        match msg {
            // The hub sends these only while alerts are on.
            ServerMsg::UsageAlert(alert) => Some(alert.message.clone()),
            ServerMsg::ApprovalNeeded(a) => {
                let new = self.approvals.insert(a.id);
                (new && hidden(hub, app))
                    .then(|| waiting_body(hub, &a.bot, "is waiting for your OK"))
            }
            ServerMsg::SignInNeeded(r) => {
                let new = self.sign_ins.insert(r.id);
                (new && hidden(hub, app)).then(|| waiting_body(hub, &r.bot, "needs you to sign in"))
            }
            // A scheduled run's answer, on the Mac's screen only when it's
            // the Mac owner's own schedule (anyone else's goes to their
            // devices alone). One that waits is the card's to announce.
            ServerMsg::Notify {
                text,
                person_id,
                waiting: false,
                ..
            } => (hidden(hub, app) && hub.is_farm_person(person_id)).then(|| text.clone()),
            ServerMsg::ApprovalResolved { id } => {
                self.approvals.remove(id);
                None
            }
            ServerMsg::SignInResolved { id } => {
                self.sign_ins.remove(id);
                None
            }
            _ => None,
        }
    }
}

/// Whether a waiting bot should post a notification now: the switch is
/// on and the app's window is hidden (never when the server can't tell).
fn hidden(hub: &Hub, app: Option<&dyn AppControls>) -> bool {
    app.and_then(AppControls::window_visible) == Some(false) && hub.notify_when_hidden()
}

/// `Mail is waiting for your OK`: the bot's name only, never the site,
/// page or text on its card. Someone else's private bot is just "A bot".
fn waiting_body(hub: &Hub, bot: &str, what: &str) -> String {
    let name = hub.notice_name(bot).unwrap_or_else(|| "A bot".to_owned());
    format!("{name} {what}")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use oxbot_core::{Bot, BotId, HubConfig, Store};
    use oxbot_protocol::{Approval, ApprovalKind, SignInRequest};

    use super::*;

    /// The desktop app's window, shown or hidden.
    #[derive(Debug)]
    struct Window(Mutex<bool>);

    impl AppControls for Window {
        fn launch_at_login(&self) -> Option<bool> {
            None
        }
        fn set_launch_at_login(&self, _on: bool) -> Result<(), String> {
            Ok(())
        }
        fn window_visible(&self) -> Option<bool> {
            Some(*self.0.lock().unwrap())
        }
    }

    fn card(id: u64, bot: &str) -> ServerMsg {
        ServerMsg::ApprovalNeeded(Approval {
            id,
            bot: bot.into(),
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: "button \"Pay now\"".into(),
            site: "bank.example".into(),
            page: Some("Transfer".into()),
            detail: Some("typed: 4210".into()),
            screenshot: None,
            asker: None,
            asker_id: None,
            always: false,
            rule: None,
            schedule: None,
            also: Vec::new(),
        })
    }

    fn sign_in(id: u64, bot: &str, window_open: bool) -> ServerMsg {
        ServerMsg::SignInNeeded(SignInRequest {
            id,
            bot: bot.into(),
            url: "https://mail.example.com/login".into(),
            site: "mail.example.com".into(),
            why: "to read your mail".into(),
            window_open,
        })
    }

    #[tokio::test]
    async fn a_waiting_bot_is_named_only_while_the_window_is_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let mail = Bot::create(tmp.path(), BotId::new("mail").unwrap(), "Mail", "").unwrap();
        let hub = Hub::new(
            vec![mail],
            Store::open_in_memory().unwrap(),
            HubConfig::new(tmp.path()),
        );
        hub.farm_person_from("Jay", true).unwrap();
        let sams = hub
            .create_bot_with("Bank", "", Some("Sam"), true)
            .await
            .unwrap();
        let window = Window(Mutex::new(true));
        let app: Option<&dyn AppControls> = Some(&window);
        let mut waiting = Waiting::default();

        assert_eq!(waiting.body(&card(1, "mail"), &hub, app), None, "on screen");
        *window.0.lock().unwrap() = false;
        assert_eq!(
            waiting.body(&card(1, "mail"), &hub, app),
            None,
            "once a card"
        );
        let body = waiting.body(&card(2, "mail"), &hub, app).unwrap();
        assert_eq!(body, "Mail is waiting for your OK");
        assert!(!body.contains("bank.example") && !body.contains("4210"));
        assert_eq!(
            waiting.body(&card(3, &sams.id), &hub, app).as_deref(),
            Some("A bot is waiting for your OK"),
            "someone else's private bot isn't named on the Mac's screen"
        );
        assert_eq!(
            waiting
                .body(&sign_in(1, "mail", false), &hub, app)
                .as_deref(),
            Some("Mail needs you to sign in")
        );
        assert_eq!(
            waiting.body(&sign_in(1, "mail", true), &hub, app),
            None,
            "the window opening isn't a new request"
        );

        hub.set_notify_when_hidden(false).unwrap();
        assert_eq!(
            waiting.body(&card(4, "mail"), &hub, app),
            None,
            "switched off"
        );
        hub.set_notify_when_hidden(true).unwrap();
        assert_eq!(
            waiting.body(&card(5, "mail"), &hub, None),
            None,
            "a server that can't tell never posts these"
        );
    }

    #[tokio::test]
    async fn a_scheduled_runs_answer_shows_on_the_mac_only_for_the_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let mail = Bot::create(tmp.path(), BotId::new("mail").unwrap(), "Mail", "").unwrap();
        let hub = Hub::new(
            vec![mail],
            Store::open_in_memory().unwrap(),
            HubConfig::new(tmp.path()),
        );
        let jay = hub.farm_person_from("Jay", true).unwrap().id;
        let sam = hub.person_named("Sam").unwrap().id;
        let window = Window(Mutex::new(false));
        let app: Option<&dyn AppControls> = Some(&window);
        let line = |person: &oxbot_core::id::PersonId, waiting| ServerMsg::Notify {
            bot: "mail".into(),
            text: "Mail: 3 new letters".into(),
            waiting,
            person_id: person.to_string(),
        };
        let mut seen = Waiting::default();
        assert_eq!(
            seen.body(&line(&jay, false), &hub, app).as_deref(),
            Some("Mail: 3 new letters")
        );
        assert_eq!(seen.body(&line(&sam, false), &hub, app), None, "Sam's own");
        assert_eq!(
            seen.body(&line(&jay, true), &hub, app),
            None,
            "its card says so"
        );
        *window.0.lock().unwrap() = true;
        assert_eq!(
            seen.body(&line(&jay, false), &hub, app),
            None,
            "the page shows it"
        );
    }

    #[tokio::test]
    async fn a_usage_alert_is_posted_as_its_message() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            Store::open_in_memory().unwrap(),
            HubConfig::new(tmp.path()),
        );
        let alert = oxbot_protocol::UsageAlert {
            limit: "openrouter_free_daily".into(),
            level: 50,
            used: 0.5,
            resets_at_ms: 0,
            message: "OpenRouter's free daily requests are at 50%.".into(),
        };
        let body = Waiting::default().body(&ServerMsg::UsageAlert(alert), &hub, None);
        assert_eq!(
            body.as_deref(),
            Some("OpenRouter's free daily requests are at 50%.")
        );
    }

    #[test]
    fn the_script_takes_its_text_as_arguments() {
        // The text is never part of the script itself.
        assert!(SCRIPT.iter().all(|line| !line.contains('"')));
        assert!(SCRIPT.contains(&"on run argv"));
    }
}
