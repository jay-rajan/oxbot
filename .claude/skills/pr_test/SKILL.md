---
name: pr_test
description: Run oxbot's end-to-end tests for a change (the current branch, uncommitted work, or a PR): pick the tests the change touches, run them against a seeded copy of the app with its own SQLite database and a stand-in Claude, screenshot every step, fix anything broken, and publish a report artifact. Use when the user types /pr_test, optionally with "all", "real", a PR number, or a base branch.
---

# /pr_test: does this change break anything?

The tests live in the repo: `e2e/tests/*.toml` (readable steps), `e2e/seed/` (bots and `seed.sql`), `e2e/claude/` (the stand-in Claude's replies), `e2e/baselines/` (approved screenshots), and `e2e/pr_test.toml` (how to build and start the app). The runner is `target/debug/oxbot-e2e` (crate `crates/oxbot-e2e`).

Arguments: none (the current branch plus uncommitted work against `main`), `all` (every test, every layer), a PR number (`gh pr diff` / `gh pr view`), `base=<branch>`, `real` (also try a couple of tests with the real Claude on Haiku; ask before spending).

Work in the oxbot repo (`~/src/oxbot` unless the current folder has `e2e/pr_test.toml`). Never commit or push. Never touch the user's real oxbot data (`~/Library/Application Support/oxbot`); every run uses throwaway folders.

## 1. Read the change and its intent
- Changed files: `oxbot-e2e pick --base <base>` prints them with the plan. For a PR: `gh pr view <n> --json title,body,commits` and `gh pr diff <n> --name-only`.
- Read the commit messages, the PR text, and the newest `PROMPTS.md` entries. Write the **intent** in one plain line (what the change means to do for the user).

## 2. Pick the tests
- `cargo build -p oxbot -p oxbot-e2e` (the commands in `pr_test.toml` → `build`).
- `target/debug/oxbot-e2e pick --base <base>` (add `--all` for `all`). It returns `run` (each with why), `skip`, `edited_tests`, and whether Safari and the app check are needed.
- Add any test whose subject the intent clearly touches even if its paths didn't match, and say why.

## 3. Changed tests: intended or not?
For each file in `edited_tests` (tests, seed, stand-in replies), compare the edit with the intent (`git diff <base> -- <file>`).
- **Intended** (the intent or PR text says the behavior changes, e.g. a renamed button): use the new test, and say so in the notes.
- **Not explained** (an expectation removed or loosened, a step dropped, with nothing in the intent about it): run the **old** test from the base (`git show <base>:<file> > <scratch>/<same name>.toml`) and flag the edit in the notes as unexplained. A change must not quietly weaken a test.

## 4. Run
Pick a run folder in the scratchpad (`<scratch>/pr_test-<time>`). Pass the reasons from the plan with `--why id=reason`.
- Chrome, always: `target/debug/oxbot-e2e run --out <run> --why … <tests…>`
- Safari, when the plan says so (or `all`): the same with `--engine safari`. If Safari can't be driven, the runner records why and moves on; tell the user the one-time switch (Safari → Settings → Advanced → "Show features for web developers", then Develop → "Allow Remote Automation").
- The Mac app check, when the plan says so (or `all`): build it first if it's older than the change (`app_build`), then `target/debug/oxbot-e2e app --out <run>`.

## 5. Broken: find why and fix it
For each broken test, read its failing step, the screenshot, and the oxbot log in `<run>/results.json`.
- Decide: is the **code** wrong (a bug the change brought in, or an old one), or did the change **mean** to alter this behavior (from the intent)?
- Code wrong → fix the code (never the test), with the repo's rules (`cargo fmt`, clippy pedantic, `cargo test --workspace`). Then rerun the broken tests, then the whole picked set, into the same run folder (results are replaced per test).
- Behavior meant to change → update the test to the new behavior and say so in the notes (and which line of the intent says so).
- A flaky step (timing) → make the test wait for what it needs (`wait_for`), never add sleeps to the app.
- Stop after three rounds and report what's still broken.

## 6. Changed screens
"Changed" means the step passed but the screen differs from its approved picture (the report shows approved / now / difference). Look at each:
- An expected result of the intent → list it as "looks different, as intended".
- Unexpected → treat it as broken: find out why.
- New tests have no approved pictures yet ("new").
Never approve screens yourself. Tell the user they can say "approve" and then run `target/debug/oxbot-e2e approve --out <run> [test ids]`.

## 7. Report artifact
Write `<run>/fixes.txt` (what you fixed: file, the bug, the fix; `- ` lines) and `<run>/notes.txt` (edited tests and your verdict, Safari or app skipped and why, unexpected screen changes, anything left), and the skipped tests one per line in `<run>/skipped.txt`. Then:

`target/debug/oxbot-e2e report --out <run> --intent "<one line>" --fixes <run>/fixes.txt --notes <run>/notes.txt --skipped <run>/skipped.txt`

Publish `<run>/index.html` with the Artifact tool (screenshots are inside the page). Use one artifact per repo: reuse the URL kept in `e2e/.report-url` if it exists (update it), else publish a new one and save its URL there. Icon: `check`.

## 8. Reply (short)
- The verdict first: all passed / N broken (and fixed?) / N screens changed.
- The intent line, and which tests ran and why (one line each or a count).
- What you fixed, in plain words.
- Edited tests and whether they were intended.
- The report link.
- What needs them: approving changed screens, Safari's switch, the app check's permissions.
