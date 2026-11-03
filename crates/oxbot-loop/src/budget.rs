//! Token estimates, calibrated per model from what providers report.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use oxbot_llm::{Message, Part, ToolSpec};

/// Characters per token before any calibration (a common rule of thumb
/// for English and code).
const CHARS_PER_TOKEN: f64 = 3.5;
/// Tokens counted for an image part.
const IMAGE_TOKENS: u64 = 1_000;
/// Tokens each message adds for its framing.
const MESSAGE_OVERHEAD: u64 = 4;

/// Estimated tokens of `text`.
#[must_use]
pub fn est(text: &str) -> u64 {
    tokens_of_chars(text.chars().count())
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn tokens_of_chars(chars: usize) -> u64 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u64
}

/// Estimated tokens of one message.
#[must_use]
pub fn est_message(m: &Message) -> u64 {
    let parts: u64 = m
        .parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => est(t),
            Part::Image { .. } => IMAGE_TOKENS,
        })
        .sum();
    let calls: u64 = m
        .tool_calls
        .iter()
        .map(|c| est(&c.name) + est(&c.raw))
        .sum();
    parts + calls + MESSAGE_OVERHEAD
}

/// Estimated tokens of tool definitions.
#[must_use]
pub fn est_tools(tools: &[ToolSpec]) -> u64 {
    tools
        .iter()
        .map(|t| est(&t.name) + est(&t.description) + est(&t.parameters.to_string()))
        .sum()
}

/// Per-model factors: what a provider counted over what we estimated.
#[derive(Debug, Default)]
pub struct Calibration(Mutex<HashMap<String, f64>>);

impl Calibration {
    /// Notes that `model` counted `actual` prompt tokens where we
    /// estimated `estimated`.
    #[allow(clippy::cast_precision_loss)]
    pub fn observe(&self, model: &str, estimated: u64, actual: u64) {
        if estimated < 100 || actual == 0 {
            return;
        }
        let ratio = (actual as f64 / estimated as f64).clamp(0.25, 4.0);
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let factor = map.entry(model.to_owned()).or_insert(ratio);
        *factor = *factor * 0.7 + ratio * 0.3;
    }

    /// `estimated` as `model` would count it (never less than estimated
    /// by more than half).
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn adjust(&self, model: &str, estimated: u64) -> u64 {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match map.get(model) {
            Some(f) => (estimated as f64 * f.max(0.5)).ceil() as u64,
            None => estimated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_grow_with_text() {
        assert_eq!(est(""), 0);
        assert!(est("hello world, this is a test") >= 7);
        let m = Message::user("x".repeat(350));
        assert_eq!(est_message(&m), 104);
    }

    #[test]
    fn calibration_moves_toward_what_the_model_counts() {
        let c = Calibration::default();
        assert_eq!(c.adjust("m", 1000), 1000);
        c.observe("m", 1000, 2000);
        assert_eq!(c.adjust("m", 1000), 2000);
        c.observe("m", 1000, 1000);
        assert!(c.adjust("m", 1000) < 2000);
        // Tiny requests teach nothing.
        c.observe("n", 10, 1000);
        assert_eq!(c.adjust("n", 1000), 1000);
    }
}
