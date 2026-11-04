//! Token estimates and budgets.
//!
//! The estimate is deliberately on the high side: a byte count over three
//! (English runs near four characters a token, page snapshots near three,
//! and a three-byte CJK character is about one token). Codex is told the
//! window's limits from these ([`TokenBudget`]), and big results are
//! stored and shown in part by them.

/// Bytes per estimated token.
const BYTES_PER_TOKEN: u64 = 3;

/// Estimated tokens of `text`.
#[must_use]
pub fn est(text: &str) -> u64 {
    (text.len() as u64).div_ceil(BYTES_PER_TOKEN)
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

    /// Codex summarizes its conversation above this (its own compaction).
    #[must_use]
    pub fn compact_at(&self) -> u64 {
        self.share(88)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_count_bytes_over_three() {
        assert_eq!(est(""), 0);
        assert_eq!(est("abc"), 1);
        assert_eq!(est("abcd"), 2);
        // A CJK character is three bytes: one token.
        assert_eq!(est("日本語"), 3);
    }

    #[test]
    fn budget_table() {
        // (W, reserve, U, compact at, inline cap, snapshot cap)
        let rows = [
            (8_192, 2_048, 5_735, 5_046, 458, 860),
            (65_536, 8_192, 54_068, 47_579, 4_000, 6_000),
            (131_072, 8_192, 116_327, 102_367, 4_000, 6_000),
            (200_000, 8_192, 181_808, 159_991, 4_000, 6_000),
        ];
        for (w, reserve, u, compact, inline, snap) in rows {
            let b = TokenBudget::for_model(w, None);
            assert_eq!(b.reserve, reserve, "{w}");
            assert_eq!(b.usable(), u, "{w}");
            assert_eq!(b.compact_at(), compact, "{w}");
            assert_eq!(b.inline_cap(), inline, "{w}");
            assert_eq!(b.snapshot_cap(), snap, "{w}");
        }
        // The answer's room: the model's own cap within 2,048..W/4.
        assert_eq!(TokenBudget::for_model(64_000, Some(4_096)).reserve, 4_096);
        assert_eq!(TokenBudget::for_model(64_000, Some(100)).reserve, 2_048);
        assert_eq!(TokenBudget::for_model(64_000, Some(60_000)).reserve, 16_000);
        // A tiny window still leaves something.
        let tiny = TokenBudget::for_model(100, None);
        assert_eq!(tiny.reserve, 25);
        assert_eq!(tiny.usable(), 70);
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
