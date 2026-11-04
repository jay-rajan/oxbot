//! Finding the `codex` program and checking it's a version oxbot speaks.
//!
//! The protocol oxbot speaks was generated from Codex 0.148.0
//! (`codex app-server generate-json-schema`); other versions may differ,
//! so only 0.148.x runs. Codex installed with npm is a Node.js script
//! (`bin/codex.js`) that starts a native program shipped beside it: oxbot
//! starts that program itself, so neither Node.js nor the script's
//! environment is involved.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Codex versions oxbot speaks: `major.minor` of 0.148.x.
pub const SUPPORTED: (u32, u32) = (0, 148);
/// Where Codex is looked for besides `PATH`.
const PLACES: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// The program oxbot runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBinary {
    /// The native program.
    pub path: PathBuf,
    /// Its version, e.g. `0.148.0`.
    pub version: String,
}

/// What Settings → Engines shows about Codex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Found, and a version oxbot speaks.
    Ready(CodexBinary),
    /// Found, but not usable: why, in words for the person.
    Unusable {
        /// The program, if it was found.
        path: Option<PathBuf>,
        /// Its version, if it said.
        version: Option<String>,
        /// Why it can't run.
        why: String,
    },
}

impl Found {
    /// The program, if it can run.
    #[must_use]
    pub fn binary(&self) -> Option<&CodexBinary> {
        match self {
            Self::Ready(b) => Some(b),
            Self::Unusable { .. } => None,
        }
    }

    /// Why it can't run, if it can't.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        match self {
            Self::Ready(_) => None,
            Self::Unusable { why, .. } => Some(why),
        }
    }
}

/// `x.y.z` from `codex --version`'s `codex-cli x.y.z`.
#[must_use]
pub fn parse_version(output: &str) -> Option<String> {
    let v = output.split_whitespace().find(|w| {
        let mut parts = w.split('.');
        let num = |p: Option<&str>| p.is_some_and(|p| p.parse::<u32>().is_ok());
        let third = parts.clone().nth(2);
        num(parts.next())
            && num(parts.next())
            && third.is_some_and(|p| p.starts_with(|c: char| c.is_ascii_digit()))
    })?;
    Some(v.to_owned())
}

/// Whether oxbot speaks version `v`.
#[must_use]
pub fn supported(v: &str) -> bool {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().ok());
    matches!(
        (parts.next(), parts.next()),
        (Some(Some(major)), Some(Some(minor))) if (major, minor) == SUPPORTED
    )
}

/// The native program behind `path`: itself, or for npm's `codex.js` the
/// program its package ships for this Mac.
#[must_use]
pub fn native(path: &Path) -> PathBuf {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let is_script = std::fs::read(&real).is_ok_and(|b| b.starts_with(b"#!"));
    if !is_script {
        return real;
    }
    // .../@openai/codex/bin/codex.js → .../@openai/codex
    let Some(package) = real.parent().and_then(Path::parent) else {
        return real;
    };
    let (arch, triple) = if cfg!(target_arch = "aarch64") {
        ("arm64", "aarch64-apple-darwin")
    } else {
        ("x64", "x86_64-apple-darwin")
    };
    [
        package
            .join("node_modules/@openai")
            .join(format!("codex-darwin-{arch}"))
            .join("vendor")
            .join(triple)
            .join("bin/codex"),
        package.join("vendor").join(triple).join("bin/codex"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .unwrap_or(real)
}

/// Where `codex` is: `configured` (settings or `OXBOT_CODEX`), else `PATH`
/// and the usual Homebrew places.
#[must_use]
pub fn locate(configured: Option<&Path>, path_var: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = configured {
        return Some(p.to_path_buf());
    }
    path_var
        .unwrap_or("")
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .chain(PLACES.iter().map(PathBuf::from))
        .map(|d| d.join("codex"))
        .find(|p| p.is_file())
}

/// Finds Codex and checks its version. `scratch` is an empty folder it
/// may use as its home while it answers (never the person's `~/.codex`).
pub async fn find(configured: Option<&Path>, path_var: Option<&str>, scratch: &Path) -> Found {
    let Some(found) = locate(configured, path_var) else {
        return Found::Unusable {
            path: None,
            version: None,
            why: "The AI helper isn't installed on the home computer (oxbot looks in PATH, /opt/homebrew/bin and \
                  /usr/local/bin, or OXBOT_CODEX)."
                .to_owned(),
        };
    };
    let program = native(&found);
    let _ = std::fs::create_dir_all(scratch);
    let out = tokio::process::Command::new(&program)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch)
        .env("CODEX_HOME", scratch)
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(Duration::from_secs(20), out).await {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(Ok(_)) | Err(_) => {
            return Found::Unusable {
                path: Some(program),
                version: None,
                why: "The AI helper didn't say its version.".to_owned(),
            };
        }
        Ok(Err(err)) => {
            return Found::Unusable {
                path: Some(program),
                version: None,
                why: format!("The AI helper couldn't start: {err}"),
            };
        }
    };
    let Some(version) = parse_version(&String::from_utf8_lossy(&out.stdout)) else {
        return Found::Unusable {
            path: Some(program),
            version: None,
            why: "The AI helper didn't say its version.".to_owned(),
        };
    };
    if !supported(&version) {
        return Found::Unusable {
            path: Some(program),
            why: format!(
                "The AI helper {version} isn't supported yet: oxbot works with version {}.{}.x.",
                SUPPORTED.0, SUPPORTED.1
            ),
            version: Some(version),
        };
    }
    Found::Ready(CodexBinary {
        path: program,
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("codex-cli 0.148.0\n").as_deref(),
            Some("0.148.0")
        );
        assert_eq!(
            parse_version("codex-cli 0.148.2-alpha.1").as_deref(),
            Some("0.148.2-alpha.1")
        );
        assert_eq!(parse_version("nothing here"), None);
        assert!(supported("0.148.0"));
        assert!(supported("0.148.7"));
        assert!(!supported("0.149.0"));
        assert!(!supported("0.147.9"));
        assert!(!supported("1.148.0"));
        assert!(!supported("x"));
    }

    #[test]
    fn npm_s_script_leads_to_the_native_program() {
        let tmp = tempfile::tempdir().unwrap();
        let package = tmp.path().join("lib/node_modules/@openai/codex");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(package.join("bin/codex.js"), "#!/usr/bin/env node\n").unwrap();
        let (arch, triple) = if cfg!(target_arch = "aarch64") {
            ("arm64", "aarch64-apple-darwin")
        } else {
            ("x64", "x86_64-apple-darwin")
        };
        let bin = package.join(format!(
            "node_modules/@openai/codex-darwin-{arch}/vendor/{triple}/bin"
        ));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("codex"), b"\xcf\xfa\xed\xfe").unwrap();
        let link_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(package.join("bin/codex.js"), link_dir.join("codex")).unwrap();
        let found = locate(None, Some(link_dir.to_str().unwrap())).unwrap();
        assert_eq!(
            native(&found),
            std::fs::canonicalize(bin.join("codex")).unwrap()
        );
        // A native program is itself.
        assert_eq!(
            native(&bin.join("codex")),
            std::fs::canonicalize(bin.join("codex")).unwrap()
        );
    }

    #[tokio::test]
    async fn a_missing_or_wrong_program_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let none = find(Some(&tmp.path().join("nope")), None, &tmp.path().join("s")).await;
        assert!(
            none.problem().unwrap().contains("couldn't start"),
            "{none:?}"
        );
        let fake = tmp.path().join("codex");
        std::fs::write(&fake, "#!/bin/sh\necho codex-cli 0.200.1\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let wrong = find(Some(&fake), None, &tmp.path().join("s")).await;
        assert_eq!(
            wrong.problem(),
            Some("The AI helper 0.200.1 isn't supported yet: oxbot works with version 0.148.x.")
        );
        std::fs::write(&fake, "#!/bin/sh\necho codex-cli 0.148.3\n").unwrap();
        let ok = find(Some(&fake), None, &tmp.path().join("s")).await;
        assert_eq!(ok.binary().unwrap().version, "0.148.3");
    }
}
