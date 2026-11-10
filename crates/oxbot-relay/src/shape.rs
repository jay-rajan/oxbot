//! What leaves the Mac for one relayed request: a body rebuilt from an
//! allow list, never the request as Codex sent it.
//!
//! - Kept: the conversation (`input`), the instructions (oxbot's own
//!   prompt), the function tools oxbot gave the bot (never Codex's own,
//!   such as `apply_patch`: see [`offered`]), and `stream`.
//! - Set by oxbot: the model and its fallbacks, the provider's privacy
//!   rules, and the longest answer.
//! - Kept only when the model takes them: `tool_choice`,
//!   `parallel_tool_calls`, `temperature`, `top_p`, `reasoning` (and with
//!   it `include`).
//! - Everything else goes: `metadata`, `client_metadata` (Codex's
//!   installation, session, thread and turn ids), `user`,
//!   `safety_identifier`, `prompt_cache_key` (OpenRouter keeps a
//!   conversation on one provider for its cache without it), `store`,
//!   `previous_response_id`, `service_tier`, `prompt`, `text`, and any
//!   field this list doesn't know.
//! - Message and tool-output items lose their ids (Codex makes them; the
//!   provider needs only `call_id` to pair a call with its result).
//!
//! Headers are never copied: the forwarded request carries only the key,
//! the content type and a fixed User-Agent ([`oxbot_llm::RELAY_USER_AGENT`]).

use oxbot_loop::gateway::Attempt;
use serde_json::{Map, Value, json};

/// Fields always kept, as Codex sent them.
const KEPT: [&str; 3] = ["input", "instructions", "stream"];

/// Optional fields, kept only when the model takes the named parameter
/// (or nothing is known about the model).
const OPTIONAL: [(&str, &str); 6] = [
    ("tool_choice", "tool_choice"),
    ("parallel_tool_calls", "parallel_tool_calls"),
    ("temperature", "temperature"),
    ("top_p", "top_p"),
    ("reasoning", "reasoning"),
    ("include", "reasoning"),
];

/// Input item types whose `id` Codex wrote itself.
const OWN_IDS: [&str; 2] = ["message", "function_call_output"];

/// The function tools in `request` that are oxbot's (named in `tools`, as
/// Codex knows them), in order: the only ones offered to the model, and
/// the only ones its answer may call (see [`crate::calls`]).
#[must_use]
pub fn offered(request: &Map<String, Value>, tools: &[String]) -> Vec<String> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| t.get("type").and_then(Value::as_str) == Some("function"))
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .filter(|name| tools.iter().any(|t| t == name))
        .map(str::to_owned)
        .collect()
}

/// The body to send for `request` on `attempt`, offering only oxbot's
/// `tools` (see [`offered`]).
#[must_use]
pub fn outgoing(
    request: &Map<String, Value>,
    attempt: &Attempt,
    tools: &[String],
) -> Map<String, Value> {
    let takes = |param: &str| {
        attempt
            .supported
            .as_ref()
            .is_none_or(|list| list.iter().any(|p| p == param))
    };
    let mut out = Map::new();
    out.insert("model".into(), Value::String(attempt.model.clone()));
    if !attempt.fallbacks.is_empty() {
        let models: Vec<&str> = std::iter::once(attempt.model.as_str())
            .chain(attempt.fallbacks.iter().map(String::as_str))
            .collect();
        out.insert("models".into(), json!(models));
    }
    for key in KEPT {
        if let Some(v) = request.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if let Some(Value::Array(input)) = out.get_mut("input") {
        for item in input.iter_mut() {
            if let Some(map) = item.as_object_mut()
                && map
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| OWN_IDS.contains(&t))
            {
                map.remove("id");
            }
        }
    }
    if let Some(Value::Array(sent)) = request.get("tools") {
        let ours = offered(request, tools);
        let kept: Vec<Value> = sent
            .iter()
            .filter(|t| t.get("type").and_then(Value::as_str) == Some("function"))
            .filter(|t| {
                t.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| ours.iter().any(|o| o == name))
            })
            .cloned()
            .collect();
        if !kept.is_empty() {
            out.insert("tools".into(), Value::Array(kept));
        }
    }
    let has_tools = out.contains_key("tools");
    for (key, param) in OPTIONAL {
        let tool_field = matches!(key, "tool_choice" | "parallel_tool_calls");
        if let Some(v) = request.get(key)
            && takes(param)
            && (has_tools || !tool_field)
        {
            out.insert(key.into(), v.clone());
        }
    }
    if takes("max_tokens") {
        out.insert("max_output_tokens".into(), json!(attempt.max_tokens));
    }
    if let Some(prefs) = &attempt.provider {
        out.insert(
            "provider".into(),
            serde_json::to_value(prefs).unwrap_or(Value::Null),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_llm::ProviderPrefs;

    /// A request as Codex 0.148 sent it (captured against a stand-in).
    fn codex_request() -> Map<String, Value> {
        let v = json!({
            "model": "e2e/model:free",
            "instructions": "You are \"Scout\", a bot running inside oxbot.",
            "input": [
                {"type": "message", "id": "msg_01a1", "role": "user",
                 "content": [{"type": "input_text", "text": "[Sam] search jobs"}]},
                {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                 "name": "mcp__chrome-devtools__navigate_page", "arguments": "{}"},
                {"type": "function_call_output", "id": "fco_01a1", "call_id": "call_1",
                 "output": "<web_page url=\"https://jobs.example.com/\">Jobs</web_page>"}
            ],
            "tools": [
                {"type": "function", "name": "mcp__chrome-devtools__navigate_page",
                 "description": "Go", "strict": false, "parameters": {"type": "object"}},
                {"type": "web_search"},
                {"type": "custom", "name": "apply_patch", "description": "Edit files"},
                {"type": "function", "name": "exec_command", "description": "Run",
                 "parameters": {"type": "object"}}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": {"summary": "auto"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "01a1a7a1-952f-7331-9a6f-c38e16705784",
            "client_metadata": {"session_id": "01a1", "x-codex-turn-metadata":
                "{\"installation_id\":\"01ff536e\"}"},
            "metadata": {"user": "sam"},
            "user": "sam",
            "safety_identifier": "x",
            "service_tier": "flex",
            "previous_response_id": "resp_1",
            "text": {"verbosity": "low"},
            "something_new": 1
        });
        v.as_object().unwrap().clone()
    }

    /// oxbot's tools in [`codex_request`].
    fn ours() -> Vec<String> {
        vec!["mcp__chrome-devtools__navigate_page".to_owned()]
    }

    fn attempt(supported: Option<&[&str]>) -> Attempt {
        Attempt {
            model: "e2e/tool-model:free".into(),
            fallbacks: vec!["e2e/backup:free".into()],
            max_tokens: 4096,
            supported: supported.map(|s| s.iter().map(|p| (*p).to_owned()).collect()),
            provider: Some(ProviderPrefs::private_bot()),
            free: true,
        }
    }

    #[test]
    fn nothing_identifying_leaves_and_oxbot_picks_the_model() {
        let out = outgoing(
            &codex_request(),
            &attempt(Some(&["tools", "tool_choice", "max_tokens"])),
            &ours(),
        );
        let text = Value::Object(out.clone()).to_string();
        for gone in [
            "prompt_cache_key",
            "client_metadata",
            "metadata",
            "\"user\":",
            "safety_identifier",
            "service_tier",
            "previous_response_id",
            "\"store\"",
            "installation_id",
            "01a1a7a1",
            "msg_01a1",
            "fco_01a1",
            "verbosity",
            "something_new",
            "web_search",
            "reasoning",
            "parallel_tool_calls",
            "e2e/model:free",
            "apply_patch",
            "exec_command",
        ] {
            assert!(!text.contains(gone), "{gone} left: {text}");
        }
        assert_eq!(out["model"], "e2e/tool-model:free");
        assert_eq!(
            out["models"],
            json!(["e2e/tool-model:free", "e2e/backup:free"])
        );
        assert_eq!(out["max_output_tokens"], 4096);
        assert_eq!(out["tool_choice"], "auto");
        assert_eq!(out["provider"]["data_collection"], "deny");
        assert_eq!(out["stream"], true);
        // The call and its result still pair up.
        assert_eq!(out["input"][1]["call_id"], "call_1");
        assert_eq!(out["input"][2]["call_id"], "call_1");
        assert_eq!(out["input"][1]["id"], "fc_1");
        assert_eq!(out["tools"].as_array().unwrap().len(), 1);
        let keys: Vec<&str> = out.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "input",
                "instructions",
                "max_output_tokens",
                "model",
                "models",
                "provider",
                "stream",
                "tool_choice",
                "tools"
            ]
        );
    }

    #[test]
    fn an_unknown_model_gets_the_optional_fields_but_never_the_ids() {
        let out = outgoing(&codex_request(), &attempt(None), &ours());
        assert_eq!(out["reasoning"], json!({"summary": "auto"}));
        assert_eq!(out["parallel_tool_calls"], true);
        assert!(!out.contains_key("prompt_cache_key"));
        assert!(!out.contains_key("client_metadata"));
    }

    #[test]
    fn only_oxbots_own_tools_are_offered() {
        let request = codex_request();
        assert_eq!(offered(&request, &ours()), ours());
        // Codex's own tools never, even as functions.
        let wanted = vec!["exec_command".to_owned(), "apply_patch".to_owned()];
        assert_eq!(offered(&request, &wanted), ["exec_command"]);
        let out = outgoing(&request, &attempt(None), &[]);
        assert!(!out.contains_key("tools"), "no tools of oxbot's: none");
        assert!(!out.contains_key("tool_choice") && !out.contains_key("parallel_tool_calls"));
    }
}
