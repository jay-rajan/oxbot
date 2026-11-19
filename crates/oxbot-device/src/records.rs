//! The phone's copies of the family's Records, so they open in its own
//! apps, even away from home. The home computer says which records this
//! phone keeps (its person's, the whole family's; see the hub's
//! `records_to_keep`); [`sync`] fetches what's new or changed and removes
//! what's no longer this phone's. Each copy keeps its file's name, in a
//! folder of its own (`<dir>/<id>/Report card.pdf`), so the app that opens
//! it sees the right kind of file. `kept.json` lists what's here.
use std::future::Future;
use std::path::{Path, PathBuf};

use oxbot_protocol::KeptRecord;

use crate::DeviceError;

/// What's kept here, beside the copies.
const KEPT: &str = "kept.json";

/// What one [`sync`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// Records fetched (new or changed).
    pub fetched: usize,
    /// Copies removed (no longer this phone's).
    pub removed: usize,
    /// Records that couldn't be fetched this time, with why.
    pub failed: Vec<(i64, String)>,
}

/// Brings the copies in `dir` in line with `list`: fetches (with `fetch`)
/// each record that's new or changed, removes those no longer listed. A
/// record that fails stays as it was and is tried again next time.
///
/// # Errors
/// `dir` can't be written.
pub async fn sync<F, Fut>(dir: &Path, list: &[KeptRecord], mut fetch: F) -> std::io::Result<Synced>
where
    F: FnMut(i64) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, DeviceError>>,
{
    std::fs::create_dir_all(dir)?;
    let mut kept = read_kept(dir);
    let mut done = Synced::default();
    // Gone from the list: the copy goes too.
    kept.retain(|k| {
        let stays = list.iter().any(|r| r.id == k.id);
        if !stays {
            let _ = std::fs::remove_dir_all(dir.join(k.id.to_string()));
            done.removed += 1;
        }
        stays
    });
    for record in list {
        let current = kept.iter().any(|k| {
            k.updated_ms == record.updated_ms
                && k.size == record.size
                && k.name == record.name
                && file_of(dir, k).is_file()
        });
        if current {
            continue;
        }
        match fetch(record.id).await {
            Ok(bytes) => {
                let folder = dir.join(record.id.to_string());
                // Whatever was here before (an older name) goes.
                let _ = std::fs::remove_dir_all(&folder);
                std::fs::create_dir_all(&folder)?;
                let part = folder.join(".part");
                std::fs::write(&part, &bytes)?;
                std::fs::rename(&part, folder.join(safe_name(&record.name)))?;
                kept.retain(|k| k.id != record.id);
                kept.push(record.clone());
                done.fetched += 1;
            }
            Err(err) => done.failed.push((record.id, err.to_string())),
        }
    }
    write_kept(dir, &kept)?;
    Ok(done)
}

/// Record `id`'s copy in `dir`, if this phone has it.
#[must_use]
pub fn copy_of(dir: &Path, id: i64) -> Option<PathBuf> {
    read_kept(dir)
        .iter()
        .find(|k| k.id == id)
        .map(|k| file_of(dir, k))
        .filter(|p| p.is_file())
}

/// Every copy in `dir` gone (the phone was unpaired).
pub fn wipe(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn file_of(dir: &Path, k: &KeptRecord) -> PathBuf {
    dir.join(k.id.to_string()).join(safe_name(&k.name))
}

/// `name` as one file name: no folders, not hidden, never empty.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let base: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .trim_start_matches('.')
        .to_owned();
    if base.is_empty() {
        "record".to_owned()
    } else {
        base
    }
}

fn read_kept(dir: &Path) -> Vec<KeptRecord> {
    std::fs::read(dir.join(KEPT))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_kept(dir: &Path, kept: &[KeptRecord]) -> std::io::Result<()> {
    let part = dir.join(".kept.part");
    std::fs::write(
        &part,
        serde_json::to_vec(kept).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(part, dir.join(KEPT))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test fixtures may panic")]
    use super::*;

    fn rec(id: i64, name: &str, updated_ms: i64) -> KeptRecord {
        KeptRecord {
            id,
            name: name.to_owned(),
            size: 3,
            updated_ms,
        }
    }

    #[tokio::test]
    async fn the_phone_keeps_its_records_and_fetches_only_what_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("records");
        let asked = std::sync::Mutex::new(Vec::new());
        let fetch = |id: i64| {
            asked.lock().unwrap().push(id);
            async move {
                if id == 9 {
                    Err(DeviceError::Refused("not yours".into()))
                } else {
                    Ok(b"pdf".to_vec())
                }
            }
        };
        let list = [rec(1, "Report card.pdf", 10), rec(2, "x-ray.jpg", 10)];
        let done = sync(&dir, &list, fetch).await.unwrap();
        assert_eq!((done.fetched, done.removed), (2, 0));
        let one = copy_of(&dir, 1).unwrap();
        assert!(one.ends_with("1/Report card.pdf"), "{}", one.display());
        assert_eq!(std::fs::read(one).unwrap(), b"pdf");

        // Nothing changed: nothing fetched.
        asked.lock().unwrap().clear();
        let done = sync(&dir, &list, fetch).await.unwrap();
        assert_eq!(done, Synced::default());
        assert!(asked.lock().unwrap().is_empty());

        // One renamed, one no longer this phone's, one that fails.
        let list = [rec(1, "Report card Nov.pdf", 20), rec(9, "secret.pdf", 5)];
        let done = sync(&dir, &list, fetch).await.unwrap();
        assert_eq!((done.fetched, done.removed), (1, 1));
        assert_eq!(done.failed.len(), 1);
        assert!(copy_of(&dir, 2).is_none() && !dir.join("2").exists());
        assert!(copy_of(&dir, 1).unwrap().ends_with("1/Report card Nov.pdf"));
        assert!(!dir.join("1/Report card.pdf").exists());
        assert!(copy_of(&dir, 9).is_none());

        wipe(&dir);
        assert!(copy_of(&dir, 1).is_none());
    }

    #[test]
    fn a_name_is_one_plain_file() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("a\\b.pdf"), "b.pdf");
        assert_eq!(safe_name("  "), "record");
    }
}
