//! Which tests a change needs, and why.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::spec::{Config, Test, glob};

/// The files a change touches against `base`: committed since it, plus
/// uncommitted and new files.
///
/// # Errors
/// Fails if git can't run.
pub fn changed_files(repo: &Path, base: &str) -> anyhow::Result<Vec<String>> {
    let git = |args: &[&str]| -> anyhow::Result<Vec<String>> {
        let out = Command::new("git").args(args).current_dir(repo).output()?;
        if !out.status.success() {
            anyhow::bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect())
    };
    let mut files = git(&["diff", "--name-only", &format!("{base}...HEAD")])?;
    files.extend(git(&["diff", "--name-only", "HEAD"])?);
    files.extend(git(&["ls-files", "--others", "--exclude-standard"])?);
    files.sort();
    files.dedup();
    Ok(files)
}

/// What to run for a change.
#[derive(Debug, Serialize)]
pub struct Plan {
    /// Tests to run, each with why.
    pub run: Vec<Picked>,
    /// Tests skipped.
    pub skip: Vec<String>,
    /// Test files (and seed or stand-in replies) the change edits: their
    /// intent needs checking.
    pub edited_tests: Vec<String>,
    /// Test files the change adds.
    pub new_tests: Vec<String>,
    /// Also run in Safari (the app's engine).
    pub safari: bool,
    /// Also run the Mac app check.
    pub app: bool,
    /// The files that changed.
    pub changed: Vec<String>,
}

/// A test picked to run.
#[derive(Debug, Serialize)]
pub struct Picked {
    /// Its file, relative to the repo.
    pub file: String,
    /// Its name.
    pub name: String,
    /// Why it runs.
    pub why: String,
}

/// Whether `file` exists in `base`.
#[must_use]
pub fn in_base(repo: &Path, base: &str, file: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", &format!("{base}:{file}")])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Picks tests for `changed` files; `all` runs everything. `is_new` says
/// whether a file is new in this change.
#[must_use]
pub fn plan(
    config: &Config,
    tests: &[Test],
    changed: &[String],
    all: bool,
    repo: &Path,
    is_new: impl Fn(&str) -> bool,
) -> Plan {
    let touches = |patterns: &[String]| -> Option<String> {
        changed.iter().find_map(|f| {
            patterns
                .iter()
                .find(|p| glob(p, f))
                .map(|p| format!("{f} (matches {p})"))
        })
    };
    let mut run = Vec::new();
    let mut skip = Vec::new();
    for test in tests {
        let file = test
            .file
            .strip_prefix(repo)
            .unwrap_or(&test.file)
            .display()
            .to_string();
        let why = if all {
            Some("all tests asked for".to_owned())
        } else if test.smoke {
            Some("smoke test: always runs".to_owned())
        } else if changed.contains(&file) {
            Some(if is_new(&file) {
                "a new test in this change".to_owned()
            } else {
                "the change edits this test".to_owned()
            })
        } else {
            touches(&test.covers).map(|hit| format!("touches {hit}"))
        };
        match why {
            Some(why) => run.push(Picked {
                file,
                name: test.name.clone(),
                why,
            }),
            None => skip.push(file),
        }
    }
    let (new_tests, edited_tests): (Vec<String>, Vec<String>) = changed
        .iter()
        .filter(|f| {
            f.starts_with("e2e/tests/")
                || f.starts_with("e2e/claude/")
                || f.starts_with("e2e/seed/")
        })
        .cloned()
        .partition(|f| is_new(f));
    Plan {
        run,
        skip,
        edited_tests,
        new_tests,
        safari: all || touches(&config.safari_when).is_some(),
        app: all || touches(&config.app_when).is_some(),
        changed: changed.to_vec(),
    }
}
