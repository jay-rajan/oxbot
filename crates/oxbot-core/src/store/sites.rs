//! Sites: what each browser signed in to.

use oxbot_protocol::SiteInfo;
use rusqlite::params;

use super::{Store, StoreError, now_ms};

impl Store {
    /// Remembers that `owner`'s browser (as [`BrowserOwner::key`] names it)
    /// signed in to `site` (a main domain) now.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_site(&self, owner: impl AsRef<str>, site: &str) -> Result<(), StoreError> {
        self.add_site_at(owner, site, now_ms())
    }

    /// Remembers a sign-in to `site` at `when_ms`; a later one wins.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_site_at(
        &self,
        owner: impl AsRef<str>,
        site: &str,
        when_ms: i64,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO sites (owner, site, signed_in_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner, site) DO UPDATE SET signed_in_ms = MAX(signed_in_ms, excluded.signed_in_ms)",
            params![owner.as_ref(), site, when_ms],
        )?;
        Ok(())
    }

    /// Forgets `site` for `owner`'s browser. Returns whether it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn remove_site(&self, owner: impl AsRef<str>, site: &str) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM sites WHERE owner = ?1 AND site = ?2",
            params![owner.as_ref(), site],
        )? > 0)
    }

    /// Forgets every site for `owner`'s browser, in one go. Returns how
    /// many there were.
    ///
    /// # Errors
    /// Fails if SQLite fails (then none are forgotten).
    pub fn clear_sites(&self, owner: impl AsRef<str>) -> Result<usize, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM sites WHERE owner = ?1", [owner.as_ref()])?)
    }

    /// Forgets `owner`'s sites that aren't in `held` (the sites its browser
    /// still has cookies for), in one transaction. Returns the ones
    /// forgotten.
    ///
    /// # Errors
    /// Fails if SQLite fails (then none are forgotten).
    pub fn keep_only_sites(
        &self,
        owner: impl AsRef<str>,
        held: &[String],
    ) -> Result<Vec<String>, StoreError> {
        let owner = owner.as_ref();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let known: Vec<String> = {
            let mut stmt = tx.prepare("SELECT site FROM sites WHERE owner = ?1")?;
            let rows = stmt.query_map([owner], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let gone: Vec<String> = known.into_iter().filter(|s| !held.contains(s)).collect();
        for site in &gone {
            tx.execute(
                "DELETE FROM sites WHERE owner = ?1 AND site = ?2",
                params![owner, site],
            )?;
        }
        tx.commit()?;
        Ok(gone)
    }

    /// The sites `owner`'s browser signed in to, newest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn sites(&self, owner: impl AsRef<str>) -> Result<Vec<SiteInfo>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT site, signed_in_ms FROM sites WHERE owner = ?1
             ORDER BY signed_in_ms DESC, site",
        )?;
        let rows = stmt.query_map([owner.as_ref()], |r| {
            Ok(SiteInfo {
                site: r.get(0)?,
                signed_in_ms: r.get(1)?,
                icon: None,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every browser owner with remembered sites (home's is empty).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_owners(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT owner FROM sites ORDER BY owner")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Whether `owner`'s sites should be checked against its cookies (they
    /// were renamed by an upgrade and Chrome wasn't asked yet).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_check_due(&self, owner: impl AsRef<str>) -> Result<bool, StoreError> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM site_checks WHERE owner = ?1)",
            [owner.as_ref()],
            |r| r.get(0),
        )?)
    }

    /// `owner`'s sites were checked against its cookies.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_check_done(&self, owner: impl AsRef<str>) -> Result<(), StoreError> {
        self.conn()
            .execute("DELETE FROM site_checks WHERE owner = ?1", [owner.as_ref()])?;
        Ok(())
    }
}
