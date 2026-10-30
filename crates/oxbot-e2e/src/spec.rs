//! Test files (`e2e/tests/*.toml`) and the repo's settings
//! (`e2e/pr_test.toml`).

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// `e2e/pr_test.toml`: how to build and start the app under test.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Commands that build what the tests need, run from the repo root.
    #[serde(default)]
    pub build: Vec<String>,
    /// The server program, relative to the repo root.
    pub server: PathBuf,
    /// The stand-in Claude, relative to the repo root.
    pub fake_claude: PathBuf,
    /// The browser tool built into the app, if built (relative path).
    pub tools: Option<PathBuf>,
    /// The Farm's person in tests.
    #[serde(default = "default_person")]
    pub person: String,
    /// Changes under these paths also run the tests in Safari.
    #[serde(default)]
    pub safari_when: Vec<String>,
    /// Changes under these paths also run the Mac app check.
    #[serde(default)]
    pub app_when: Vec<String>,
    /// The built Mac app, for the app check.
    pub app: Option<PathBuf>,
    /// Commands that build the Mac app.
    #[serde(default)]
    pub app_build: Vec<String>,
}

fn default_person() -> String {
    "Jay".to_owned()
}

impl Config {
    /// Reads `e2e/pr_test.toml` under `repo`.
    ///
    /// # Errors
    /// Fails if it's missing or not valid.
    pub fn load(repo: &Path) -> anyhow::Result<Self> {
        let path = repo.join("e2e/pr_test.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("in {}", path.display()))
    }
}

/// One end-to-end test.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Test {
    /// What it checks, as a title.
    pub name: String,
    /// A sentence on why it matters.
    #[serde(default)]
    pub about: String,
    /// Paths (globs) this test covers; a change touching one picks the test.
    #[serde(default)]
    pub covers: Vec<String>,
    /// Always run (the smoke set).
    #[serde(default)]
    pub smoke: bool,
    /// Connect as this person (a Gate person); empty means the Farm.
    #[serde(default, rename = "as")]
    pub as_person: Option<String>,
    /// The stand-in Claude's script, in `e2e/claude/`.
    #[serde(default = "default_claude")]
    pub claude: String,
    /// The steps, in order.
    #[serde(rename = "step")]
    pub steps: Vec<Step>,
    /// The file it came from (set when loaded).
    #[serde(skip)]
    pub file: PathBuf,
}

fn default_claude() -> String {
    "default.toml".to_owned()
}

/// One step. Exactly one action; `shot` names the screenshot taken after it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Go to a path on the app, e.g. `/`.
    pub open: Option<String>,
    /// Click the element showing this text (or with this label).
    pub click: Option<String>,
    /// Type into a field.
    #[serde(rename = "type")]
    pub type_: Option<TypeInto>,
    /// Type a message in the chat box and press Enter.
    pub send: Option<String>,
    /// Press a key, e.g. `Enter` or `Escape`.
    pub key: Option<String>,
    /// Wait until this text shows.
    pub wait_for: Option<String>,
    /// This text must show.
    pub expect: Option<String>,
    /// This text must not show.
    pub expect_not: Option<String>,
    /// The field or button with this text or label must be disabled.
    pub expect_disabled: Option<String>,
    /// This JavaScript must be true on the page.
    pub expect_js: Option<String>,
    /// Pause, in milliseconds.
    pub wait_ms: Option<u64>,
    /// Close the hidden sign-in window, as a person would after signing in.
    pub close_sign_in_window: Option<bool>,
    /// A name for the screenshot after this step.
    pub shot: Option<String>,
    /// Why this step is here.
    pub note: Option<String>,
    /// How long `wait_for` waits (ms; default 10 000).
    pub timeout_ms: Option<u64>,
}

/// `type = { into = "Name", text = "Recipes" }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TypeInto {
    /// The field's placeholder, label or id.
    pub into: String,
    /// What to type.
    pub text: String,
    /// Clear it first (default true).
    #[serde(default = "yes")]
    pub clear: bool,
}

fn yes() -> bool {
    true
}

impl Step {
    /// The step in words, for the report.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.open {
            parts.push(format!("Open {p}"));
        }
        if let Some(t) = &self.click {
            parts.push(format!("Click \u{201c}{t}\u{201d}"));
        }
        if let Some(t) = &self.type_ {
            parts.push(format!("Type \u{201c}{}\u{201d} into {}", t.text, t.into));
        }
        if let Some(t) = &self.send {
            parts.push(format!("Send \u{201c}{t}\u{201d}"));
        }
        if let Some(k) = &self.key {
            parts.push(format!("Press {k}"));
        }
        if let Some(t) = &self.wait_for {
            parts.push(format!("Wait for \u{201c}{t}\u{201d}"));
        }
        if let Some(t) = &self.expect {
            parts.push(format!("Expect \u{201c}{t}\u{201d}"));
        }
        if let Some(t) = &self.expect_not {
            parts.push(format!("Expect no \u{201c}{t}\u{201d}"));
        }
        if let Some(t) = &self.expect_disabled {
            parts.push(format!("Expect \u{201c}{t}\u{201d} disabled"));
        }
        if let Some(t) = &self.expect_js {
            parts.push(format!("Check {t}"));
        }
        if let Some(ms) = self.wait_ms {
            parts.push(format!("Wait {ms} ms"));
        }
        if self.close_sign_in_window == Some(true) {
            parts.push("Close the sign-in window".to_owned());
        }
        if parts.is_empty() {
            parts.push("Look".to_owned());
        }
        parts.join(", then ")
    }

    fn actions(&self) -> usize {
        [
            self.open.is_some(),
            self.click.is_some(),
            self.type_.is_some(),
            self.send.is_some(),
            self.key.is_some(),
            self.wait_for.is_some(),
            self.expect.is_some(),
            self.expect_not.is_some(),
            self.expect_disabled.is_some(),
            self.expect_js.is_some(),
            self.wait_ms.is_some(),
            self.close_sign_in_window.is_some(),
        ]
        .iter()
        .filter(|a| **a)
        .count()
    }
}

impl Test {
    /// Reads a test file.
    ///
    /// # Errors
    /// Fails if it can't be read, isn't valid, or a step has no action.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let source =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut test: Self =
            toml::from_str(&source).with_context(|| format!("in {}", path.display()))?;
        for (i, step) in test.steps.iter().enumerate() {
            if step.actions() == 0 && step.shot.is_none() {
                bail!("{}: step {} does nothing", path.display(), i + 1);
            }
        }
        path.clone_into(&mut test.file);
        Ok(test)
    }

    /// Its id: the file name without `.toml`.
    #[must_use]
    pub fn id(&self) -> String {
        self.file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Every test in `e2e/tests/`, sorted by file name.
///
/// # Errors
/// Fails if the folder can't be read or a test isn't valid.
pub fn all_tests(repo: &Path) -> anyhow::Result<Vec<Test>> {
    let dir = repo.join("e2e/tests");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files.iter().map(|f| Test::load(f)).collect()
}

/// Whether `path` matches `pattern`: `*` is any run of characters except
/// `/`, `**` any run including `/`.
#[must_use]
pub fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
                (0..=s.len()).any(|i| go(rest, &s[i..]))
            }
            Some(b'*') => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| go(&p[1..], &s[i..])),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_paths() {
        assert!(glob("crates/oxbot/ui/**", "crates/oxbot/ui/index.html"));
        assert!(glob("crates/**/hub.rs", "crates/oxbot-core/src/hub.rs"));
        assert!(glob("crates/*/src/lib.rs", "crates/oxbot/src/lib.rs"));
        assert!(!glob("crates/*/lib.rs", "crates/oxbot/src/lib.rs"));
        assert!(glob("app/**", "app/src-tauri/src/main.rs"));
        assert!(!glob("app/**", "crates/app.rs"));
    }

    #[test]
    fn steps_describe_themselves() {
        let step: Step = toml::from_str("click = \"Bank\"\nshot = \"x\"").unwrap();
        assert_eq!(step.describe(), "Click \u{201c}Bank\u{201d}");
        assert!(
            toml::from_str::<Step>("klick = \"x\"").is_err(),
            "typos are errors"
        );
    }
}
