//! The page's messages for comparing engines and models (#21). The hub
//! decides who may (the Farm owner only); this only passes them on and
//! answers.

use oxbot_core::{Hub, HubError, Viewer};
use oxbot_protocol::{ClientMsg, ServerMsg};
use std::sync::Arc;

fn error(e: &HubError) -> Vec<ServerMsg> {
    vec![ServerMsg::Error {
        message: e.to_string(),
    }]
}

fn answer(result: Result<Vec<ServerMsg>, HubError>) -> Vec<ServerMsg> {
    result.unwrap_or_else(|e| error(&e))
}

/// Whether `msg` is one of the comparison messages, all the owner's.
#[must_use]
pub fn is_compare(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::RateTurn { .. }
            | ClientMsg::ListRatings { .. }
            | ClientMsg::DraftTestCase { .. }
            | ClientMsg::MarkTestCase { .. }
            | ClientMsg::UpdateTestCase { .. }
            | ClientMsg::DeleteTestCase { .. }
            | ClientMsg::ListTestCases { .. }
            | ClientMsg::EstimateBatch { .. }
            | ClientMsg::StartBatch { .. }
            | ClientMsg::StopBatch { .. }
            | ClientMsg::ListRuns { .. }
            | ClientMsg::OpenRun { .. }
            | ClientMsg::JudgeRuns { .. }
            | ClientMsg::SetRunVerdict { .. }
            | ClientMsg::Scoreboard { .. }
            | ClientMsg::SetShadow { .. }
            | ClientMsg::DeleteAllRuns
            | ClientMsg::ListComparePrefs
            | ClientMsg::SetComparePrefs(_)
    )
}

/// Handles a comparison message from `viewer`. Gives other messages back.
///
/// # Errors
/// The message, if it isn't a comparison message.
pub async fn handle(
    msg: ClientMsg,
    hub: &Arc<Hub>,
    viewer: &Viewer,
) -> Result<Vec<ServerMsg>, ClientMsg> {
    if !is_compare(&msg) {
        return Err(msg);
    }
    if !hub.is_owner_viewer(viewer) {
        return Ok(error(&HubError::NotAllowed(format!(
            "Only {} compares engines and models.",
            hub.owner_display_name()
        ))));
    }
    Ok(match msg {
        ClientMsg::RateTurn { bot, seq, up } => answer(
            hub.rate_turn(&bot, seq, up, viewer)
                .and_then(|()| hub.ratings_for(&bot, viewer).map(|m| vec![m])),
        ),
        ClientMsg::ListRatings { bot } => answer(hub.ratings_for(&bot, viewer).map(|m| vec![m])),
        ClientMsg::DraftTestCase { bot, seq } => answer(
            hub.draft_test_case(&bot, seq, viewer)
                .map(|d| vec![ServerMsg::CaseDraft(d)]),
        ),
        ClientMsg::MarkTestCase {
            bot,
            seq,
            title,
            rubric,
            network,
            uses_sign_ins,
        } => {
            let marked = hub
                .mark_test_case(&bot, seq, &title, &rubric, network, uses_sign_ins, viewer)
                .await;
            answer(marked.and_then(|_| {
                Ok(vec![
                    ServerMsg::TestCases {
                        cases: hub.test_cases_for(&bot, viewer)?,
                        bot: bot.clone(),
                    },
                    hub.ratings_for(&bot, viewer)?,
                ])
            }))
        }
        ClientMsg::UpdateTestCase {
            id,
            title,
            rubric,
            network,
            archived,
        } => answer(
            hub.update_test_case(
                &id,
                title.as_deref(),
                rubric.as_deref(),
                network,
                archived,
                viewer,
            )
            .map(|(bot, cases)| vec![ServerMsg::TestCases { bot, cases }]),
        ),
        ClientMsg::DeleteTestCase { id } => answer(
            hub.delete_test_case(&id, viewer)
                .map(|(bot, cases)| vec![ServerMsg::TestCases { bot, cases }]),
        ),
        ClientMsg::ListTestCases { bot } => answer(
            hub.test_cases_for(&bot, viewer)
                .map(|cases| vec![ServerMsg::TestCases { bot, cases }]),
        ),
        other => runs(other, hub, viewer).await,
    })
}

/// Batches, runs, verdicts, the scoreboard and shadows.
async fn runs(msg: ClientMsg, hub: &Arc<Hub>, viewer: &Viewer) -> Vec<ServerMsg> {
    match msg {
        ClientMsg::EstimateBatch {
            bot,
            cases,
            targets,
            repeats,
        } => answer(
            hub.estimate_batch(&bot, &cases, &targets, repeats, viewer)
                .map(|e| vec![ServerMsg::BatchEstimate(e)]),
        ),
        ClientMsg::StartBatch {
            bot,
            cases,
            targets,
            repeats,
        } => answer(
            hub.start_batch(&bot, &cases, &targets, repeats, viewer)
                .and_then(|_| {
                    Ok(vec![ServerMsg::Runs {
                        runs: hub.runs_for(&bot, None, viewer)?,
                        bot,
                    }])
                }),
        ),
        ClientMsg::StopBatch { id } => match hub.stop_batch(&id, viewer).await {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::ListRuns { bot, case } => answer(
            hub.runs_for(&bot, case.as_deref(), viewer)
                .map(|runs| vec![ServerMsg::Runs { bot, runs }]),
        ),
        ClientMsg::OpenRun { run } => answer(hub.run_detail(&run, viewer).map(|m| vec![m])),
        ClientMsg::JudgeRuns { runs, local } => match hub.judge_runs(&runs, local, viewer) {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::SetRunVerdict { run, verdict } => answer(
            hub.set_run_verdict(&run, verdict, viewer)
                .map(|r| vec![ServerMsg::TrialRun(Box::new(r))]),
        ),
        ClientMsg::Scoreboard { bot } => answer(
            hub.scoreboard_for(&bot, viewer)
                .map(|rows| vec![ServerMsg::Scoreboard { bot, rows }]),
        ),
        ClientMsg::SetShadow { bot, target } => {
            answer(hub.set_shadow(&bot, target, viewer).map(|m| vec![m]))
        }
        ClientMsg::ListComparePrefs => answer(
            hub.compare_prefs(viewer)
                .map(|p| vec![ServerMsg::ComparePrefs(p)]),
        ),
        ClientMsg::SetComparePrefs(prefs) => answer(
            hub.set_compare_prefs(prefs, viewer)
                .map(|p| vec![ServerMsg::ComparePrefs(p)]),
        ),
        ClientMsg::DeleteAllRuns => match hub.delete_all_runs(viewer) {
            Ok(n) => vec![ServerMsg::Notice {
                message: format!("Deleted {n} test runs."),
                owner: None,
                owner_id: None,
            }],
            Err(e) => error(&e),
        },
        _ => Vec::new(),
    }
}
