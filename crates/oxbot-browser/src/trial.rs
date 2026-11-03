//! Practice-run browser profiles: a copy of a signed-in profile without
//! its history, made in an instant on APFS (`cp -c`: copy-on-write, no
//! extra disk until written), and deleting what a run left behind.
//!
//! A practice run never uses a live profile, so nothing a site rewrites
//! during it (cookies, storage) flows back, and the whole sandbox can be
//! killed and deleted at once.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::BrowserError;

/// What a cloned profile leaves out, by name (an entry is left out if its
/// name starts with one of these): browsing history, open tabs and
/// sessions, icons, caches, service workers, the downloads list, and
/// Chrome's own locks (a copied lock would make the copy look in use).
/// Sign-ins (cookies, local storage, databases) stay.
pub const NOT_CLONED: &[&str] = &[
    "History",
    "Visited Links",
    "Top Sites",
    "Sessions",
    "Current Session",
    "Current Tabs",
    "Last Session",
    "Last Tabs",
    "Favicons",
    "Cache",
    "Code Cache",
    "GPUCache",
    "GrShaderCache",
    "GraphiteDawnCache",
    "ShaderCache",
    "DawnCache",
    "Service Worker",
    "Download",
    "Shortcuts",
    "Network Action Predictor",
    "Singleton",
    "DevToolsActivePort",
    "Crashpad",
    "BrowserMetrics",
];

/// Whether a profile entry called `name` is left out of a clone.
#[must_use]
pub fn left_out(name: &str) -> bool {
    NOT_CLONED.iter().any(|prefix| name.starts_with(prefix))
}

/// Whether a folder in a profile's top level is one of Chrome's own
/// profiles (`Default`, `Profile 1`, `Guest Profile`…), whose entries are
/// cloned one by one so history inside them can be left out.
fn is_profile_folder(name: &str) -> bool {
    name == "Default" || name.starts_with("Profile ") || name.ends_with(" Profile")
}

/// Copies the Chrome profile `from` to `to` (which must not exist yet)
/// for a practice run, without [`NOT_CLONED`]. On APFS each file is a
/// copy-on-write clone; elsewhere a plain copy. The copy is private to this
/// user (0700) like the original.
///
/// # Errors
/// Fails if a folder can't be made or `cp` fails.
pub async fn clone_profile(from: &Path, to: &Path) -> Result<(), BrowserError> {
    make_private_dir(to)?;
    if !from.is_dir() {
        // Nothing signed in yet: an empty profile.
        return Ok(());
    }
    let mut top = Vec::new();
    for entry in read_dir(from)? {
        let Some(name) = entry
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        if left_out(&name) {
            continue;
        }
        if entry.is_dir() && is_profile_folder(&name) {
            let inner = to.join(&name);
            make_private_dir(&inner)?;
            let parts: Vec<PathBuf> = read_dir(&entry)?
                .into_iter()
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !left_out(n))
                })
                .collect();
            copy(&parts, &inner).await?;
        } else {
            top.push(entry);
        }
    }
    copy(&top, to).await
}

/// Deletes everything in `root` (practice runs left behind by an oxbot
/// that stopped mid-run), after stopping any Chrome still running on a
/// profile in it. Returns how many entries went.
pub async fn sweep(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    if entries.is_empty() {
        return 0;
    }
    crate::browser::stop_chromes_under(root).await;
    let mut gone = 0;
    for path in entries {
        let removed = if path.is_dir() {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        match removed {
            Ok(()) => gone += 1,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "couldn't delete a practice run's leftovers");
            }
        }
    }
    if gone > 0 {
        tracing::info!(gone, "deleted practice runs left behind");
    }
    gone
}

fn make_private_dir(dir: &Path) -> Result<(), BrowserError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| BrowserError::Chrome(format!("couldn't make {}: {e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            BrowserError::Chrome(format!("couldn't protect {}: {e}", dir.display()))
        })?;
    }
    Ok(())
}

fn read_dir(dir: &Path) -> Result<Vec<PathBuf>, BrowserError> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| BrowserError::Chrome(format!("couldn't read {}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Copies `parts` into the folder `into` with one `cp`: clones on APFS,
/// a plain copy if the disk can't clone.
async fn copy(parts: &[PathBuf], into: &Path) -> Result<(), BrowserError> {
    if parts.is_empty() {
        return Ok(());
    }
    if cfg!(target_os = "macos") && run_cp("-cRp", parts, into).await? {
        return Ok(());
    }
    if run_cp("-Rp", parts, into).await? {
        return Ok(());
    }
    Err(BrowserError::Chrome(format!(
        "couldn't copy the browser profile into {}",
        into.display()
    )))
}

async fn run_cp(flags: &str, parts: &[PathBuf], into: &Path) -> Result<bool, BrowserError> {
    let status = Command::new("cp")
        .arg(flags)
        .args(parts)
        .arg(into)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|source| BrowserError::Spawn {
            program: "cp".to_owned(),
            source,
        })?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[tokio::test]
    async fn a_clone_keeps_sign_ins_and_leaves_history_out() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("profile");
        for kept in [
            "Local State",
            "Default/Cookies",
            "Default/Preferences",
            "Default/Local Storage/leveldb/000003.log",
            "Default/IndexedDB/https_jobs.example.test_0.indexeddb.leveldb/LOG",
            "Profile 1/Cookies",
        ] {
            touch(&from.join(kept), "kept");
        }
        for gone in [
            "Default/History",
            "Default/History-journal",
            "Default/Visited Links",
            "Default/Top Sites",
            "Default/Sessions/Session_1",
            "Default/Current Session",
            "Default/Favicons",
            "Default/Cache/Cache_Data/index",
            "Default/Code Cache/js/index",
            "Default/Service Worker/Database/LOG",
            "Default/DownloadMetadata",
            "GPUCache/index",
            "Profile 1/History",
            "DevToolsActivePort",
        ] {
            touch(&from.join(gone), "gone");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("host-123", from.join("SingletonLock")).unwrap();

        let to = tmp.path().join("trial/profile");
        clone_profile(&from, &to).await.unwrap();
        for kept in [
            "Local State",
            "Default/Cookies",
            "Default/Preferences",
            "Default/Local Storage/leveldb/000003.log",
            "Default/IndexedDB/https_jobs.example.test_0.indexeddb.leveldb/LOG",
            "Profile 1/Cookies",
        ] {
            assert_eq!(
                std::fs::read_to_string(to.join(kept)).unwrap(),
                "kept",
                "{kept}"
            );
        }
        for gone in [
            "Default/History",
            "Default/History-journal",
            "Default/Visited Links",
            "Default/Top Sites",
            "Default/Sessions",
            "Default/Current Session",
            "Default/Favicons",
            "Default/Cache",
            "Default/Code Cache",
            "Default/Service Worker",
            "Default/DownloadMetadata",
            "GPUCache",
            "Profile 1/History",
            "DevToolsActivePort",
            "SingletonLock",
        ] {
            assert!(
                std::fs::symlink_metadata(to.join(gone)).is_err(),
                "{gone} was copied"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&to).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // Writing the copy leaves the original as it was.
        std::fs::write(to.join("Default/Cookies"), "changed").unwrap();
        assert_eq!(
            std::fs::read_to_string(from.join("Default/Cookies")).unwrap(),
            "kept"
        );
    }

    #[tokio::test]
    async fn no_profile_yet_is_an_empty_one() {
        let tmp = tempfile::tempdir().unwrap();
        let to = tmp.path().join("trial/profile");
        clone_profile(&tmp.path().join("none"), &to).await.unwrap();
        assert!(to.is_dir());
        assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn sweeping_deletes_what_runs_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("trials");
        touch(&root.join("r1/profile/Default/Cookies"), "x");
        touch(&root.join("r2/work/notes.md"), "x");
        assert_eq!(sweep(&root).await, 2);
        assert!(root.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(sweep(&root).await, 0);
        assert_eq!(sweep(&tmp.path().join("missing")).await, 0);
    }
}
