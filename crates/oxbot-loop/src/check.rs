//! Notes oxbot adds to a Codex turn: checking a final answer before it's
//! accepted, and the nudge to wrap up near the step limit.
//!
//! When a turn that used tools answers in plain text, oxbot looks for the
//! values in the answer (numbers of 10 or more, decimals, money, percents,
//! temperatures) that nothing the bot read in this task had ([`numbers`]),
//! and sends the answer back once with a short checklist: the task word
//! for word, those values, and "check each part the task asked for".
//!
//! [`numbers`]: crate::numbers

use std::fmt::Write as _;

/// Tool calls a turn needs before its answer is checked.
pub const CHECK_AFTER_CALLS: usize = 2;
/// Most ungrounded values named.
pub const MAX_NAMED: usize = 8;
/// Most characters of the task quoted back.
const MAX_GOAL: usize = 600;

/// The note that sends an answer back: the task word for word, the values
/// nothing read had (if any), and the checklist. `browser`: name the page
/// tools; else the stored results and files.
#[must_use]
pub fn note(goal: &str, ungrounded: &[String], browser: bool) -> String {
    let goal = goal.trim();
    let goal = match goal.char_indices().nth(MAX_GOAL) {
        Some((cut, _)) => format!("{}…", &goal[..cut]),
        None => goal.to_owned(),
    };
    let mut note = format!("[oxbot] Before you answer, check it. The task was: «{goal}».");
    if !ungrounded.is_empty() {
        let tools = if browser {
            "find_in_page, read_section or query_rows"
        } else {
            "read_result or the file tools"
        };
        let _ = write!(
            note,
            " These values aren't in anything you read in this task: {}. Check them ({tools}) or \
             remove them.",
            ungrounded.join(", ")
        );
    }
    note.push_str(
        " Check each part the task asked for (every filter, the number of items, each field). \
         Then reply to the person with your complete answer, written for them: they never see \
         this note, so don't describe your checks.",
    );
    note
}

/// Shortest first answer worth keeping over a much shorter checked one.
const KEEP_FIRST_MIN_CHARS: usize = 300;

/// The answer to show after a check: the checked one, unless it shrank to
/// less than half of a substantial first answer. Small models sometimes
/// answer the check with a report about their checks ("The plan meets all
/// the requirements…") instead of the answer; then the first one is kept.
#[must_use]
pub fn answer_after_check(first: &str, checked: &str) -> String {
    let (first, checked) = (first.trim(), checked.trim());
    let first_len = first.chars().count();
    if first_len >= KEEP_FIRST_MIN_CHARS && checked.chars().count() * 2 < first_len {
        return first.to_owned();
    }
    if checked.is_empty() {
        return first.to_owned();
    }
    checked.to_owned()
}

/// Share of the step budget after which the model is told to wrap up.
const WRAP_UP_PERCENT: u32 = 70;

/// The wrap-up note once `step` reaches 70% of `max_steps` (once:
/// `nudged` says it was given).
pub fn wrap_up_note(step: u32, max_steps: u32, nudged: &mut bool) -> Option<String> {
    let at = (max_steps * WRAP_UP_PERCENT).div_ceil(100);
    if *nudged || step < at || step >= max_steps {
        return None;
    }
    *nudged = true;
    let left = max_steps - step;
    Some(format!(
        "[oxbot] You have {left} steps left: finish with what you have, and say what's missing."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_quotes_the_task_and_names_the_values() {
        assert_eq!(
            note(
                "[Jay] the 5 tallest finished towers",
                &["351.8".into(), "13".into()],
                true
            ),
            "[oxbot] Before you answer, check it. The task was: «[Jay] the 5 tallest finished \
             towers». These values aren't in anything you read in this task: 351.8, 13. Check \
             them (find_in_page, read_section or query_rows) or remove them. Check each part the \
             task asked for (every filter, the number of items, each field). Then reply to the \
             person with your complete answer, written for them: they never see this note, so \
             don't describe your checks."
        );
        assert_eq!(
            note(" [Sam] sum the file ", &[], false),
            "[oxbot] Before you answer, check it. The task was: «[Sam] sum the file». Check each \
             part the task asked for (every filter, the number of items, each field). Then reply \
             to the person with your complete answer, written for them: they never see this \
             note, so don't describe your checks."
        );
        assert!(
            note("[Sam] go", &["12".into()], false).contains("(read_result or the file tools)")
        );
        assert!(note("[Sam] go", &[], false).contains("they never see this note"));
        let long = "x".repeat(700);
        let n = note(&long, &[], true);
        assert!(n.contains(&format!("«{}…»", "x".repeat(600))), "{n}");
    }

    #[test]
    fn a_report_about_the_checks_never_replaces_the_answer() {
        let plan = "Monday: stir-fry. 1 lb chicken, 2 peppers. Steps: 1. Cook rice. ".repeat(10);
        let report = "The plan meets all the task requirements; recipes are in meal-plan.md.";
        assert_eq!(answer_after_check(&plan, report), plan.trim());
        // A checked answer of about the same size wins (it may fix values).
        let fixed = plan.replace("2 peppers", "3 peppers");
        assert_eq!(answer_after_check(&plan, &fixed), fixed.trim());
        // A short first answer: the checked one wins, even if shorter.
        assert_eq!(
            answer_after_check("It's 42 km.", "About 40 km."),
            "About 40 km."
        );
        // An empty checked answer keeps the first.
        assert_eq!(answer_after_check("Here it is.", "  "), "Here it is.");
    }
}
