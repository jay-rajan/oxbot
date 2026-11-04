//! Engines: what runs a bot. The shared types and traits come from
//! `oxbot-engine` (re-exported here); the engine is `oxbot-codex`, with
//! the tools, gate and model requests from `oxbot-loop`.

pub use oxbot_engine::*;

/// `kind` as the protocol names it.
#[must_use]
pub fn to_wire(kind: EngineKind) -> oxbot_protocol::EngineKind {
    match kind {
        EngineKind::Codex => oxbot_protocol::EngineKind::Codex,
    }
}

/// The engine the protocol's `kind` names.
#[must_use]
pub fn from_wire(kind: oxbot_protocol::EngineKind) -> EngineKind {
    match kind {
        oxbot_protocol::EngineKind::Codex => EngineKind::Codex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_go_to_the_wire_and_back() {
        for kind in EngineKind::ALL {
            assert_eq!(from_wire(to_wire(kind)), kind);
            assert_eq!(
                serde_json::to_value(to_wire(kind)).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
    }
}
