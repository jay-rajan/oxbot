//! Relays the server's log to the apps, so they see what the terminal shows:
//! Chrome starting, the browser tool installing, Claude's warnings.
//!
//! [`RelayLayer`] is a `tracing` layer: every `info`, `warn` or `error` event
//! from oxbot's own crates (and Claude's stderr) becomes a [`LogLine`] on the
//! [`LogRelay`], which keeps the most recent ones for apps that connect later.

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use oxbot_protocol::LogLine;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// How many recent lines a newly connected app gets.
pub const RECENT_LINES: usize = 200;

/// The relay: live lines plus the most recent ones.
#[derive(Debug)]
pub struct LogRelay {
    live: broadcast::Sender<LogLine>,
    recent: Mutex<VecDeque<LogLine>>,
}

impl LogRelay {
    /// An empty relay.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            live: broadcast::channel(256).0,
            recent: Mutex::new(VecDeque::with_capacity(RECENT_LINES)),
        })
    }

    /// New lines as they're logged.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.live.subscribe()
    }

    /// The most recent lines, oldest first.
    #[must_use]
    pub fn recent(&self) -> Vec<LogLine> {
        self.recent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn push(&self, line: LogLine) {
        {
            let mut recent = self.recent.lock().unwrap_or_else(PoisonError::into_inner);
            if recent.len() == RECENT_LINES {
                recent.pop_front();
            }
            recent.push_back(line.clone());
        }
        let _ = self.live.send(line);
    }
}

/// The `tracing` layer that feeds a [`LogRelay`].
#[derive(Debug, Clone)]
pub struct RelayLayer(pub Arc<LogRelay>);

/// Whether an event belongs in the app's activity log.
fn wanted(level: Level, target: &str) -> bool {
    level <= Level::INFO && (target.starts_with("oxbot") || target == "claude_stderr")
}

impl<S: Subscriber> Layer<S> for RelayLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !wanted(*meta.level(), meta.target()) {
            return;
        }
        let mut text = Text::default();
        event.record(&mut text);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        self.0.push(LogLine {
            level: meta.level().as_str().to_lowercase(),
            target: meta.target().to_owned(),
            message: text.0,
            ts_ms,
        });
    }
}

/// Collects an event's message, then its fields as `key=value`.
#[derive(Default)]
struct Text(String);

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.add(field, format_args!("{value}"));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.add(field, format_args!("{value:?}"));
    }
}

impl Text {
    fn add(&mut self, field: &Field, value: fmt::Arguments<'_>) {
        if field.name() == "message" {
            let rest = std::mem::take(&mut self.0);
            let _ = write!(self.0, "{value}{rest}");
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[test]
    fn oxbot_lines_are_relayed_with_their_fields() {
        let relay = LogRelay::new();
        let subscriber = tracing_subscriber::registry().with(RelayLayer(Arc::clone(&relay)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "oxbot_browser", pid = 42, "Chrome started");
            tracing::debug!(target: "oxbot_browser", "too chatty");
            tracing::info!(target: "hyper", "not ours");
            tracing::warn!(target: "claude_stderr", "rate limited");
        });
        let lines = relay.recent();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "Chrome started pid=42");
        assert_eq!(lines[0].level, "info");
        assert_eq!(lines[1].target, "claude_stderr");
    }

    #[test]
    fn only_the_most_recent_lines_are_kept() {
        let relay = LogRelay::new();
        for i in 0..RECENT_LINES + 5 {
            relay.push(LogLine {
                level: "info".into(),
                target: "oxbot".into(),
                message: i.to_string(),
                ts_ms: 0,
            });
        }
        let lines = relay.recent();
        assert_eq!(lines.len(), RECENT_LINES);
        assert_eq!(lines[0].message, "5");
    }
}
