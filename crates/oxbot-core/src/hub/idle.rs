//! Whether the home is quiet enough to restart oxbot (for an update): no
//! bot working, no request or sign-in waiting for someone, no schedule
//! about to run.

use std::sync::PoisonError;

use super::Hub;

/// How long before a schedule runs that an update holds off.
pub const SCHEDULE_MARGIN_MS: i64 = 15 * 60 * 1000;

impl Hub {
    /// Why oxbot shouldn't restart at `now_ms`, in plain words; `None` when
    /// it's quiet.
    pub async fn restart_waits_for(&self, now_ms: i64) -> Option<String> {
        let busy: Vec<String> = {
            let runtimes = self.runtimes.lock().await;
            let bots = self.bots.read().unwrap_or_else(PoisonError::into_inner);
            runtimes
                .iter()
                .filter(|(_, r)| r.busy)
                .map(|(id, _)| bots.get(id).map_or_else(|| id.to_string(), |b| b.name.clone()))
                .collect()
        };
        if let Some(first) = busy.first() {
            return Some(match busy.len() {
                1 => format!("{first} is working"),
                n => format!("{first} and {} more are working", n - 1),
            });
        }
        if !self.pending_approvals().is_empty() {
            return Some("a bot is waiting for an answer".to_owned());
        }
        if !self.pending_sign_ins().is_empty() {
            return Some("a bot is waiting for a sign-in".to_owned());
        }
        match self.store.next_active_run(now_ms) {
            Ok(Some(next)) if next - now_ms < SCHEDULE_MARGIN_MS => {
                Some("a schedule is about to run".to_owned())
            }
            Ok(_) => None,
            // Can't tell: wait rather than cut a scheduled run short.
            Err(err) => {
                tracing::warn!(%err, "couldn't read the next scheduled run");
                Some("oxbot couldn't read its schedules".to_owned())
            }
        }
    }
}
