//! File tools for the bot's own `work/` folder: `Read`, `Write`, `Edit`,
//! `Glob`, `Grep`, with the names and arguments models know them by.
//!
//! The gate already refused any path outside `work/`; these check again
//! (defense in depth): a path must be inside `work/` both as written and
//! after following symlinks, and files are opened only after that check.

use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use oxbot_llm::ToolSpec;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};

/// Most lines one `Read` returns.
const MAX_READ_LINES: usize = 2_000;
/// Most bytes one `Read` returns.
const MAX_READ_BYTES: usize = 256 * 1024;
/// Biggest file `Write` writes.
const MAX_WRITE_BYTES: usize = 1024 * 1024;
/// Biggest file `Edit` and `Grep` read.
const MAX_EDIT_BYTES: u64 = 10 * 1024 * 1024;
/// Most paths `Glob` lists, and lines `Grep` returns.
const MAX_RESULTS: usize = 500;
/// Most entries a search walks.
const MAX_WALK: usize = 50_000;

/// The file tools' names, in their fixed order.
pub const FILE_TOOLS: [&str; 5] = ["Read", "Write", "Edit", "Glob", "Grep"];

/// Whether `name` is a file tool that changes files.
#[must_use]
pub fn changes_files(name: &str) -> bool {
    matches!(name, "Write" | "Edit")
}

/// The file tools as the model sees them.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "Read".into(),
            description:
                "Read a text file in your work folder. Lines come numbered. For long files, \
                pass offset (first line, from 1) and limit (number of lines)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "The file, relative to your work folder or absolute inside it."},
                    "offset": {"type": "integer", "description": "First line to read, from 1."},
                    "limit": {"type": "integer", "description": "How many lines to read (at most 2000)."}
                },
                "required": ["file_path"]
            }),
        },
        ToolSpec {
            name: "Write".into(),
            description:
                "Write a text file in your work folder (replacing it if it exists). Folders are \
                made as needed."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "The file, relative to your work folder or absolute inside it."},
                    "content": {"type": "string", "description": "The whole new text of the file."}
                },
                "required": ["file_path", "content"]
            }),
        },
        ToolSpec {
            name: "Edit".into(),
            description:
                "Replace exact text in a file in your work folder. old_string must appear \
                exactly once, unless replace_all is true."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "The file, relative to your work folder or absolute inside it."},
                    "old_string": {"type": "string", "description": "The exact text to replace."},
                    "new_string": {"type": "string", "description": "The text to put there instead."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence."}
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        },
        ToolSpec {
            name: "Glob".into(),
            description: "List files in your work folder whose path matches a pattern such as \
                \"**/*.md\" (* within a folder, ** across folders, ? one character)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The pattern."},
                    "path": {"type": "string", "description": "Folder to search in (default: your work folder)."}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "Grep".into(),
            description:
                "Search the text of files in your work folder with a regular expression. Shows \
                file:line: text for each match."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The regular expression."},
                    "path": {"type": "string", "description": "File or folder to search (default: your work folder)."},
                    "glob": {"type": "string", "description": "Only files whose name matches this pattern, e.g. \"*.md\"."},
                    "case_insensitive": {"type": "boolean", "description": "Ignore upper and lower case."}
                },
                "required": ["pattern"]
            }),
        },
    ]
}

/// What a file tool returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    /// The text.
    pub text: String,
    /// The file it read, relative to `work/`, for reads.
    pub read_path: Option<String>,
}

/// Runs file tool `name` with `args` inside `work`. Blocking: call it off
/// the async threads.
///
/// # Errors
/// What went wrong, for the model.
pub fn run(name: &str, args: &Value, work: &Path) -> Result<FileResult, String> {
    let work_real = work
        .canonicalize()
        .map_err(|e| format!("Your work folder can't be read: {e}"))?;
    let text = |field: &str| args.get(field).and_then(Value::as_str);
    let need = |field: &str| text(field).ok_or_else(|| format!("{name} needs {field}"));
    match name {
        "Read" => {
            let path = resolve(work, &work_real, Some(need("file_path")?))?;
            read(&path, args, &work_real).map(|text| FileResult {
                text,
                read_path: Some(shown(&path, work)),
            })
        }
        "Write" => {
            let path = resolve(work, &work_real, Some(need("file_path")?))?;
            write(&path, need("content")?, &work_real).map(plain)
        }
        "Edit" => {
            let path = resolve(work, &work_real, Some(need("file_path")?))?;
            let all = args
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            edit(
                &path,
                need("old_string")?,
                need("new_string")?,
                all,
                &work_real,
            )
            .map(plain)
        }
        "Glob" => {
            let base = resolve(work, &work_real, text("path"))?;
            glob(&base, need("pattern")?, work).map(plain)
        }
        "Grep" => {
            let base = resolve(work, &work_real, text("path"))?;
            let ci = args
                .get("case_insensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            grep(&base, need("pattern")?, text("glob"), ci, work).map(plain)
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn plain(text: String) -> FileResult {
    FileResult {
        text,
        read_path: None,
    }
}

const OUTSIDE: &str = "Bots may only use files in their own work folder.";

/// `given` (relative to `work`, or absolute) as a path inside `work`:
/// lexically, and its nearest existing folder after following symlinks.
fn resolve(work: &Path, work_real: &Path, given: Option<&str>) -> Result<PathBuf, String> {
    let Some(given) = given.filter(|g| !g.trim().is_empty()) else {
        return Ok(work.to_path_buf());
    };
    let given = Path::new(given);
    // `work` as given and as resolved are both accepted roots.
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        work.join(given)
    };
    let lexical = normalize(&joined);
    let root = if lexical.starts_with(normalize(work)) {
        normalize(work)
    } else if lexical.starts_with(work_real) {
        work_real.to_path_buf()
    } else {
        return Err(OUTSIDE.to_owned());
    };
    // The nearest part of it that exists must really be inside too.
    let mut existing = lexical.as_path();
    while !existing.exists() {
        match existing.parent() {
            Some(p) if p.starts_with(&root) => existing = p,
            _ => break,
        }
    }
    let real = existing
        .canonicalize()
        .map_err(|e| format!("{} can't be read: {e}", shown(&lexical, work)))?;
    if !real.starts_with(work_real) {
        return Err(OUTSIDE.to_owned());
    }
    Ok(lexical)
}

/// The path with `.` and `..` worked out, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The path as the model sees it: relative to `work`.
fn shown(path: &Path, work: &Path) -> String {
    path.strip_prefix(normalize(work))
        .or_else(|_| path.strip_prefix(work))
        .map_or_else(|_| path.display().to_string(), |p| p.display().to_string())
}

/// The file's real path, checked to be inside `work_real`.
fn real_inside(path: &Path, work_real: &Path) -> Result<PathBuf, String> {
    let real = path
        .canonicalize()
        .map_err(|e| format!("{} can't be opened: {e}", path.display()))?;
    if real.starts_with(work_real) {
        Ok(real)
    } else {
        Err(OUTSIDE.to_owned())
    }
}

fn read(path: &Path, args: &Value, work_real: &Path) -> Result<String, String> {
    let real = real_inside(path, work_real)?;
    if real.is_dir() {
        return Err("That's a folder; use Glob to list it.".to_owned());
    }
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(1)
        .max(1);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(MAX_READ_LINES)
        .clamp(1, MAX_READ_LINES);
    let mut bytes = Vec::new();
    fs::File::open(&real)
        .and_then(|f| f.take(MAX_EDIT_BYTES).read_to_end(&mut bytes))
        .map_err(|e| format!("couldn't read it: {e}"))?;
    if bytes.contains(&0) {
        return Err("That file isn't text.".to_owned());
    }
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    if total == 0 {
        return Ok("(the file is empty)".to_owned());
    }
    if offset > total {
        return Ok(format!("(the file has only {total} lines)"));
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (n, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        if out.len() + line.len() > MAX_READ_BYTES {
            break;
        }
        let _ = writeln!(out, "{:>6}\t{line}", n + 1);
        last = n + 1;
    }
    if last < total {
        let _ = write!(
            out,
            "(lines {offset}-{last} of {total}; read on with offset {})",
            last + 1
        );
    }
    Ok(out)
}

/// Refuses to write through a symlink.
fn not_a_link(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(OUTSIDE.to_owned()),
        Ok(m) if m.is_dir() => Err("That's a folder.".to_owned()),
        _ => Ok(()),
    }
}

fn write(path: &Path, content: &str, work_real: &Path) -> Result<String, String> {
    if content.len() > MAX_WRITE_BYTES {
        return Err("That's too big to write (1 MB at most).".to_owned());
    }
    let parent = path.parent().ok_or_else(|| OUTSIDE.to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("couldn't make the folder: {e}"))?;
    real_inside(parent, work_real)?;
    not_a_link(path)?;
    fs::write(path, content).map_err(|e| format!("couldn't write it: {e}"))?;
    Ok(format!(
        "Wrote {} bytes to {}.",
        content.len(),
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ))
}

fn edit(path: &Path, old: &str, new: &str, all: bool, work_real: &Path) -> Result<String, String> {
    if old.is_empty() {
        return Err("old_string can't be empty.".to_owned());
    }
    if old == new {
        return Err("old_string and new_string are the same.".to_owned());
    }
    not_a_link(path)?;
    let real = real_inside(path, work_real)?;
    let size = fs::metadata(&real).map_or(0, |m| m.len());
    if size > MAX_EDIT_BYTES {
        return Err("That file is too big to edit.".to_owned());
    }
    let text = fs::read_to_string(&real).map_err(|e| format!("couldn't read it: {e}"))?;
    let count = text.matches(old).count();
    let updated = match count {
        0 => return Err("old_string isn't in the file.".to_owned()),
        1 => text.replacen(old, new, 1),
        _ if all => text.replace(old, new),
        n => {
            return Err(format!(
                "old_string is in the file {n} times; give more text around it, or set replace_all."
            ));
        }
    };
    if updated.len() > MAX_WRITE_BYTES.max(usize::try_from(size).unwrap_or(usize::MAX)) {
        return Err("The edited file would be too big.".to_owned());
    }
    fs::write(&real, updated).map_err(|e| format!("couldn't write it: {e}"))?;
    Ok(format!(
        "Edited {} ({count} {}).",
        real.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        if count == 1 {
            "replacement"
        } else {
            "replacements"
        }
    ))
}

/// A glob pattern as an anchored regular expression.
fn glob_regex(pattern: &str) -> Result<Regex, String> {
    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).map_err(|e| format!("bad pattern: {e}"))
}

/// Every file under `base` (not following symlinks), as paths relative
/// to `base`.
fn walk(base: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK {
                return out;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .map_or_else(|_| String::new(), |p| p.to_string_lossy().into_owned());
                out.push((path, rel));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn glob(base: &Path, pattern: &str, work: &Path) -> Result<String, String> {
    if base.is_file() {
        return Err("path must be a folder.".to_owned());
    }
    let re = glob_regex(pattern)?;
    let hits: Vec<String> = walk(base)
        .into_iter()
        .filter(|(_, rel)| re.is_match(rel))
        .map(|(path, _)| shown(&path, work))
        .collect();
    if hits.is_empty() {
        return Ok("No files match.".to_owned());
    }
    let mut out = hits
        .iter()
        .take(MAX_RESULTS)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if hits.len() > MAX_RESULTS {
        let _ = write!(out, "\n(and {} more)", hits.len() - MAX_RESULTS);
    }
    Ok(out)
}

fn grep(
    base: &Path,
    pattern: &str,
    name_glob: Option<&str>,
    case_insensitive: bool,
    work: &Path,
) -> Result<String, String> {
    let re = RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("bad pattern: {e}"))?;
    let names = name_glob.map(glob_regex).transpose()?;
    let files = if base.is_file() {
        vec![(base.to_path_buf(), String::new())]
    } else {
        walk(base)
    };
    let mut out = String::new();
    let mut hits = 0;
    'files: for (path, _) in files {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if names.as_ref().is_some_and(|g| !g.is_match(&name)) {
            continue;
        }
        if fs::metadata(&path).map_or(true, |m| m.len() > MAX_EDIT_BYTES) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (n, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits += 1;
                if hits > MAX_RESULTS || out.len() > MAX_READ_BYTES {
                    out.push_str("(more matches left out)");
                    break 'files;
                }
                let line: String = line.chars().take(300).collect();
                let _ = writeln!(out, "{}:{}: {line}", shown(&path, work), n + 1);
            }
        }
    }
    if out.is_empty() {
        return Ok("No matches.".to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("bot/work");
        fs::create_dir_all(work.join("notes")).unwrap();
        fs::write(work.join("notes/a.md"), "alpha\nbeta\ngamma\n").unwrap();
        fs::write(work.join("b.txt"), "Beta here\n").unwrap();
        fs::write(tmp.path().join("bot/bot.toml"), "secret = 1\n").unwrap();
        (tmp, work)
    }

    #[test]
    fn read_numbers_lines_and_pages() {
        let (_tmp, work) = setup();
        let r = run("Read", &json!({"file_path": "notes/a.md"}), &work).unwrap();
        assert_eq!(r.text, "     1\talpha\n     2\tbeta\n     3\tgamma\n");
        assert_eq!(r.read_path.as_deref(), Some("notes/a.md"));
        let r = run(
            "Read",
            &json!({"file_path": "notes/a.md", "offset": 2, "limit": 1}),
            &work,
        )
        .unwrap();
        assert!(
            r.text.starts_with("     2\tbeta\n(lines 2-2 of 3"),
            "{}",
            r.text
        );
    }

    #[test]
    fn paths_outside_work_are_refused() {
        let (tmp, work) = setup();
        for path in [
            "../bot.toml",
            "notes/../../bot.toml",
            tmp.path().join("bot/bot.toml").to_str().unwrap(),
            "/etc/hosts",
        ] {
            assert_eq!(
                run("Read", &json!({"file_path": path}), &work).unwrap_err(),
                OUTSIDE,
                "{path}"
            );
        }
        assert_eq!(
            run(
                "Write",
                &json!({"file_path": "../x.txt", "content": "x"}),
                &work
            )
            .unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            run("Glob", &json!({"pattern": "*", "path": ".."}), &work).unwrap_err(),
            OUTSIDE
        );
    }

    #[test]
    fn symlinks_out_of_work_are_refused() {
        let (tmp, work) = setup();
        std::os::unix::fs::symlink(tmp.path().join("bot"), work.join("up")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("bot/bot.toml"), work.join("t.toml")).unwrap();
        assert_eq!(
            run("Read", &json!({"file_path": "up/bot.toml"}), &work).unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            run("Read", &json!({"file_path": "t.toml"}), &work).unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            run(
                "Write",
                &json!({"file_path": "t.toml", "content": "x"}),
                &work
            )
            .unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            run(
                "Write",
                &json!({"file_path": "up/new.txt", "content": "x"}),
                &work
            )
            .unwrap_err(),
            OUTSIDE
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("bot/bot.toml")).unwrap(),
            "secret = 1\n"
        );
        // Globbing doesn't follow it either.
        let r = run("Glob", &json!({"pattern": "**/*.toml"}), &work).unwrap();
        assert_eq!(r.text, "No files match.");
    }

    #[test]
    fn write_and_edit() {
        let (_tmp, work) = setup();
        run(
            "Write",
            &json!({"file_path": "new/dir/c.md", "content": "one two two"}),
            &work,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(work.join("new/dir/c.md")).unwrap(),
            "one two two"
        );
        let e = run(
            "Edit",
            &json!({"file_path": "new/dir/c.md", "old_string": "two", "new_string": "2"}),
            &work,
        )
        .unwrap_err();
        assert!(e.contains("2 times"), "{e}");
        run(
            "Edit",
            &json!({"file_path": "new/dir/c.md", "old_string": "two", "new_string": "2", "replace_all": true}),
            &work,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(work.join("new/dir/c.md")).unwrap(),
            "one 2 2"
        );
    }

    #[test]
    fn glob_and_grep() {
        let (_tmp, work) = setup();
        assert_eq!(
            run("Glob", &json!({"pattern": "**/*.md"}), &work)
                .unwrap()
                .text,
            "notes/a.md"
        );
        assert_eq!(
            run("Glob", &json!({"pattern": "*.txt"}), &work)
                .unwrap()
                .text,
            "b.txt"
        );
        let r = run(
            "Grep",
            &json!({"pattern": "beta", "case_insensitive": true}),
            &work,
        )
        .unwrap();
        assert_eq!(r.text, "b.txt:1: Beta here\nnotes/a.md:2: beta\n");
        let r = run("Grep", &json!({"pattern": "beta", "glob": "*.md"}), &work).unwrap();
        assert_eq!(r.text, "notes/a.md:2: beta\n");
    }
}
