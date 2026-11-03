//! The OpenAI-style wire format: request bodies out, answer chunks in.
//!
//! OpenRouter and local servers (Ollama, LM Studio) speak the same shape;
//! only OpenRouter gets its own fields (`models`, `provider`).

use serde_json::{Map, Value, json};

use crate::error::{ErrorKind, LlmError};
use crate::types::{
    CallUsage, ChatDelta, ChatRequest, FinishReason, Message, Part, Provider, Role, ToolChoice,
};

/// The JSON body of a streaming chat request.
#[must_use]
pub fn request_body(req: &ChatRequest, provider: Provider) -> Value {
    // Optional parameters go only where the model takes them (OpenRouter
    // with a known list); local servers get them all.
    let takes = |name: &str| match (&req.supported, provider) {
        (Some(names), Provider::OpenRouter) => names.iter().any(|n| n == name),
        _ => true,
    };
    let mut body = Map::new();
    body.insert("model".into(), Value::String(req.model.clone()));
    body.insert(
        "messages".into(),
        Value::Array(req.messages.iter().map(message).collect()),
    );
    if !req.tools.is_empty() {
        let tools = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        let choice = match &req.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Tool(name) => json!({"type": "function", "function": {"name": name}}),
        };
        if takes("tool_choice") {
            body.insert("tool_choice".into(), choice);
        }
        if let Some(parallel) = req
            .parallel_tool_calls
            .filter(|_| takes("parallel_tool_calls"))
        {
            body.insert("parallel_tool_calls".into(), Value::Bool(parallel));
        }
    }
    if let Some(max) = req.max_tokens.filter(|_| takes("max_tokens")) {
        body.insert("max_tokens".into(), json!(max));
    }
    if let Some(t) = req.temperature.filter(|_| takes("temperature")) {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(seed) = req.seed.filter(|_| takes("seed")) {
        body.insert("seed".into(), json!(seed));
    }
    body.insert("stream".into(), Value::Bool(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    if provider == Provider::OpenRouter {
        if !req.fallbacks.is_empty() {
            let models: Vec<&str> = std::iter::once(req.model.as_str())
                .chain(req.fallbacks.iter().map(String::as_str))
                .collect();
            body.insert("models".into(), json!(models));
        }
        if let Some(prefs) = &req.provider {
            body.insert(
                "provider".into(),
                serde_json::to_value(prefs).unwrap_or(Value::Null),
            );
        }
    }
    Value::Object(body)
}

fn message(m: &Message) -> Value {
    let mut out = Map::new();
    out.insert("role".into(), Value::String(m.role.as_str().into()));
    match m.role {
        Role::Assistant => {
            let text = m.text();
            out.insert(
                "content".into(),
                if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                },
            );
            if !m.tool_calls.is_empty() {
                let calls = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        // The model's own text, byte for byte, when it is a
                        // JSON object (a stable prefix keeps provider caches
                        // warm); `{}` when it was blank or invalid, since
                        // providers refuse invalid JSON in history.
                        let valid = !c.raw.trim().is_empty() && c.arguments_error().is_none();
                        let arguments = if valid { c.raw.clone() } else { "{}".to_owned() };
                        json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": arguments}})
                    })
                    .collect();
                out.insert("tool_calls".into(), Value::Array(calls));
            }
        }
        Role::System | Role::User | Role::Tool => {
            out.insert("content".into(), content(&m.parts));
            if let Some(id) = &m.tool_call_id {
                out.insert("tool_call_id".into(), Value::String(id.clone()));
            }
        }
    }
    Value::Object(out)
}

/// A plain string for one text part; an array of parts otherwise.
fn content(parts: &[Part]) -> Value {
    match parts {
        [] => Value::String(String::new()),
        [Part::Text(t)] => Value::String(t.clone()),
        parts => Value::Array(
            parts
                .iter()
                .map(|p| match p {
                    Part::Text(t) => json!({"type": "text", "text": t}),
                    Part::Image { mime, base64 } => {
                        json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{base64}")}})
                    }
                })
                .collect(),
        ),
    }
}

/// Reads streamed answer chunks and remembers what the last piece needs.
#[derive(Debug, Default)]
pub struct ChunkReader {
    model: String,
    generation_id: Option<String>,
    finish: Option<FinishReason>,
    usage: CallUsage,
    content: bool,
}

impl ChunkReader {
    /// A reader for one answer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any text or tool call has arrived.
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.content
    }

    /// Whether the model said why it stopped.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.finish.is_some()
    }

    /// Reads one `data:` chunk.
    ///
    /// # Errors
    /// The chunk isn't JSON, or it carries an error (mid-stream errors).
    pub fn chunk(&mut self, data: &str) -> Result<Vec<ChatDelta>, LlmError> {
        let v: Value = serde_json::from_str(data).map_err(|err| {
            LlmError::new(
                ErrorKind::Protocol,
                format!("The model service sent a broken chunk: {err}"),
            )
            .partial(self.content)
        })?;
        self.meta(&v);
        let choice = v.get("choices").and_then(|c| c.get(0));
        if let Some(error) = v.get("error").filter(|e| !e.is_null()) {
            return Err(LlmError::from_stream_error(error).partial(self.content));
        }
        let mut out = Vec::new();
        if let Some(choice) = choice {
            if let Some(delta) = choice.get("delta") {
                self.delta(delta, &mut out);
            }
            self.finish_reason(choice)?;
        }
        Ok(out)
    }

    /// Reads a whole non-streamed answer (servers that ignore `stream`).
    ///
    /// # Errors
    /// The body isn't an answer, or carries an error.
    pub fn whole(&mut self, body: &str) -> Result<Vec<ChatDelta>, LlmError> {
        let v: Value = serde_json::from_str(body).map_err(|err| {
            LlmError::new(
                ErrorKind::Protocol,
                format!("The model service sent a broken answer: {err}"),
            )
        })?;
        if let Some(error) = v.get("error").filter(|e| !e.is_null()) {
            return Err(LlmError::from_stream_error(error).partial(false));
        }
        self.meta(&v);
        let mut out = Vec::new();
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return Err(LlmError::new(
                ErrorKind::Protocol,
                "The model service sent an answer without choices.",
            ));
        };
        if let Some(message) = choice.get("message") {
            self.delta(message, &mut out);
        }
        self.finish_reason(choice)?;
        Ok(out)
    }

    /// The last piece: model, generation id, finish reason, usage. `None`
    /// if the model never said why it stopped.
    #[must_use]
    pub fn done(&self) -> Option<ChatDelta> {
        let finish = self.finish.clone()?;
        Some(ChatDelta::Done {
            model: self.model.clone(),
            generation_id: self.generation_id.clone(),
            finish,
            usage: self.usage,
        })
    }

    fn meta(&mut self, v: &Value) {
        if let Some(model) = v
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        {
            model.clone_into(&mut self.model);
        }
        if let Some(id) = v
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| !i.is_empty())
        {
            self.generation_id = Some(id.to_owned());
        }
        if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = read_usage(usage);
        }
    }

    fn delta(&mut self, delta: &Value, out: &mut Vec<ChatDelta>) {
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            self.content = true;
            out.push(ChatDelta::Text(text.to_owned()));
        }
        let calls = delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for (pos, call) in calls.iter().enumerate() {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or_else(|| u32::try_from(pos).unwrap_or(u32::MAX));
            let function = call.get("function");
            let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
            let args = match function.and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => s.clone(),
                // Some servers send arguments as an object.
                Some(v @ Value::Object(_)) => v.to_string(),
                _ => String::new(),
            };
            self.content = true;
            out.push(ChatDelta::ToolCallPart {
                index,
                id: text(call.get("id")),
                name: text(function.and_then(|f| f.get("name"))),
                args,
            });
        }
    }

    fn finish_reason(&mut self, choice: &Value) -> Result<(), LlmError> {
        let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) else {
            return Ok(());
        };
        let reason = FinishReason::from(reason);
        if reason == FinishReason::Error {
            return Err(
                LlmError::new(ErrorKind::Server, "The model stopped with an error.")
                    .partial(self.content),
            );
        }
        self.finish = Some(reason);
        Ok(())
    }
}

fn read_usage(u: &Value) -> CallUsage {
    let n = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
    CallUsage {
        prompt_tokens: n(u.get("prompt_tokens")),
        completion_tokens: n(u.get("completion_tokens")),
        cached_tokens: n(u
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))),
        reasoning_tokens: n(u
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))),
        cost_usd: u
            .get("cost")
            .and_then(Value::as_f64)
            .filter(|c| c.is_finite() && *c >= 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrouter::ProviderPrefs;
    use crate::types::{ToolCall, ToolSpec};

    fn request() -> ChatRequest {
        let mut req = ChatRequest::new(
            "a/model:free",
            vec![
                Message::system("rules"),
                Message::user("hello"),
                Message::assistant(
                    "",
                    vec![ToolCall::from_raw("c1", "Read", r#"{"file_path":"a.txt"}"#)],
                ),
                Message::tool("c1", vec![Part::Text("contents".into())]),
                Message::assistant("bad call", vec![ToolCall::from_raw("c2", "Read", "{oops")]),
                Message::tool(
                    "c2",
                    vec![
                        Part::Text("seen".into()),
                        Part::Image {
                            mime: "image/png".into(),
                            base64: "QUJD".into(),
                        },
                    ],
                ),
            ],
        );
        req.fallbacks = vec!["b/model:free".into()];
        req.tools = vec![ToolSpec {
            name: "Read".into(),
            description: "Reads a file".into(),
            parameters: json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
        }];
        req.max_tokens = Some(4096);
        req.provider = Some(ProviderPrefs::private_bot());
        req
    }

    #[test]
    fn openrouter_request_has_the_openai_shape_plus_routing() {
        let body = request_body(&request(), Provider::OpenRouter);
        assert_eq!(body["model"], "a/model:free");
        assert_eq!(body["models"], json!(["a/model:free", "b/model:free"]));
        assert_eq!(body["provider"]["data_collection"], "deny");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["tools"][0]["function"]["name"], "Read");
        let m = &body["messages"];
        assert_eq!(m[0], json!({"role": "system", "content": "rules"}));
        assert_eq!(m[1], json!({"role": "user", "content": "hello"}));
        assert_eq!(m[2]["content"], Value::Null);
        assert_eq!(
            m[2]["tool_calls"][0],
            json!({
                "id": "c1", "type": "function", "function": {"name": "Read", "arguments": "{\"file_path\":\"a.txt\"}"}
            })
        );
        assert_eq!(
            m[3],
            json!({"role": "tool", "content": "contents", "tool_call_id": "c1"})
        );
        assert_eq!(m[4]["content"], "bad call");
        assert_eq!(m[4]["tool_calls"][0]["function"]["arguments"], "{}");
        assert_eq!(
            m[5]["content"][1]["image_url"]["url"],
            "data:image/png;base64,QUJD"
        );
        assert!(body.get("user").is_none(), "no person data in requests");
        assert!(body.get("temperature").is_none());
        assert!(body.get("seed").is_none());
    }

    #[test]
    fn raw_arguments_are_resent_byte_for_byte() {
        let raw = "{ \"b\": 1,  \"a\": [2] }";
        let mut req = ChatRequest::new(
            "m",
            vec![Message::assistant(
                "",
                vec![
                    ToolCall::from_raw("c1", "x", raw),
                    ToolCall::from_raw("c2", "y", ""),
                ],
            )],
        );
        req.temperature = Some(0.5);
        req.seed = Some(7);
        let body = request_body(&req, Provider::OpenRouter);
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["arguments"],
            raw
        );
        assert_eq!(
            body["messages"][0]["tool_calls"][1]["function"]["arguments"],
            "{}"
        );
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["seed"], 7);
    }

    #[test]
    fn only_parameters_the_model_takes_are_sent() {
        let mut req = request();
        req.temperature = Some(0.2);
        req.seed = Some(7);
        req.supported = Some(vec!["tools".into(), "max_tokens".into(), "seed".into()]);
        let body = request_body(&req, Provider::OpenRouter);
        assert_eq!(body["tools"][0]["function"]["name"], "Read");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["seed"], 7);
        for left_out in ["tool_choice", "parallel_tool_calls", "temperature"] {
            assert!(body.get(left_out).is_none(), "{left_out}");
        }
        assert_eq!(body["provider"]["require_parameters"], true);

        req.supported = Some(Vec::new());
        let body = request_body(&req, Provider::OpenRouter);
        assert!(body.get("tools").is_some(), "tools are always sent");
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["stream"], true);

        let local = request_body(&req, Provider::Local);
        assert_eq!(local["parallel_tool_calls"], false, "local servers get all");
        assert_eq!(local["max_tokens"], 4096);
    }

    #[test]
    fn local_request_has_no_openrouter_fields() {
        let body = request_body(&request(), Provider::Local);
        assert!(body.get("models").is_none());
        assert!(body.get("provider").is_none());
        assert_eq!(body["model"], "a/model:free");
    }

    #[test]
    fn no_tools_means_no_tool_fields() {
        let req = ChatRequest::new("m", vec![Message::user("hi")]);
        let body = request_body(&req, Provider::OpenRouter);
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
        assert!(body.get("models").is_none());
        assert!(body.get("provider").is_none());
    }

    #[test]
    fn chunks_become_deltas_and_a_done() {
        let mut r = ChunkReader::new();
        let first = r
            .chunk(r#"{"id":"gen-1","model":"a/model:free","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"}}]}"#)
            .unwrap();
        assert_eq!(first, vec![ChatDelta::Text("Hi".into())]);
        let call = r
            .chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"Read","arguments":"{\"fi"}}]}}]}"#)
            .unwrap();
        assert_eq!(
            call,
            vec![ChatDelta::ToolCallPart {
                index: 0,
                id: Some("c1".into()),
                name: Some("Read".into()),
                args: "{\"fi".into()
            }]
        );
        assert!(r.done().is_none());
        r.chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
            .unwrap();
        r.chunk(r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":64},"completion_tokens_details":{"reasoning_tokens":5},"cost":0}}"#).unwrap();
        assert_eq!(
            r.done().unwrap(),
            ChatDelta::Done {
                model: "a/model:free".into(),
                generation_id: Some("gen-1".into()),
                finish: FinishReason::ToolCalls,
                usage: CallUsage {
                    prompt_tokens: 100,
                    completion_tokens: 20,
                    cached_tokens: 64,
                    reasoning_tokens: 5,
                    cost_usd: Some(0.0)
                },
            }
        );
    }

    #[test]
    fn mid_stream_error_chunk_is_an_error() {
        let mut r = ChunkReader::new();
        r.chunk(r#"{"choices":[{"delta":{"content":"par"}}]}"#)
            .unwrap();
        let err = r
            .chunk(r#"{"id":"gen-abc","model":"openai/gpt-4o","error":{"code":429,"message":"Rate limit exceeded","metadata":{"error_type":"rate_limit_exceeded"}},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::RateLimited);
        assert!(err.partial);

        let mut r = ChunkReader::new();
        let err = r
            .chunk(r#"{"choices":[{"delta":{},"finish_reason":"error"}]}"#)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Server);
        assert!(!err.partial);
    }

    #[test]
    fn broken_chunk_is_a_protocol_error() {
        let err = ChunkReader::new().chunk("{not json").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Protocol);
    }

    #[test]
    fn whole_answers_from_servers_that_ignore_stream() {
        let mut r = ChunkReader::new();
        let deltas = r
            .whole(r#"{"id":"x","model":"llama","choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"c","type":"function","function":{"name":"Glob","arguments":{"pattern":"*"}}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#)
            .unwrap();
        assert_eq!(
            deltas,
            vec![ChatDelta::ToolCallPart {
                index: 0,
                id: Some("c".into()),
                name: Some("Glob".into()),
                args: r#"{"pattern":"*"}"#.into()
            }]
        );
        let Some(ChatDelta::Done { model, usage, .. }) = r.done() else {
            panic!("no done")
        };
        assert_eq!(model, "llama");
        assert_eq!(usage.cost_usd, None);
    }
}
