//! oxbot's tool names as Codex takes them. Codex keeps every name that
//! starts with `mcp__` for its own MCP tools, so a dynamic tool can't be
//! called `mcp__chrome-devtools__click`: Codex gets the tool's own name
//! (`click`, `find_in_page`, `request_sign_in`), which is also how the
//! bot's instructions name them, and oxbot maps each call back to its
//! full name before the gate and the hub see it. Two tools with the same
//! own name keep a longer, still unique one.

use std::collections::HashMap;

use oxbot_llm::ToolSpec;
use serde_json::{Value, json};

/// Both ways between oxbot's names and Codex's.
#[derive(Debug, Clone, Default)]
pub struct ToolNames {
    to_codex: HashMap<String, String>,
    to_oxbot: HashMap<String, String>,
}

/// A name Codex takes: letters, digits, `_` and `-`, not starting with
/// `mcp`.
fn codex_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match cleaned.strip_prefix("mcp__") {
        Some(rest) => rest.replace("__", "_"),
        None if cleaned == "mcp" => "mcp_tool".to_owned(),
        None => cleaned,
    }
}

impl ToolNames {
    /// The names for `specs`.
    #[must_use]
    pub fn new(specs: &[ToolSpec]) -> Self {
        let own = |name: &str| name.rsplit("__").next().unwrap_or(name).to_owned();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for s in specs {
            *counts.entry(own(&s.name)).or_default() += 1;
        }
        let mut names = Self::default();
        for s in specs {
            let short = own(&s.name);
            let codex = if counts.get(&short) == Some(&1) {
                codex_safe(&short)
            } else {
                codex_safe(&s.name)
            };
            names.to_codex.insert(s.name.clone(), codex.clone());
            names.to_oxbot.insert(codex, s.name.clone());
        }
        names
    }

    /// oxbot's name for the tool Codex called `codex`.
    #[must_use]
    pub fn oxbot(&self, codex: &str) -> Option<&str> {
        self.to_oxbot.get(codex).map(String::as_str)
    }

    /// Codex's name for oxbot's tool `name`.
    #[must_use]
    pub fn codex(&self, name: &str) -> Option<&str> {
        self.to_codex.get(name).map(String::as_str)
    }

    /// `specs` as `thread/start`'s dynamic tools, without those Codex
    /// refused (`dropped`, by Codex's name).
    #[must_use]
    pub fn dynamic_tools(&self, specs: &[ToolSpec], dropped: &[String]) -> Vec<Value> {
        specs
            .iter()
            .filter_map(|s| {
                let name = self.codex(&s.name)?;
                (!dropped.iter().any(|d| d == name)).then(|| {
                    json!({
                        "type": "function",
                        "name": name,
                        "description": s.description,
                        "inputSchema": s.parameters,
                    })
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name}."),
            parameters: json!({"type": "object"}),
        }
    }

    #[test]
    fn own_names_both_ways_and_never_mcp() {
        let specs = vec![
            spec("mcp__chrome-devtools__click"),
            spec("mcp__chrome-devtools__take_snapshot"),
            spec("mcp__oxbot__find_in_page"),
            spec("mcp__oxbot__request_sign_in"),
            spec("Read"),
            spec("mcp__a__list"),
            spec("mcp__b__list"),
        ];
        let names = ToolNames::new(&specs);
        assert_eq!(names.codex("mcp__chrome-devtools__click"), Some("click"));
        assert_eq!(names.oxbot("click"), Some("mcp__chrome-devtools__click"));
        assert_eq!(
            names.oxbot("find_in_page"),
            Some("mcp__oxbot__find_in_page")
        );
        assert_eq!(names.codex("Read"), Some("Read"));
        // The same own name twice: longer names, still both ways.
        assert_eq!(names.codex("mcp__a__list"), Some("a_list"));
        assert_eq!(names.oxbot("b_list"), Some("mcp__b__list"));
        let tools = names.dynamic_tools(&specs, &["Read".to_owned()]);
        assert_eq!(tools.len(), 6);
        for t in &tools {
            assert!(!t["name"].as_str().unwrap().starts_with("mcp"), "{t}");
            assert_eq!(t["type"], "function");
        }
        assert_eq!(tools[0]["inputSchema"], json!({"type": "object"}));
    }
}
