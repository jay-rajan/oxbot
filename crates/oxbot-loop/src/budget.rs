//! Token estimates and budgets.
//!
//! The estimate is deliberately on the high side: a byte count over three
//! (English runs near four characters a token, page snapshots near three,
//! and a three-byte CJK character is about one token). Providers report
//! what they really counted; each model's ratio of that to the estimate is
//! kept ([`Calibration`]) and saved, so budgets follow the model's own
//! tokenizer without bundling any.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use oxbot_llm::{Message, Part, ToolSpec};

use crate::store::{CalibrationRow, LoopStore};

/// Bytes per estimated token.
const BYTES_PER_TOKEN: u64 = 3;
/// Tokens counted for an image part.
const IMAGE_TOKENS: u64 = 1_000;
/// Tokens each message adds for its framing.
pub const MESSAGE_OVERHEAD: u64 = 4;

/// Estimated tokens of `text`.
#[must_use]
pub fn est(text: &str) -> u64 {
    (text.len() as u64).div_ceil(BYTES_PER_TOKEN)
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
        .map(|c| est(&c.name) + est(&c.raw) + est(&c.id))
        .sum();
    parts + calls + MESSAGE_OVERHEAD
}

/// Estimated tokens of tool definitions, counted once per request.
#[must_use]
pub fn est_tools(tools: &[ToolSpec]) -> u64 {
    tools
        .iter()
        .map(|t| est(&t.name) + est(&t.description) + est(&t.parameters.to_string()))
        .sum()
}

/// `tokens` with `ratio` applied, rounded up.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn scaled(tokens: u64, ratio: f64) -> u64 {
    (tokens as f64 * ratio).ceil() as u64
}

/// How many tokens the context may use, for one request.
///
/// From the window `W` of the smallest model that may answer:
/// `reserve` is kept for the answer, `margin = W/20` for estimate error,
/// and the rest, `U`, is what a request may use. Every threshold is a
/// share of `U` (see the design notes on each method).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    /// The model's window (the smallest of the models that may answer).
    pub window: u64,
    /// Kept free for the answer.
    pub reserve: u64,
}

impl TokenBudget {
    /// The budget for a window of `window` tokens whose answers are at most
    /// `max_completion` tokens (8,192 when unknown), with the reserve
    /// between 2,048 and a quarter of the window.
    #[must_use]
    pub fn for_model(window: u64, max_completion: Option<u32>) -> Self {
        let wanted = u64::from(max_completion.unwrap_or(8_192));
        let reserve = wanted.max(2_048).min(window / 4).max(1);
        Self { window, reserve }
    }

    /// Room kept for estimate error.
    #[must_use]
    pub fn margin(&self) -> u64 {
        self.window / 20
    }

    /// `U`: the prompt tokens a request may use (never sent above this).
    #[must_use]
    pub fn usable(&self) -> u64 {
        self.window
            .saturating_sub(self.reserve)
            .saturating_sub(self.margin())
    }

    /// Same as [`Self::usable`].
    #[must_use]
    pub fn available(&self) -> u64 {
        self.usable()
    }

    fn share(&self, percent: u64) -> u64 {
        self.usable() * percent / 100
    }

    /// A single tool result above this is stored and shown in part.
    #[must_use]
    pub fn inline_cap(&self) -> u64 {
        self.share(8).min(4_000)
    }

    /// Budget for a trimmed page snapshot.
    #[must_use]
    pub fn snapshot_cap(&self) -> u64 {
        self.share(15).min(6_000)
    }

    /// Budget for what the bot knows (and past tasks), in the header.
    #[must_use]
    pub fn knowledge(&self) -> u64 {
        self.share(6).clamp(1_500, 6_000)
    }

    /// Old tool results are cleared above this.
    #[must_use]
    pub fn clear_at(&self) -> u64 {
        self.share(60)
    }

    /// Clearing goes down to this.
    #[must_use]
    pub fn clear_to(&self) -> u64 {
        self.share(40)
    }

    /// Clearing happens only if it frees this much (each clearing breaks
    /// the provider's cache from that point).
    #[must_use]
    pub fn clear_at_least(&self) -> u64 {
        self.share(10)
    }

    /// Still above this after clearing: summarize (compact).
    #[must_use]
    pub fn compact_at(&self) -> u64 {
        self.share(80)
    }

    /// A compaction aims for this.
    #[must_use]
    pub fn compact_target(&self) -> u64 {
        self.share(35)
    }

    /// Longest summary a compaction may write.
    #[must_use]
    pub fn compact_output(&self) -> u32 {
        u32::try_from(self.reserve.min(4_000)).unwrap_or(4_000)
    }
}

/// Smoothing for the per-model ratio.
const ALPHA: f64 = 0.3;
/// The ratio stays within these.
const MIN_RATIO: f64 = 0.5;
const MAX_RATIO: f64 = 2.0;
/// What a too-long request does to the ratio.
const OVERFLOW_BUMP: f64 = 1.2;
/// Requests smaller than this teach nothing (framing dominates).
const MIN_SAMPLE: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Learned {
    ewma: f64,
    last: f64,
    samples: u32,
}

/// Per-model ratios: what a provider counted over what was estimated.
#[derive(Debug, Default)]
pub struct Calibration(Mutex<HashMap<String, Learned>>);

impl Calibration {
    /// The ratios saved in `store`.
    #[must_use]
    pub fn load(store: &dyn LoopStore) -> Self {
        let rows = store.calibrations().unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read the token calibration");
            Vec::new()
        });
        let map = rows
            .into_iter()
            .map(|r| {
                let ratio = r.ratio.clamp(MIN_RATIO, MAX_RATIO);
                (
                    r.model,
                    Learned {
                        ewma: ratio,
                        last: ratio,
                        samples: r.samples,
                    },
                )
            })
            .collect();
        Self(Mutex::new(map))
    }

    /// Notes that `model` counted `actual` prompt tokens where `estimated`
    /// were estimated. Returns the row to save, if anything was learned.
    #[allow(clippy::cast_precision_loss)]
    pub fn observe(&self, model: &str, estimated: u64, actual: u64) -> Option<CalibrationRow> {
        if model.is_empty() || estimated < MIN_SAMPLE || actual == 0 {
            return None;
        }
        let r = (actual as f64 / estimated as f64).clamp(MIN_RATIO, MAX_RATIO);
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let learned = map.entry(model.to_owned()).or_insert(Learned {
            ewma: r,
            last: r,
            samples: 0,
        });
        learned.ewma = (learned.ewma * (1.0 - ALPHA) + r * ALPHA).clamp(MIN_RATIO, MAX_RATIO);
        learned.last = r;
        learned.samples = learned.samples.saturating_add(1);
        Some(CalibrationRow {
            model: model.to_owned(),
            ratio: learned.ewma,
            samples: learned.samples,
        })
    }

    /// A request to `model` was too long for it: count higher from now on.
    pub fn overflowed(&self, model: &str) -> Option<CalibrationRow> {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let learned = map.entry(model.to_owned()).or_insert(Learned {
            ewma: 1.0,
            last: 1.0,
            samples: 0,
        });
        learned.ewma = (learned.ewma * OVERFLOW_BUMP).min(MAX_RATIO);
        learned.last = (learned.last * OVERFLOW_BUMP).min(MAX_RATIO);
        Some(CalibrationRow {
            model: model.to_owned(),
            ratio: learned.ewma,
            samples: learned.samples,
        })
    }

    /// Provider tokens per estimated token for `model`: the larger of the
    /// smoothed and the last ratio, so errors fall on the safe side; 1 for
    /// a model not seen yet.
    #[must_use]
    pub fn ratio(&self, model: &str) -> f64 {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(model).map_or(1.0, |l| l.ewma.max(l.last))
    }

    /// The largest ratio among `models` (any of them may answer); 1 for
    /// none.
    #[must_use]
    pub fn ratio_of_any<'a>(&self, models: impl IntoIterator<Item = &'a String>) -> f64 {
        models
            .into_iter()
            .map(|m| self.ratio(m))
            .reduce(f64::max)
            .unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryLoopStore;

    #[test]
    fn estimates_count_bytes_over_three() {
        assert_eq!(est(""), 0);
        assert_eq!(est("abc"), 1);
        assert_eq!(est("abcd"), 2);
        // A CJK character is three bytes: one token.
        assert_eq!(est("日本語"), 3);
        let m = Message::user("x".repeat(300));
        assert_eq!(est_message(&m), 104);
    }

    #[test]
    fn budget_table() {
        // (W, reserve, U, clear at, compact at, inline cap, snapshot cap, knowledge)
        let rows = [
            (8_192, 2_048, 5_735, 3_441, 4_588, 458, 860, 1_500),
            (65_536, 8_192, 54_068, 32_440, 43_254, 4_000, 6_000, 3_244),
            (131_072, 8_192, 116_327, 69_796, 93_061, 4_000, 6_000, 6_000),
            (
                200_000, 8_192, 181_808, 109_084, 145_446, 4_000, 6_000, 6_000,
            ),
        ];
        for (w, reserve, u, clear, compact, inline, snap, know) in rows {
            let b = TokenBudget::for_model(w, None);
            assert_eq!(b.reserve, reserve, "{w}");
            assert_eq!(b.usable(), u, "{w}");
            assert_eq!(b.clear_at(), clear, "{w}");
            assert_eq!(b.compact_at(), compact, "{w}");
            assert_eq!(b.inline_cap(), inline, "{w}");
            assert_eq!(b.snapshot_cap(), snap, "{w}");
            assert_eq!(b.knowledge(), know, "{w}");
            assert_eq!(b.clear_to(), u * 40 / 100);
            assert_eq!(b.clear_at_least(), u * 10 / 100);
            assert_eq!(b.compact_target(), u * 35 / 100);
        }
        // The answer's room: the model's own cap within 2,048..W/4.
        assert_eq!(TokenBudget::for_model(64_000, Some(4_096)).reserve, 4_096);
        assert_eq!(TokenBudget::for_model(64_000, Some(100)).reserve, 2_048);
        assert_eq!(TokenBudget::for_model(64_000, Some(60_000)).reserve, 16_000);
        // A tiny window still leaves something.
        let tiny = TokenBudget::for_model(100, None);
        assert_eq!(tiny.reserve, 25);
        assert_eq!(tiny.usable(), 70);
        assert_eq!(tiny.compact_output(), 25);
    }

    #[test]
    fn calibration_converges_is_clamped_and_saved() {
        let store = MemoryLoopStore::new();
        let c = Calibration::load(&store);
        assert!((c.ratio("m") - 1.0).abs() < f64::EPSILON);
        // The model counts 0.8 of the estimate, again and again.
        let mut row = None;
        for _ in 0..30 {
            row = c.observe("m", 1_000, 800);
        }
        assert!((c.ratio("m") - 0.8).abs() < 0.01, "{}", c.ratio("m"));
        // One request counted more: the ratio follows at once (safe side).
        c.observe("m", 1_000, 1_000);
        assert!((c.ratio("m") - 1.0).abs() < 0.01);
        // Clamped both ways.
        c.observe("big", 1_000, 9_000);
        assert!((c.ratio("big") - 2.0).abs() < f64::EPSILON);
        c.observe("small", 1_000, 10);
        assert!((c.ratio("small") - 0.5).abs() < f64::EPSILON);
        // Tiny requests teach nothing.
        assert!(c.observe("n", 10, 1_000).is_none());
        // Saved and read back.
        store.save_calibration(&row.unwrap()).unwrap();
        let again = Calibration::load(&store);
        assert!((again.ratio("m") - 0.8).abs() < 0.01);
        // A too-long request bumps it by a fifth.
        again.overflowed("m");
        assert!(
            (again.ratio("m") - 0.96).abs() < 0.01,
            "{}",
            again.ratio("m")
        );
    }

    #[test]
    fn the_estimate_is_conservative() {
        // (text, tokens a real tokenizer counted). English and snapshot
        // lines come out at or above what providers count.
        let english = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let snapshot =
            "  uid=3_14 link \"Desk lamp, brass\" url=\"https://shop.example.com/p/14\"\n"
                .repeat(40);
        // English is about 4 chars a token; snapshots about 3.2.
        assert!(est(&english) >= english.len() as u64 / 4);
        assert!(est(&snapshot) * 32 >= snapshot.len() as u64 * 10);
        // CJK: one token per character at most.
        let cjk = "検索結果を表示します".repeat(20);
        assert!(est(&cjk) >= cjk.chars().count() as u64);
    }
}
