//! The browser tool, as the loop uses it: `chrome-devtools-mcp` (or a
//! stand-in) behind [`BrowserTools`], one per session, on the bot's
//! owner's Chrome. The model sees its tools as
//! `mcp__chrome-devtools__<name>`, the same names as on Claude.

use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use oxbot_engine::BrowserSpec;
use oxbot_llm::ToolSpec;
use serde_json::Value;

/// The browser tool's prefix, as models see its tools.
pub const BROWSER_PREFIX: &str = "mcp__chrome-devtools__";

/// Browser tools that change something on a page (not re-run after a
/// restart; their result is saved before anything else happens).
pub const SIDE_EFFECT_TOOLS: [&str; 8] = [
    "click",
    "fill",
    "fill_form",
    "type_text",
    "press_key",
    "upload_file",
    "drag",
    "handle_dialog",
];

/// What one browser tool call returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserOutput {
    /// Its text blocks, joined.
    pub text: String,
    /// How many images it returned (not passed on to models).
    pub images: usize,
    /// The tool said it failed.
    pub is_error: bool,
}

/// One session's browser tool.
pub trait BrowserTools: Send + Sync + fmt::Debug {
    /// Its tools, with their own names (e.g. `click`), as it lists them.
    fn specs(&self) -> BoxFuture<'_, Result<Vec<ToolSpec>, String>>;

    /// Runs tool `name` with `args` (a JSON object). Dropping the future
    /// cancels the call.
    fn call(&self, name: &str, args: Value) -> BoxFuture<'_, Result<BrowserOutput, String>>;

    /// Stops it (its process, if any).
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

/// Makes a session's browser tool.
pub trait BrowserFactory: Send + Sync + fmt::Debug {
    /// The browser tool for `spec`. `env` is the host's environment for
    /// child processes (e.g. a `PATH` that finds Node.js).
    fn open(&self, spec: &BrowserSpec, env: &[(String, String)]) -> Arc<dyn BrowserTools>;
}

/// The tools a model gets from `listed`: only names in `allow`, sorted by
/// name, with the browser prefix, and without schema keywords some
/// providers refuse.
#[must_use]
pub fn model_specs(listed: Vec<ToolSpec>, allow: &[String]) -> Vec<ToolSpec> {
    let mut specs: Vec<ToolSpec> = listed
        .into_iter()
        .filter(|t| allow.contains(&t.name))
        .map(|mut t| {
            strip_schema(&mut t.parameters);
            t.name = format!("{BROWSER_PREFIX}{}", t.name);
            t
        })
        .collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    specs.dedup_by(|a, b| a.name == b.name);
    specs
}

/// Removes `$schema` and `$comment` everywhere in a schema.
fn strip_schema(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("$schema");
            map.remove("$comment");
            for child in map.values_mut() {
                strip_schema(child);
            }
        }
        Value::Array(list) => list.iter_mut().for_each(strip_schema),
        _ => {}
    }
}

#[cfg(any(test, feature = "test-tools"))]
pub use scripted::ScriptedBrowser;

#[cfg(any(test, feature = "test-tools"))]
mod scripted {
    use std::sync::{Arc, Mutex, PoisonError};

    use futures_util::future::BoxFuture;
    use oxbot_engine::BrowserSpec;
    use oxbot_llm::ToolSpec;
    use serde_json::{Value, json};

    use super::{BrowserFactory, BrowserOutput, BrowserTools};

    type Responder = Box<dyn Fn(&str, &Value) -> BrowserOutput + Send + Sync>;

    /// A browser tool that answers from a script, for tests. Every call is
    /// kept, so tests can check what ran (and what didn't).
    pub struct ScriptedBrowser {
        specs: Vec<ToolSpec>,
        responder: Responder,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl std::fmt::Debug for ScriptedBrowser {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ScriptedBrowser")
        }
    }

    impl ScriptedBrowser {
        /// A browser listing the usual tools, answering with `responder`.
        #[must_use]
        pub fn new(
            responder: impl Fn(&str, &Value) -> BrowserOutput + Send + Sync + 'static,
        ) -> Arc<Self> {
            let names = [
                "click",
                "fill",
                "fill_form",
                "list_pages",
                "navigate_page",
                "new_page",
                "press_key",
                "select_page",
                "take_screenshot",
                "take_snapshot",
                "type_text",
                "upload_file",
                "wait_for",
                "evaluate_script",
            ];
            let specs = names
                .iter()
                .map(|n| ToolSpec {
                    name: (*n).to_owned(),
                    description: format!("The {n} tool."),
                    parameters: json!({"$schema": "x", "type": "object", "properties": {}}),
                })
                .collect();
            Arc::new(Self {
                specs,
                responder: Box::new(responder),
                calls: Mutex::new(Vec::new()),
            })
        }

        /// A browser whose snapshot is `snapshot` and whose other tools say
        /// they're done.
        #[must_use]
        pub fn with_snapshot(snapshot: impl Into<String>) -> Arc<Self> {
            let snapshot = snapshot.into();
            Self::new(move |name, _| BrowserOutput {
                text: if name == "take_snapshot" {
                    snapshot.clone()
                } else {
                    format!("Done: {name}")
                },
                ..BrowserOutput::default()
            })
        }

        /// Every call so far: (tool, arguments).
        #[must_use]
        pub fn calls(&self) -> Vec<(String, Value)> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl BrowserTools for ScriptedBrowser {
        fn specs(&self) -> BoxFuture<'_, Result<Vec<ToolSpec>, String>> {
            let specs = self.specs.clone();
            Box::pin(async move { Ok(specs) })
        }

        fn call(&self, name: &str, args: Value) -> BoxFuture<'_, Result<BrowserOutput, String>> {
            let out = (self.responder)(name, &args);
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((name.to_owned(), args));
            Box::pin(async move { Ok(out) })
        }

        fn shutdown(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    /// Every session gets the same scripted browser.
    #[derive(Debug)]
    pub struct SameBrowser(pub Arc<ScriptedBrowser>);

    impl BrowserFactory for SameBrowser {
        fn open(&self, _: &BrowserSpec, _: &[(String, String)]) -> Arc<dyn BrowserTools> {
            Arc::clone(&self.0) as Arc<dyn BrowserTools>
        }
    }
}

#[cfg(any(test, feature = "test-tools"))]
pub use scripted::SameBrowser;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_allowed_tools_sorted_and_prefixed() {
        let listed = vec![
            ToolSpec {
                name: "take_snapshot".into(),
                description: String::new(),
                parameters: json!({"$schema": "x", "type": "object",
                    "properties": {"a": {"$comment": "c", "type": "string"}}}),
            },
            ToolSpec {
                name: "evaluate_script".into(),
                description: String::new(),
                parameters: json!({}),
            },
            ToolSpec {
                name: "click".into(),
                description: String::new(),
                parameters: json!({}),
            },
        ];
        let allow = vec!["click".to_owned(), "take_snapshot".to_owned()];
        let specs = model_specs(listed, &allow);
        let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "mcp__chrome-devtools__click",
                "mcp__chrome-devtools__take_snapshot"
            ]
        );
        assert_eq!(
            specs[1].parameters,
            json!({"type": "object", "properties": {"a": {"type": "string"}}})
        );
    }
}
