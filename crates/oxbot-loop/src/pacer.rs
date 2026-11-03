//! The pacer: free model requests across the whole household stay under
//! OpenRouter's limits (20 a minute, and the free daily budget its key
//! info reports). Waiting requests are served in turn, bot by bot, so one
//! busy bot can't starve the others. Paid requests don't wait here.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use oxbot_llm::{KeyInfo, LlmError};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::meter::{now_ms, utc_day};

/// Free requests per rolling minute (OpenRouter allows 20; one is kept
/// for clock skew).
pub const FREE_PER_MINUTE: usize = 19;
/// Key info is read again after this many free requests.
const REFRESH_EVERY: u32 = 10;

/// Which queue a request waits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lane {
    /// A real task: served first.
    #[default]
    Live,
    /// A practice run: served when no real task waits.
    Trial,
}

/// Why the pacer won't send a free request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacerStop {
    /// The turn was stopped while waiting.
    Cancelled,
    /// OpenRouter's free daily requests are used up.
    DailyUsedUp,
}

/// Reads the key's limits (OpenRouter's `GET /key`).
pub trait Account: Send + Sync {
    /// The key's limits and today's use.
    fn key_info(&self) -> BoxFuture<'_, Result<KeyInfo, LlmError>>;
}

impl Account for oxbot_llm::OpenAiClient {
    fn key_info(&self) -> BoxFuture<'_, Result<KeyInfo, LlmError>> {
        Box::pin(oxbot_llm::OpenAiClient::key_info(self))
    }
}

#[derive(Default)]
struct State {
    /// When the free requests of the last minute went out.
    sent: VecDeque<Instant>,
    /// Waiting requests by ticket: (bot, lane).
    waiting: BTreeMap<u64, (String, Lane)>,
    next_ticket: u64,
    /// When each bot was last served (a counter), for taking turns.
    served: HashMap<String, u64>,
    serial: u64,
    /// Free requests left today when the key was last read, and the day.
    remaining: Option<(u32, String)>,
    /// Free requests sent since then.
    since_refresh: u32,
    /// The UTC day free requests were stopped for.
    stopped: Option<String>,
    /// Last key info read.
    key: Option<KeyInfo>,
}

/// One per household, shared by every loop session.
pub struct Pacer {
    state: Mutex<State>,
    notify: Notify,
    per_minute: usize,
    account: Option<Arc<dyn Account>>,
    me: std::sync::Weak<Pacer>,
}

impl std::fmt::Debug for Pacer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pacer")
            .field("per_minute", &self.per_minute)
            .finish_non_exhaustive()
    }
}

impl Pacer {
    /// A pacer at OpenRouter's free limits, reading the key's limits from
    /// `account` when there is one.
    #[must_use]
    pub fn new(account: Option<Arc<dyn Account>>) -> Arc<Self> {
        Self::with_rate(FREE_PER_MINUTE, account)
    }

    /// A pacer allowing `per_minute` free requests a minute.
    #[must_use]
    pub fn with_rate(per_minute: usize, account: Option<Arc<dyn Account>>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            state: Mutex::new(State::default()),
            notify: Notify::new(),
            per_minute: per_minute.max(1),
            account,
            me: me.clone(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for `bot`'s turn to send one free request. Returns once it may.
    ///
    /// # Errors
    /// [`PacerStop::Cancelled`] when `cancel` fires while waiting,
    /// [`PacerStop::DailyUsedUp`] when today's free requests are used up.
    pub async fn acquire(
        &self,
        bot: &str,
        lane: Lane,
        cancel: &CancellationToken,
    ) -> Result<(), PacerStop> {
        let ticket = {
            let mut s = self.state();
            let t = s.next_ticket;
            s.next_ticket += 1;
            s.waiting.insert(t, (bot.to_owned(), lane));
            t
        };
        let _leave = Leave {
            pacer: self,
            ticket,
        };
        let refresh = loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let wait_until = {
                let mut s = self.state();
                if Self::used_up(&mut s) {
                    return Err(PacerStop::DailyUsedUp);
                }
                let now = Instant::now();
                while s
                    .sent
                    .front()
                    .is_some_and(|t| now.duration_since(*t) >= Duration::from_mins(1))
                {
                    s.sent.pop_front();
                }
                let full = s.sent.len() >= self.per_minute;
                if !full && next_ticket(&s) == Some(ticket) {
                    s.sent.push_back(now);
                    s.serial += 1;
                    let serial = s.serial;
                    s.served.insert(bot.to_owned(), serial);
                    s.waiting.remove(&ticket);
                    s.since_refresh += 1;
                    break s.since_refresh.is_multiple_of(REFRESH_EVERY) || s.remaining.is_none();
                }
                full.then(|| s.sent.front().map(|t| *t + Duration::from_mins(1)))
                    .flatten()
            };
            let sleep = async {
                match wait_until {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = &mut notified => {}
                () = sleep => {}
                () = cancel.cancelled() => return Err(PacerStop::Cancelled),
            }
        };
        // Others may go now.
        self.notify.notify_waiters();
        if refresh {
            self.refresh_soon();
        }
        Ok(())
    }

    /// Whether free requests are out for today (and forgets yesterday's
    /// stop).
    fn used_up(s: &mut State) -> bool {
        let today = utc_day(now_ms());
        if s.stopped.as_ref().is_some_and(|d| *d != today) {
            s.stopped = None;
        }
        if s.remaining.as_ref().is_some_and(|(_, d)| *d != today) {
            s.remaining = None;
            s.since_refresh = 0;
        }
        s.stopped.is_some()
            || s.remaining
                .as_ref()
                .is_some_and(|(left, _)| *left <= s.since_refresh)
    }

    /// Free requests are used up until the next UTC day (OpenRouter said
    /// so).
    pub fn stop_free_today(&self) {
        self.state().stopped = Some(utc_day(now_ms()));
        self.notify.notify_waiters();
    }

    /// Whether free requests are out for today.
    #[must_use]
    pub fn free_used_up(&self) -> bool {
        Self::used_up(&mut self.state())
    }

    /// Free requests left today, when known.
    #[must_use]
    pub fn free_left(&self) -> Option<u32> {
        let s = self.state();
        s.remaining
            .as_ref()
            .map(|(left, _)| left.saturating_sub(s.since_refresh))
    }

    /// The key's limits as last read.
    #[must_use]
    pub fn key_info(&self) -> Option<KeyInfo> {
        self.state().key.clone()
    }

    /// New key info: today's free budget starts from what it says.
    pub fn note_key(&self, info: KeyInfo) {
        let mut s = self.state();
        if let Some(q) = info.free_daily {
            s.remaining = Some((q.remaining, utc_day(now_ms())));
            s.since_refresh = 0;
        }
        s.key = Some(info);
        drop(s);
        self.notify.notify_waiters();
    }

    /// Reads the key's limits again in the background (after a 429, and
    /// every few requests).
    pub fn refresh_soon(&self) {
        let Some(account) = self.account.clone() else {
            return;
        };
        let me = self.me.clone();
        tokio::spawn(async move {
            match account.key_info().await {
                Ok(info) => {
                    if let Some(pacer) = me.upgrade() {
                        pacer.note_key(info);
                    }
                }
                Err(err) => tracing::info!(%err, "couldn't read OpenRouter key info"),
            }
        });
    }
}

/// The ticket served next: live before practice runs, then the bot served
/// longest ago (never served first), then the oldest ticket.
fn next_ticket(s: &State) -> Option<u64> {
    s.waiting
        .iter()
        .min_by_key(|(ticket, (bot, lane))| {
            (
                *lane == Lane::Trial,
                s.served.get(bot).copied().unwrap_or(0),
                **ticket,
            )
        })
        .map(|(t, _)| *t)
}

/// Takes a ticket out of the queue when its request stops waiting.
struct Leave<'a> {
    pacer: &'a Pacer,
    ticket: u64,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        let removed = self.pacer.state().waiting.remove(&self.ticket).is_some();
        if removed {
            self.pacer.notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_llm::Quota;

    #[tokio::test(start_paused = true)]
    async fn at_most_the_rate_per_minute() {
        let pacer = Pacer::with_rate(3, None);
        let cancel = CancellationToken::new();
        let start = Instant::now();
        for _ in 0..3 {
            pacer.acquire("a", Lane::Live, &cancel).await.unwrap();
        }
        assert!(start.elapsed() < Duration::from_secs(1));
        pacer.acquire("a", Lane::Live, &cancel).await.unwrap();
        assert!(start.elapsed() >= Duration::from_mins(1));
    }

    #[tokio::test(start_paused = true)]
    async fn bots_take_turns() {
        let pacer = Pacer::with_rate(1, None);
        let cancel = CancellationToken::new();
        pacer.acquire("busy", Lane::Live, &cancel).await.unwrap();
        // The busy bot queues three more, then a quiet bot one.
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for bot in ["busy", "busy", "busy", "quiet"] {
            let pacer = Arc::clone(&pacer);
            let order = Arc::clone(&order);
            let cancel = cancel.clone();
            tasks.push(tokio::spawn(async move {
                pacer.acquire(bot, Lane::Live, &cancel).await.unwrap();
                order.lock().unwrap().push(bot);
            }));
            tokio::task::yield_now().await;
        }
        for t in tasks {
            t.await.unwrap();
        }
        let order = order.lock().unwrap().clone();
        assert_eq!(order[0], "quiet", "{order:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_can_be_cancelled() {
        let pacer = Pacer::with_rate(1, None);
        let cancel = CancellationToken::new();
        pacer.acquire("a", Lane::Live, &cancel).await.unwrap();
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            stop.cancel();
        });
        assert_eq!(
            pacer.acquire("a", Lane::Live, &cancel).await,
            Err(PacerStop::Cancelled)
        );
        // The cancelled ticket left the queue: another bot goes next minute.
        let fresh = CancellationToken::new();
        pacer.acquire("b", Lane::Live, &fresh).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_daily_budget_comes_from_the_key() {
        let pacer = Pacer::with_rate(100, None);
        pacer.note_key(KeyInfo {
            free_daily: Some(Quota {
                used: 998,
                limit: 1000,
                remaining: 2,
            }),
            ..KeyInfo::default()
        });
        let cancel = CancellationToken::new();
        pacer.acquire("a", Lane::Live, &cancel).await.unwrap();
        assert_eq!(pacer.free_left(), Some(1));
        pacer.acquire("a", Lane::Live, &cancel).await.unwrap();
        assert_eq!(
            pacer.acquire("a", Lane::Live, &cancel).await,
            Err(PacerStop::DailyUsedUp)
        );
        assert!(pacer.free_used_up());
    }

    #[tokio::test(start_paused = true)]
    async fn a_daily_limit_stops_free_requests_for_the_day() {
        let pacer = Pacer::with_rate(100, None);
        pacer.stop_free_today();
        assert_eq!(
            pacer
                .acquire("a", Lane::Live, &CancellationToken::new())
                .await,
            Err(PacerStop::DailyUsedUp)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn live_tasks_go_before_practice_runs() {
        let pacer = Pacer::with_rate(1, None);
        let cancel = CancellationToken::new();
        pacer.acquire("x", Lane::Live, &cancel).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for (bot, lane) in [("t", Lane::Trial), ("l", Lane::Live)] {
            let pacer = Arc::clone(&pacer);
            let order = Arc::clone(&order);
            let cancel = cancel.clone();
            tasks.push(tokio::spawn(async move {
                pacer.acquire(bot, lane, &cancel).await.unwrap();
                order.lock().unwrap().push(bot);
            }));
            tokio::task::yield_now().await;
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec!["l", "t"]);
    }
}
