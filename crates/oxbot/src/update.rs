//! The parts of an app update that aren't the app itself: its packs.
//!
//! An update is a slim app (no packs inside) whose signature the oxbot app
//! checks with its update key. Its `packs.json`, sealed by that signature,
//! names the packs it runs with and each one's SHA-256. Before the update
//! is installed, every one of them must be in oxbot's own tools folder:
//! copied from this app when it carries that version (on the Mac's disk a
//! copy shares the same blocks, so it takes no space), else downloaded and
//! checked against that SHA-256. Packs other versions used are removed once
//! a version has started with all of its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt as _;
use tokio::io::AsyncWriteExt as _;

use crate::packs::{self, Pack};

/// Where releases are: GitHub Releases of oxbot's public repo.
pub const RELEASES: &str = "https://github.com/jay-rajan/oxbot/releases";

/// The feed a channel's apps check: a fixed `feed` release holds one file
/// per channel, replaced on each release.
#[must_use]
pub fn feed_url(channel: &str) -> String {
    format!("{RELEASES}/download/feed/{channel}.json")
}

/// A version's release notes.
#[must_use]
pub fn notes_url(version: &str) -> String {
    format!("{RELEASES}/tag/v{version}")
}

/// Where packs are downloaded from: a fixed `packs` release holds every
/// pack any version uses, each made once.
const PACKS: &str = "https://github.com/jay-rajan/oxbot/releases/download/packs";

/// This build's chip, as pack files name it.
pub const ARCH: &str = if cfg!(target_arch = "aarch64") {
    "arm64"
} else {
    "x64"
};

/// A pack's download file, e.g. `node-24.22.2-arm64.tar.gz`.
#[must_use]
pub fn pack_file(pack: &Pack) -> String {
    format!("{}-{}-{ARCH}.tar.gz", pack.name, pack.version)
}

/// Where the pack list sits inside an update's archive.
const LIST_IN_ARCHIVE: &str = "oxbot.app/Contents/Resources/packs.json";

/// The packs an update runs with, read from its archive (an
/// `oxbot.app.tar.gz` whose signature was already checked).
///
/// # Errors
/// The archive has no readable pack list.
pub async fn packs_in_update(archive: &Path) -> Result<Vec<Pack>, String> {
    let out = tokio::process::Command::new("tar")
        .arg("-xzOf")
        .arg(archive)
        .arg(LIST_IN_ARCHIVE)
        .output()
        .await
        .map_err(|e| format!("couldn't open the update: {e}"))?;
    if !out.status.success() {
        return Err("the update has no pack list".to_owned());
    }
    packs::parse_list(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| format!("the update's pack list: {e}"))
}

/// One of oxbot's own downloads, for the network log.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The pack.
    pub pack: String,
    /// When it started (ms since the Unix epoch).
    pub ts_ms: i64,
    /// Bytes received.
    pub bytes: u64,
    /// How long it took.
    pub ms: u64,
    /// `ok`, or a short reason.
    pub outcome: String,
}

/// Puts every pack in `list` in `own` (oxbot's tools folder): already
/// there, copied from `inside` (this app's `Resources/tools`), or
/// downloaded. `on_fetch` hears of each download.
///
/// # Errors
/// A pack couldn't be had, or didn't match its SHA-256.
pub async fn gather(
    list: &[Pack],
    own: &Path,
    inside: &Path,
    on_fetch: &(dyn Fn(Fetched) + Send + Sync),
) -> Result<(), String> {
    tokio::fs::create_dir_all(own)
        .await
        .map_err(|e| format!("couldn't make {}: {e}", own.display()))?;
    let mut http = None;
    for pack in list {
        let dest = own.join(pack.folder());
        if packs::is_marked(&dest, pack) {
            continue;
        }
        let here = inside.join(&pack.name);
        if packs::is_marked(&here, pack) {
            copy_dir(&here, &dest).await?;
            continue;
        }
        if http.is_none() {
            http = Some(client()?);
        }
        let Some(http) = &http else { continue };
        let started = Instant::now();
        let ts_ms = crate::runtime::now_ms();
        let got = download(http, pack, own).await;
        on_fetch(Fetched {
            pack: pack.folder(),
            ts_ms,
            bytes: *got.as_ref().unwrap_or(&0),
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            outcome: got.as_ref().map_or_else(Clone::clone, |_| "ok".to_owned()),
        });
        got?;
        unpack(&own.join(pack_file(pack)), &dest, pack).await?;
    }
    Ok(())
}

/// Removes what `own` holds besides `list`'s packs: other versions'
/// packs, and half-made downloads. Run once a version has all its packs.
pub fn tidy(list: &[Pack], own: &Path) {
    let Ok(entries) = std::fs::read_dir(own) else {
        return;
    };
    let keep: Vec<String> = list.iter().map(Pack::folder).collect();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) {
            continue;
        }
        // Only what oxbot puts here: pack folders and their downloads.
        let ours = [packs::BROWSER, packs::CODEX, packs::NODE]
            .iter()
            .any(|p| name.starts_with(&format!("{p}-")));
        if !ours {
            continue;
        }
        let path = entry.path();
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match gone {
            Ok(()) => tracing::info!(pack = %name, "removed a pack this version doesn't use"),
            Err(err) => tracing::warn!(pack = %name, %err, "couldn't remove an old pack"),
        }
    }
}

/// Copies a pack folder, as a clone of the same blocks where the disk
/// allows it (macOS `cp -c`).
async fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let part = with_suffix(to, ".part");
    let _ = tokio::fs::remove_dir_all(&part).await;
    let ok = tokio::process::Command::new("cp")
        .args(if cfg!(target_os = "macos") {
            &["-cR"][..]
        } else {
            &["-R"][..]
        })
        .arg(from)
        .arg(&part)
        .status()
        .await
        .is_ok_and(|s| s.success());
    if !ok {
        let _ = tokio::fs::remove_dir_all(&part).await;
        return Err(format!("couldn't copy {}", from.display()));
    }
    finish(&part, to).await
}

/// Downloads `pack` into `own` (resuming a half-made download) and checks
/// its size and SHA-256. Returns the bytes received.
async fn download(http: &reqwest::Client, pack: &Pack, own: &Path) -> Result<u64, String> {
    let file = own.join(pack_file(pack));
    if sha256_of(&file).await.as_deref() == Some(pack.sha256.as_str()) {
        return Ok(0);
    }
    let part = with_suffix(&file, ".part");
    let have = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());
    let mut req = http.get(format!("{PACKS}/{}", pack_file(pack)));
    if have > 0 && have < pack.size {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("couldn't download {}: {}", pack.folder(), e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("couldn't download {}: {status}", pack.folder()));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&part)
        .await
        .map_err(|e| e.to_string())?;
    let mut got = 0u64;
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk =
            chunk.map_err(|e| format!("the download of {} stopped: {}", pack.folder(), e.without_url()))?;
        got += chunk.len() as u64;
        if got + if resumed { have } else { 0 } > pack.size {
            break;
        }
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    let size = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());
    if size != pack.size || sha256_of(&part).await.as_deref() != Some(pack.sha256.as_str()) {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!(
            "{} didn't match its checksum; it will be downloaded again",
            pack.folder()
        ));
    }
    tokio::fs::rename(&part, &file)
        .await
        .map_err(|e| e.to_string())?;
    Ok(got)
}

/// Unpacks a checked download into `dest` and makes sure it's `pack`.
async fn unpack(archive: &Path, dest: &Path, pack: &Pack) -> Result<(), String> {
    let part = with_suffix(dest, ".part");
    let _ = tokio::fs::remove_dir_all(&part).await;
    tokio::fs::create_dir_all(&part)
        .await
        .map_err(|e| e.to_string())?;
    let ok = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&part)
        .status()
        .await
        .is_ok_and(|s| s.success());
    if !ok || !packs::is_marked(&part, pack) {
        let _ = tokio::fs::remove_dir_all(&part).await;
        return Err(format!("couldn't unpack {}", pack.folder()));
    }
    finish(&part, dest).await?;
    let _ = tokio::fs::remove_file(archive).await;
    Ok(())
}

/// Moves a finished `.part` folder into place.
async fn finish(part: &Path, to: &Path) -> Result<(), String> {
    let _ = tokio::fs::remove_dir_all(to).await;
    tokio::fs::rename(part, to)
        .await
        .map_err(|e| format!("couldn't put {} in place: {e}", to.display()))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A file's SHA-256 in hex, or `None` if it can't be read.
async fn sha256_of(path: &Path) -> Option<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut file = std::fs::File::open(path).ok()?;
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            ctx.update(&buf[..n]);
        }
        Some(ctx.finish().as_ref().iter().fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        }))
    })
    .await
    .ok()
    .flatten()
}

/// HTTPS only, with the system's trust store; GitHub's downloads redirect
/// to its file host.
fn client() -> Result<reqwest::Client, String> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(BuilderVerifierExt::with_platform_verifier)
        .map_err(|e| e.to_string())?
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("oxbot/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.without_url().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(name: &str, version: &str) -> Pack {
        Pack {
            name: name.into(),
            version: version.into(),
            sha256: "00".into(),
            size: 1,
        }
    }

    #[tokio::test]
    async fn an_update_names_its_packs_inside_its_signed_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let res = tmp.path().join("oxbot.app/Contents/Resources");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(
            res.join(packs::LIST),
            r#"{"schema":1,"packs":[{"name":"node","version":"24.22.2","sha256":"ab","size":5}]}"#,
        )
        .unwrap();
        let archive = tmp.path().join("oxbot.app.tar.gz");
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(tmp.path())
            .arg("oxbot.app")
            .status()
            .unwrap();
        assert!(made.success());
        let list = packs_in_update(&archive).await.unwrap();
        assert_eq!(list, [Pack { sha256: "ab".into(), size: 5, ..pack(packs::NODE, "24.22.2") }]);
        // Something else isn't an update.
        let not = tmp.path().join("other.tar.gz");
        std::fs::write(&not, b"not an archive").unwrap();
        assert!(packs_in_update(&not).await.is_err());
    }

    #[tokio::test]
    async fn packs_this_app_carries_are_copied_and_old_ones_tidied() {
        let tmp = tempfile::tempdir().unwrap();
        let own = tmp.path().join("tools");
        let inside = tmp.path().join("oxbot.app/Contents/Resources/tools");
        let node = pack(packs::NODE, "24.22.2");
        std::fs::create_dir_all(inside.join(packs::NODE).join("bin")).unwrap();
        std::fs::write(inside.join("node/bin/node"), b"#!").unwrap();
        packs::mark(&inside.join(packs::NODE), &node).unwrap();
        // An older version, a half-made download, and something not ours.
        std::fs::create_dir_all(own.join("node-22.0.0")).unwrap();
        std::fs::write(own.join("codex-0.1.0-arm64.tar.gz.part"), b"x").unwrap();
        std::fs::create_dir_all(own.join("notes")).unwrap();

        let fetched = std::sync::Mutex::new(Vec::new());
        gather(std::slice::from_ref(&node), &own, &inside, &|f| fetched.lock().unwrap().push(f))
            .await
            .unwrap();
        assert!(fetched.lock().unwrap().is_empty(), "nothing downloaded");
        let copy = own.join("node-24.22.2");
        assert!(packs::is_marked(&copy, &node));
        assert_eq!(std::fs::read(copy.join("bin/node")).unwrap(), b"#!");
        // Already there: left as it is.
        gather(std::slice::from_ref(&node), &own, &inside, &|_| {}).await.unwrap();

        tidy(std::slice::from_ref(&node), &own);
        let mut left: Vec<String> = std::fs::read_dir(&own)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["node-24.22.2", "notes"]);
    }
}
