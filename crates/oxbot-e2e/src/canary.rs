//! The key canary: the stand-in `OpenRouter` hands out a fake key, and after
//! each test the runner looks for it everywhere oxbot could have written
//! it: every file in its data folder (bot folders, the database and its
//! `-wal`/`-shm`, backups, Chrome profiles, its log) and the page. It must
//! be in none of them.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The part of a canary key that is searched for: its random tail, so a
/// cut-off, quoted or escaped copy is still found.
#[must_use]
pub fn needle(canary: &str) -> &str {
    canary.rsplit('-').next().unwrap_or(canary)
}

/// The byte patterns to look for: the needle as UTF-8 and as UTF-16
/// (Chrome keeps some page data that way).
fn patterns(needle: &str) -> Vec<Vec<u8>> {
    let utf16: Vec<u8> = needle.encode_utf16().flat_map(u16::to_le_bytes).collect();
    vec![needle.as_bytes().to_vec(), utf16]
}

/// Whether `text` holds the canary.
#[must_use]
pub fn in_text(canary: &str, text: &str) -> bool {
    text.contains(needle(canary))
}

/// Every file under `root` holding the canary (symbolic links aren't
/// followed). Files that can't be read are skipped.
#[must_use]
pub fn files_with(canary: &str, root: &Path) -> Vec<PathBuf> {
    let patterns = patterns(needle(canary));
    let mut found = Vec::new();
    let mut dirs = vec![root.to_owned()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() && file_has(&path, &patterns) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Whether the file holds any pattern, read in pieces that overlap so a
/// match across two pieces is found.
fn file_has(path: &Path, patterns: &[Vec<u8>]) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let overlap = patterns.iter().map(Vec::len).max().unwrap_or(1) - 1;
    let mut buf = vec![0u8; 1 << 20];
    let mut kept = 0;
    loop {
        let Ok(n) = file.read(&mut buf[kept..]) else {
            return false;
        };
        if n == 0 {
            return false;
        }
        let filled = kept + n;
        let window = &buf[..filled];
        if patterns.iter().any(|p| contains(window, p)) {
            return true;
        }
        kept = overlap.min(filled);
        buf.copy_within(filled - kept..filled, 0);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    let mut at = 0;
    while let Some(i) = haystack[at..].iter().position(|&b| b == first) {
        let start = at + i;
        if haystack[start + 1..].starts_with(rest) {
            return true;
        }
        at = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-or-v1-e2e-0123456789abcdef0123456789abcdef";

    #[test]
    fn it_finds_the_key_in_any_file_and_form() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("bots/a/work")).unwrap();
        std::fs::write(root.join("clean.txt"), "nothing here").unwrap();
        std::fs::write(root.join("bots/a/work/notes.md"), format!("key: {KEY}")).unwrap();
        let utf16: Vec<u8> = needle(KEY)
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        std::fs::write(
            root.join("store.ldb"),
            [b"\x00\x01".as_slice(), &utf16].concat(),
        )
        .unwrap();
        // Across the 1 MB read boundary.
        let mut big = vec![b'x'; (1 << 20) - 10];
        big.extend_from_slice(needle(KEY).as_bytes());
        std::fs::write(root.join("oxbot.db-wal"), big).unwrap();
        let found: Vec<String> = files_with(KEY, root)
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().display().to_string())
            .collect();
        assert_eq!(found, ["bots/a/work/notes.md", "oxbot.db-wal", "store.ldb"]);
        assert!(in_text(KEY, &format!("\"{}\"", needle(KEY))));
        assert!(!in_text(KEY, "sk-or-v1-e2e-"));
    }
}
