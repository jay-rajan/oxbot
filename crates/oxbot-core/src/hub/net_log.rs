//! Settings → Activity: writing down each exchange with the outside world
//! (bots' visits to websites, devices connecting, oxbot's own calls; the
//! relay writes questions to the AI itself), and showing each person the
//! rows that are theirs.
//!
//! Who sees what: the owner sees every row. Anyone else sees the rows of
//! their own bots (questions and visits), of their own devices, and
//! questions to the AI made for them. Kid accounts see none.

use std::collections::HashMap;
use std::sync::Arc;

use oxbot_browser::{Visit, VisitSink};
use oxbot_protocol::{DEFAULT_NET_ROWS, MAX_NET_ROWS, NetKind, NetRow, ServerMsg};

use super::{Hub, HubError};
use crate::bot::Viewer;
use crate::id::{BotId, BrowserOwner, PersonId};
use crate::store::{NetDetail, NetRecord, NetScope, Role, now_ms};

/// How often old rows are deleted (at start, then this often).
const PRUNE_EVERY_MS: i64 = 86_400_000;
/// The outcome of a device still connected.
pub const CONNECTED: &str = "connected";

/// A device's connection under way, for [`Hub::device_disconnected`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceVisit {
    /// Its row.
    pub id: i64,
    /// When it connected (ms since the Unix epoch).
    pub started_ms: i64,
}

impl Hub {
    /// Writes `rec` on a blocking thread, so the caller never waits for
    /// the database.
    fn log_net(&self, rec: NetRecord) {
        let store = Arc::clone(&self.store);
        let write = move || {
            if let Err(err) = store.add_net(&rec) {
                tracing::warn!(%err, "couldn't note an exchange for Settings → Activity");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(write)),
            Err(_) => write(),
        }
    }

    /// Where `owner`'s browser sends its finished visits.
    pub(super) fn visit_sink(&self, owner: &BrowserOwner) -> VisitSink {
        let hub = self.me.clone();
        let person = match owner {
            BrowserOwner::Home => None,
            BrowserOwner::Person(id) => Some(id.to_string()),
        };
        Arc::new(move |visit: Visit| {
            if let Some(hub) = hub.upgrade() {
                hub.log_net(visit_record(&visit, person.clone()));
            }
        })
    }

    /// oxbot itself called `host` (the models list, the AI account):
    /// `label` says what, in plain words.
    pub fn note_oxbot_call(&self, call: OxbotCall) {
        self.log_net(NetRecord {
            who: "oxbot".to_owned(),
            host: call.host,
            label: call.label,
            bytes_up: call.bytes_up,
            bytes_down: call.bytes_down,
            detail: NetDetail {
                ms: call.ms,
                outcome: call.outcome,
                ..NetDetail::default()
            },
            ..NetRecord::new(NetKind::Oxbot, call.ts_ms)
        });
    }

    /// A device connected to the home computer over `network`: its row,
    /// marked [`CONNECTED`] until [`Hub::device_disconnected`]. `None` if
    /// it couldn't be written.
    pub fn device_connected(
        &self,
        device: &oxbot_protocol::DeviceInfo,
        person: Option<&PersonId>,
        network: &str,
    ) -> Option<DeviceVisit> {
        let started_ms = now_ms();
        let rec = NetRecord {
            who: device.name.clone(),
            device: Some(device.id.clone()),
            person: person.map(ToString::to_string),
            host: network.to_owned(),
            label: if network.is_empty() {
                "connected".to_owned()
            } else {
                format!("connected on {network}")
            },
            detail: NetDetail {
                outcome: CONNECTED.to_owned(),
                ..NetDetail::default()
            },
            ..NetRecord::new(NetKind::Device, started_ms)
        };
        match self.store.add_net(&rec) {
            Ok(id) => Some(DeviceVisit { id, started_ms }),
            Err(err) => {
                tracing::warn!(%err, "couldn't note a device connecting");
                None
            }
        }
    }

    /// The device's connection ended, after sending `bytes_up` to the home
    /// computer and getting `bytes_down` from it.
    pub fn device_disconnected(&self, visit: &DeviceVisit, bytes_up: u64, bytes_down: u64) {
        let ended = now_ms();
        let detail = NetDetail {
            ms: u64::try_from(ended - visit.started_ms).unwrap_or(0),
            outcome: "ok".to_owned(),
            ended_ms: Some(ended),
            ..NetDetail::default()
        };
        let store = Arc::clone(&self.store);
        let id = visit.id;
        let write = move || {
            if let Err(err) = store.finish_net(id, bytes_up, bytes_down, &detail) {
                tracing::warn!(%err, "couldn't note a device disconnecting");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(write)),
            Err(_) => write(),
        }
    }

    /// Deletes rows older than 30 days, at most once a day; at start, also
    /// ends devices an earlier oxbot left connected.
    pub(super) fn prune_net_log(&self, starting: bool) {
        use std::sync::atomic::Ordering;
        let now = now_ms();
        if !starting && now - self.net_pruned.load(Ordering::Relaxed) < PRUNE_EVERY_MS {
            return;
        }
        self.net_pruned.store(now, Ordering::Relaxed);
        if starting && let Err(err) = self.store.end_open_net(CONNECTED) {
            tracing::warn!(%err, "couldn't end devices left connected");
        }
        match self.store.prune_net_log(now) {
            Ok(0) => {}
            Ok(n) => tracing::info!(n, "old activity rows deleted"),
            Err(err) => tracing::warn!(%err, "couldn't delete old activity rows"),
        }
    }

    /// Which rows `viewer` may see.
    fn net_scope(&self, viewer: &Viewer) -> NetScope {
        if self.is_owner_viewer(viewer) {
            return NetScope::All;
        }
        let viewer = self.resolve(viewer);
        let Some(me) = viewer.id.clone() else {
            return NetScope::Nothing;
        };
        if self.role_of(&me) == Role::Kid {
            return NetScope::Nothing;
        }
        let assistant = self
            .store
            .person_by_id(&me)
            .ok()
            .flatten()
            .map(|p| p.assistant.to_string());
        let bots = self
            .bots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|b| {
                b.owner.as_ref() == Some(&me) || assistant.as_deref() == Some(b.id.as_str())
            })
            .map(|b| b.id.to_string())
            .collect();
        NetScope::Person {
            id: me.to_string(),
            bots,
        }
    }

    /// Settings → Activity for `viewer`: the newest rows they may see (of
    /// `kind`, older than `before_ms`), and today's sums.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn net_log_for(
        &self,
        viewer: &Viewer,
        kind: Option<NetKind>,
        before_ms: Option<i64>,
        limit: Option<u32>,
    ) -> Result<ServerMsg, HubError> {
        let scope = self.net_scope(viewer);
        let limit = limit.unwrap_or(DEFAULT_NET_ROWS).clamp(1, MAX_NET_ROWS);
        let (records, more) = self.store.net_log(&scope, kind, before_ms, limit)?;
        let totals = self.store.net_totals(&scope, kind, local_midnight_ms())?;
        let names = self.person_names();
        let models = self.model_names();
        let rows = records
            .into_iter()
            .map(|r| self.net_row(r, &names, &models))
            .collect();
        Ok(ServerMsg::NetLog { rows, totals, more })
    }

    /// Each model's name as people know it, by id.
    fn model_names(&self) -> HashMap<String, String> {
        self.loop_engine
            .as_ref()
            .map(|e| {
                e.catalog()
                    .get()
                    .models
                    .iter()
                    .map(|m| (m.id.clone(), m.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A stored row as clients get it: names as they are now.
    fn net_row(
        &self,
        r: NetRecord,
        names: &HashMap<PersonId, String>,
        models: &HashMap<String, String>,
    ) -> NetRow {
        let who = r
            .bot
            .as_deref()
            .and_then(|b| BotId::new(b).ok())
            .and_then(|b| self.bot(b.as_str()).ok())
            .map_or(r.who, |b| b.name);
        let person = r
            .person
            .as_deref()
            .and_then(|p| PersonId::new(p).ok())
            .and_then(|p| names.get(&p).cloned());
        let label = match (r.kind, r.detail.model.as_deref()) {
            (NetKind::Ai, Some(model)) => models.get(model).cloned().unwrap_or(r.label),
            _ => r.label,
        };
        NetRow {
            id: r.id,
            ts_ms: r.ts_ms,
            kind: r.kind,
            who,
            bot: r.bot,
            person,
            host: r.host,
            label,
            bytes_up: r.bytes_up,
            bytes_down: r.bytes_down,
            cost_usd: r.cost_usd,
            swapped: r.detail.swapped,
            ms: r.detail.ms,
            outcome: r.detail.outcome,
            other_hosts: r.detail.other_hosts.unwrap_or(0),
        }
    }
}

/// One of oxbot's own calls to the outside.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OxbotCall {
    /// When it started (ms since the Unix epoch).
    pub ts_ms: i64,
    /// Where it went, e.g. `openrouter.ai`.
    pub host: String,
    /// What it was, in plain words, e.g. `checked the free models list`.
    pub label: String,
    /// Bytes sent.
    pub bytes_up: u64,
    /// Bytes received.
    pub bytes_down: u64,
    /// How long it took.
    pub ms: u64,
    /// `ok`, or a short plain reason.
    pub outcome: String,
}

/// A finished visit as a row: a bot's (by id; its name is looked up when
/// shown), on `person`'s browser if it's theirs.
fn visit_record(visit: &Visit, person: Option<String>) -> NetRecord {
    let mut rec = NetRecord::new(oxbot_protocol::NetKind::Website, visit.started_ms);
    rec.who = visit
        .user
        .clone()
        .unwrap_or_else(|| "sign-in window".to_owned());
    rec.bot.clone_from(&visit.user);
    rec.person = person;
    rec.host.clone_from(&visit.site);
    rec.label = match visit.pages {
        0 => "kept loading".to_owned(),
        1 => "read 1 page".to_owned(),
        n => format!("read {n} pages"),
    };
    rec.bytes_up = visit.bytes_up;
    rec.bytes_down = visit.bytes_down;
    rec.detail = NetDetail {
        ms: u64::try_from(visit.last_ms - visit.started_ms).unwrap_or(0),
        outcome: "ok".to_owned(),
        pages: Some(visit.pages),
        other_hosts: Some(visit.other_sites),
        ..NetDetail::default()
    };
    rec
}

/// Today's local midnight, ms since the Unix epoch.
fn local_midnight_ms() -> i64 {
    use chrono::{Local, TimeZone as _};
    let today = Local::now().date_naive();
    today
        .and_hms_opt(0, 0, 0)
        .and_then(|t| Local.from_local_datetime(&t).earliest())
        .map_or(0, |t| t.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visit_is_a_website_row_in_plain_words() {
        let visit = Visit {
            user: Some("scout".into()),
            site: "loblaws.ca".into(),
            started_ms: 1_000,
            last_ms: 4_000,
            pages: 2,
            bytes_up: 300,
            bytes_down: 9_000,
            other_sites: 3,
        };
        let rec = visit_record(&visit, Some("p1".into()));
        assert_eq!(rec.kind, NetKind::Website);
        assert_eq!(rec.bot.as_deref(), Some("scout"));
        assert_eq!(rec.person.as_deref(), Some("p1"));
        assert_eq!(
            (rec.host.as_str(), rec.label.as_str()),
            ("loblaws.ca", "read 2 pages")
        );
        assert_eq!((rec.bytes_up, rec.bytes_down), (300, 9_000));
        assert_eq!(rec.detail.ms, 3_000);
        assert_eq!(rec.detail.other_hosts, Some(3));
        let lone = visit_record(
            &Visit {
                pages: 1,
                user: None,
                ..visit
            },
            None,
        );
        assert_eq!(lone.label, "read 1 page");
        assert_eq!(lone.bot, None);
    }

    fn rows(msg: ServerMsg) -> Vec<NetRow> {
        match msg {
            ServerMsg::NetLog { rows, .. } => rows,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_owner_sees_all_a_person_their_own_and_a_kid_nothing() {
        use crate::HubConfig;
        use crate::bot::Bot;
        use crate::store::Store;
        let dir = tempfile::tempdir().unwrap();
        let bots = dir.path().join("bots");
        std::fs::create_dir_all(&bots).unwrap();
        let store = Store::open_in_memory().unwrap();
        let sam = store.find_or_create_person("Sam").unwrap().0.id;
        let kid = store.find_or_create_person("Kit").unwrap().0.id;
        store.start_profile(&kid, Role::Kid, false).unwrap();
        store.set_role(&kid, Role::Kid).unwrap();
        let mut shop = Bot::create(&bots, BotId::new("shop").unwrap(), "Shop", "").unwrap();
        shop.owner = Some(sam.clone());
        shop.save().unwrap();
        let mut kits = Bot::create(&bots, BotId::new("kits").unwrap(), "Kit's", "").unwrap();
        kits.owner = Some(kid.clone());
        kits.save().unwrap();
        let home = Bot::create(&bots, BotId::new("home").unwrap(), "Home", "").unwrap();
        let hub = Hub::new(vec![shop, kits, home], store, HubConfig::new(&bots));
        let jay = hub.owner_person("Jay").unwrap();
        let add = |kind, bot: Option<&str>, person: Option<&PersonId>| {
            let mut rec = NetRecord::new(kind, now_ms());
            rec.bot = bot.map(str::to_owned);
            rec.who = bot.unwrap_or("oxbot").to_owned();
            rec.person = person.map(ToString::to_string);
            rec.host = "example.com".into();
            rec.bytes_up = 1;
            hub.store.add_net(&rec).unwrap();
        };
        add(NetKind::Website, Some("shop"), None);
        add(NetKind::Ai, Some("home"), Some(&sam));
        add(NetKind::Website, Some("home"), Some(&sam));
        add(NetKind::Device, None, Some(&sam));
        add(NetKind::Ai, Some("kits"), Some(&kid));
        add(NetKind::Oxbot, None, None);
        add(NetKind::Device, None, Some(&jay.id));
        let home_computer = rows(
            hub.net_log_for(&Viewer::home_computer("Jay"), None, None, None)
                .unwrap(),
        );
        assert_eq!(home_computer.len(), 7);
        let as_jay = Viewer::person(jay.id.clone(), "Jay", false);
        assert_eq!(
            rows(hub.net_log_for(&as_jay, None, None, None).unwrap()).len(),
            7
        );
        let as_sam = Viewer::person(sam.clone(), "Sam", false);
        let sams = rows(hub.net_log_for(&as_sam, None, None, None).unwrap());
        let mut seen: Vec<(NetKind, String)> =
            sams.iter().map(|r| (r.kind, r.who.clone())).collect();
        seen.sort_by_key(|(k, w)| (k.as_str(), w.clone()));
        assert_eq!(
            seen,
            [
                (NetKind::Ai, "Home".to_owned()),
                (NetKind::Device, "oxbot".to_owned()),
                (NetKind::Website, "Shop".to_owned()),
            ]
        );
        assert!(
            sams.iter()
                .filter(|r| r.kind == NetKind::Ai)
                .all(|r| r.person.as_deref() == Some("Sam"))
        );
        let as_kid = Viewer::person(kid, "Kit", false);
        let msg = hub.net_log_for(&as_kid, None, None, None).unwrap();
        let ServerMsg::NetLog { rows, totals, more } = msg else {
            panic!()
        };
        assert!(rows.is_empty() && !more && totals.today_rows == 0);
        // Only websites, one at a time.
        let ServerMsg::NetLog { rows, more, totals } = hub
            .net_log_for(
                &Viewer::home_computer("Jay"),
                Some(NetKind::Website),
                None,
                Some(1),
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert!(more);
        assert_eq!(totals.today_rows, 2);
    }

    #[test]
    fn today_starts_at_local_midnight() {
        let midnight = local_midnight_ms();
        let now = now_ms();
        assert!(midnight <= now && now - midnight < 25 * 3_600_000);
    }
}
