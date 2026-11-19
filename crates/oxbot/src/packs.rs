//! The big tools oxbot runs (its browser, Codex and Node.js) ship as
//! packs: folders apart from the app, so an update downloads only the ones
//! that changed. Each app carries `packs.json` in its Resources, naming the
//! pack versions it was tested with, sealed by the app's signature. It runs
//! only with exactly those: a pack is found in oxbot's own tools folder
//! (`<data>/tools/<name>-<version>`, where updates put them) or inside the
//! app (`Resources/tools/<name>`, where the .dmg's app carries a first
//! copy). Each pack folder says what it is in `.pack.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The app's list of packs, in its Resources.
pub const LIST: &str = "packs.json";
/// What a pack folder says it is.
pub const MARK: &str = ".pack.json";

/// The browser (`Camoufox.app`).
pub const BROWSER: &str = "browser";
/// Codex's release folder (`bin/codex`).
pub const CODEX: &str = "codex";
/// Node.js (`bin/node`).
pub const NODE: &str = "node";

/// One pack an app version runs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    /// [`BROWSER`], [`CODEX`] or [`NODE`].
    pub name: String,
    /// Its version, as the release that made it named it.
    pub version: String,
    /// SHA-256 of its download (`<name>-<version>-<arch>.tar.gz`).
    pub sha256: String,
    /// The download's size, in bytes.
    pub size: u64,
}

impl Pack {
    /// Its folder's name in oxbot's tools folder.
    #[must_use]
    pub fn folder(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

#[derive(Debug, Deserialize)]
struct List {
    schema: u32,
    packs: Vec<Pack>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Mark {
    name: String,
    version: String,
}

/// The packs the app in `resources` runs with. `Ok(None)`: it has no list
/// (a source checkout, or an app from before packs), and finds its tools
/// the old way.
///
/// # Errors
/// The list is there but can't be read.
pub fn read_list(resources: &Path) -> Result<Option<Vec<Pack>>, String> {
    let file = resources.join(LIST);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{}: {err}", file.display())),
    };
    parse_list(&text)
        .map(Some)
        .map_err(|err| format!("{}: {err}", file.display()))
}

/// A pack list's text, read.
///
/// # Errors
/// It isn't a pack list this version knows.
pub fn parse_list(text: &str) -> Result<Vec<Pack>, String> {
    let list: List = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if list.schema != 1 {
        return Err("a newer kind of pack list".to_owned());
    }
    Ok(list.packs)
}

/// Writes what `dir` holds, so oxbot can tell it's `pack`.
///
/// # Errors
/// The file can't be written.
pub fn mark(dir: &Path, pack: &Pack) -> std::io::Result<()> {
    let mark = Mark {
        name: pack.name.clone(),
        version: pack.version.clone(),
    };
    std::fs::write(
        dir.join(MARK),
        serde_json::to_vec(&mark).map_err(std::io::Error::other)?,
    )
}

/// Whether `dir` holds `pack` (by its mark).
#[must_use]
pub fn is_marked(dir: &Path, pack: &Pack) -> bool {
    std::fs::read(dir.join(MARK))
        .ok()
        .and_then(|b| serde_json::from_slice::<Mark>(&b).ok())
        .is_some_and(|m| m.name == pack.name && m.version == pack.version)
}

/// Where `pack` is: oxbot's own tools folder (`own`) first, then inside
/// the app (`inside`, its `Resources/tools`). `None` if neither has that
/// exact version.
#[must_use]
pub fn locate(pack: &Pack, own: &Path, inside: &Path) -> Option<PathBuf> {
    [own.join(pack.folder()), inside.join(&pack.name)]
        .into_iter()
        .find(|dir| is_marked(dir, pack))
}

/// Where the app's packs are, by name, and the ones it can't find.
#[must_use]
pub fn locate_all(
    packs: &[Pack],
    own: &Path,
    inside: &Path,
) -> (Vec<(String, PathBuf)>, Vec<Pack>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for pack in packs {
        match locate(pack, own, inside) {
            Some(dir) => found.push((pack.name.clone(), dir)),
            None => missing.push(pack.clone()),
        }
    }
    (found, missing)
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

    #[test]
    fn a_pack_is_found_in_oxbots_folder_first_then_in_the_app() {
        let tmp = tempfile::tempdir().unwrap();
        let own = tmp.path().join("tools");
        let inside = tmp.path().join("oxbot.app/Contents/Resources/tools");
        let codex = pack(CODEX, "0.148.0");
        // The .dmg's app carries it.
        std::fs::create_dir_all(inside.join(CODEX)).unwrap();
        mark(&inside.join(CODEX), &codex).unwrap();
        assert_eq!(locate(&codex, &own, &inside), Some(inside.join(CODEX)));
        // An update put it in oxbot's folder: that one wins.
        let mine = own.join("codex-0.148.0");
        std::fs::create_dir_all(&mine).unwrap();
        mark(&mine, &codex).unwrap();
        assert_eq!(locate(&codex, &own, &inside), Some(mine));
        // Another version is never taken for it.
        let newer = pack(CODEX, "0.150.0");
        assert_eq!(locate(&newer, &own, &inside), None);
        let (found, missing) = locate_all(&[codex, newer.clone()], &own, &inside);
        assert_eq!(found.len(), 1);
        assert_eq!(missing, [newer]);
    }

    #[test]
    fn the_app_says_which_packs_it_runs_with() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_list(tmp.path()).unwrap(), None);
        std::fs::write(
            tmp.path().join(LIST),
            r#"{"schema":1,"packs":[{"name":"node","version":"24.22.2","sha256":"ab","size":5}]}"#,
        )
        .unwrap();
        assert_eq!(
            read_list(tmp.path()).unwrap(),
            Some(vec![Pack {
                name: NODE.into(),
                version: "24.22.2".into(),
                sha256: "ab".into(),
                size: 5
            }])
        );
        std::fs::write(tmp.path().join(LIST), r#"{"schema":2,"packs":[]}"#).unwrap();
        assert!(read_list(tmp.path()).is_err());
    }
}
