//! The `config.toml` oxbot writes into a Codex process's own CODEX_HOME
//! every time it starts it (never edited by hand; a person's own
//! `~/.codex` is never read or used).
//!
//! It turns off everything that would reach the network on its own
//! (update checks, analytics, feedback, metrics, plugins and their sync,
//! apps, web search), every built-in tool (shell, images, plans, goals,
//! skills, sub-agents, asking the person: oxbot's own tools do all of
//! that), and every block of Codex's own instructions (environment,
//! permissions, project docs): a household bot, not a coding agent in a
//! repo. Its only model provider is oxbot's relay on 127.0.0.1, with the
//! process's token in its environment (`OXBOT_RELAY_TOKEN`), never here.
//! Commands would need approval and there is no shell anyway, so Codex
//! never asks for one; the sandbox is read-only with no network.

use std::io::Write as _;
use std::path::Path;

/// The environment variable that holds the relay token.
pub const TOKEN_ENV: &str = "OXBOT_RELAY_TOKEN";

/// The config for a process whose relay is at `relay_base`
/// (`http://127.0.0.1:<port>/v1`).
#[must_use]
pub fn config_toml(relay_base: &str) -> String {
    format!(
        r#"# Written by oxbot each time it starts this Codex process; changes are
# overwritten. Only oxbot's relay is contacted.
model_provider = "oxbot"
check_for_update_on_startup = false
web_search = "disabled"
approval_policy = "never"
sandbox_mode = "read-only"
project_doc_max_bytes = 0
include_environment_context = false
include_permissions_instructions = false
include_apps_instructions = false
include_collaboration_mode_instructions = false

[model_providers.oxbot]
name = "oxbot relay"
base_url = "{relay_base}"
env_key = "{TOKEN_ENV}"
wire_api = "responses"

[analytics]
enabled = false

[feedback]
enabled = false

[otel]
metrics_exporter = "none"

[history]
persistence = "none"

[skills]
include_instructions = false

[skills.bundled]
enabled = false

[tools.update_plan]
enabled = false

[tools.experimental_request_user_input]
enabled = false

[sandbox_workspace_write]
network_access = false

[features]
plugins = false
apps = false
memories = false
multi_agent = false
image_generation = false
shell_tool = false
view_image = false
goals = false
shell_snapshot = false
hooks = false
tool_suggest = false
"#
    )
}

/// Writes `text` as `home/config.toml`, readable only by this Mac's user,
/// all at once; `home` is made owner-only too.
///
/// # Errors
/// The folder or file couldn't be written.
pub fn write(home: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home)?;
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    let tmp = home.join("config.toml.part");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, home.join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes: any change here is a change to what Codex may do.
    const GOLDEN: &str = include_str!("config.golden.toml");

    #[test]
    fn the_config_is_exactly_the_golden_one() {
        assert_eq!(config_toml("http://127.0.0.1:41234/v1"), GOLDEN);
    }

    #[test]
    fn nothing_that_reaches_out_is_left_on() {
        let text = config_toml("http://127.0.0.1:1/v1");
        let parsed: toml_like::Doc = toml_like::parse(&text);
        assert_eq!(parsed.get("model_provider"), Some("\"oxbot\""));
        assert_eq!(parsed.get("check_for_update_on_startup"), Some("false"));
        assert_eq!(parsed.get("web_search"), Some("\"disabled\""));
        assert_eq!(parsed.get("analytics.enabled"), Some("false"));
        assert_eq!(parsed.get("otel.metrics_exporter"), Some("\"none\""));
        for f in [
            "plugins",
            "apps",
            "memories",
            "multi_agent",
            "shell_tool",
            "image_generation",
        ] {
            assert_eq!(parsed.get(&format!("features.{f}")), Some("false"), "{f}");
        }
        assert_eq!(
            parsed.get("model_providers.oxbot.base_url"),
            Some("\"http://127.0.0.1:1/v1\"")
        );
        // The token is never written down.
        assert!(!text.contains("Bearer"));
        assert!(!text.contains("experimental_bearer_token"));
        assert!(!text.contains("http_headers"));
    }

    #[test]
    fn written_owner_only_and_replaced_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("codex");
        write(&home, "a = 1\n").unwrap();
        write(&home, "b = 2\n").unwrap();
        let file = home.join("config.toml");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "b = 2\n");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(!home.join("config.toml.part").exists());
    }

    /// Just enough TOML reading for the checks above: `table.key = value`.
    mod toml_like {
        use std::collections::HashMap;

        pub struct Doc(HashMap<String, String>);

        impl Doc {
            pub fn get(&self, key: &str) -> Option<&str> {
                self.0.get(key).map(String::as_str)
            }
        }

        pub fn parse(text: &str) -> Doc {
            let mut table = String::new();
            let mut out = HashMap::new();
            for line in text.lines().map(str::trim) {
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                if let Some(t) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                    table = format!("{t}.");
                    continue;
                }
                if let Some((k, v)) = line.split_once(" = ") {
                    out.insert(format!("{table}{k}"), v.to_owned());
                }
            }
            Doc(out)
        }
    }
}
