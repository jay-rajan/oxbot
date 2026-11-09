//! Devices: the phones and laptops paired with this home computer.

use oxbot_protocol::DeviceInfo;
use rusqlite::{Connection, OptionalExtension, params};

use super::people::{find_or_create, find_person};
use super::{PersonRecord, Store, StoreError, now_ms};
use crate::id::PersonId;

pub(super) const DEVICE_SELECT: &str =
    "SELECT g.id, COALESCE(p.name, ''), g.name, g.paired_ms, g.last_seen_ms, g.person_id
     FROM devices g LEFT JOIN people p ON p.id = g.person_id";

pub(super) fn device_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(DeviceInfo, String)> {
    Ok((
        DeviceInfo {
            id: row.get(0)?,
            person: row.get(1)?,
            name: row.get(2)?,
            paired_ms: row.get(3)?,
            last_seen_ms: row.get(4)?,
            online: false,
        },
        row.get(5)?,
    ))
}

pub(super) fn insert_device(
    conn: &Connection,
    id: &str,
    person: &PersonRecord,
    name: &str,
    key_hash: &str,
) -> Result<DeviceInfo, StoreError> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO devices (id, person_id, name, key_hash, paired_ms, last_seen_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, person.id.as_str(), name, key_hash, now],
    )?;
    Ok(DeviceInfo {
        id: id.to_owned(),
        person: person.name.clone(),
        name: name.to_owned(),
        paired_ms: now,
        last_seen_ms: now,
        online: false,
    })
}

impl Store {
    /// Remembers a newly paired device for the person called exactly `person`
    /// (made if new, in the same transaction). `key_hash` is the hash of its
    /// key. The flag says whether the person is new.
    ///
    /// # Errors
    /// Fails if the database does (a key already used, say).
    pub fn add_device(
        &self,
        id: &str,
        person: &str,
        name: &str,
        key_hash: &str,
    ) -> Result<(DeviceInfo, bool), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (found, new) = find_or_create(&tx, person)?;
        let device = insert_device(&tx, id, &found, name, key_hash)?;
        tx.commit()?;
        Ok((device, new))
    }

    /// Remembers a newly paired device for the person `person`.
    ///
    /// # Errors
    /// Fails if there's no such person or the database fails.
    pub fn add_device_for(
        &self,
        id: &str,
        person: &PersonId,
        name: &str,
        key_hash: &str,
    ) -> Result<DeviceInfo, StoreError> {
        let conn = self.conn();
        let found = find_person(&conn, "id", person.as_str())?
            .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
        insert_device(&conn, id, &found, name, key_hash)
    }

    /// The device holding the key with this hash, if it's still paired; marks
    /// it as seen now.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn device_by_key(&self, key_hash: &str) -> Result<Option<DeviceInfo>, StoreError> {
        Ok(self
            .device_and_person_by_key(key_hash)?
            .map(|(device, _)| device))
    }

    /// Like [`Store::device_by_key`], with the id of the device's person.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn device_and_person_by_key(
        &self,
        key_hash: &str,
    ) -> Result<Option<(DeviceInfo, PersonId)>, StoreError> {
        let conn = self.conn();
        let now = now_ms();
        conn.execute(
            "UPDATE devices SET last_seen_ms = ?2 WHERE key_hash = ?1",
            params![key_hash, now],
        )?;
        let found = conn
            .query_row(
                &format!("{DEVICE_SELECT} WHERE g.key_hash = ?1"),
                params![key_hash],
                device_row,
            )
            .optional()?;
        Ok(found.and_then(|(device, person)| Some((device, PersonId::new(person).ok()?))))
    }

    /// Every paired device, oldest first.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{DEVICE_SELECT} ORDER BY g.paired_ms, g.id"))?;
        let devices = stmt
            .query_map([], device_row)?
            .map(|r| r.map(|(device, _)| device))
            .collect::<Result<_, _>>()?;
        Ok(devices)
    }

    /// Forgets a device; its key stops working. Returns whether it was paired.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn remove_device(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM devices WHERE id = ?1", params![id])?
            > 0)
    }
}
