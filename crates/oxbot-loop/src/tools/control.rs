//! The loop's own tools: `ask_user`, which ends the turn with a question
//! for the person.

use oxbot_llm::ToolSpec;
use serde_json::json;

/// The name models call it by.
pub const ASK_USER: &str = "mcp__oxbot__ask_user";

/// `ask_user` as the model sees it.
#[must_use]
pub fn ask_user_spec() -> ToolSpec {
    ToolSpec {
        name: ASK_USER.to_owned(),
        description:
            "Ask the person one short question when you can't go on without their answer. \
            Your turn ends; their reply comes as the next message."
                .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The question, in one or two sentences."}
            },
            "required": ["question"]
        }),
    }
}
