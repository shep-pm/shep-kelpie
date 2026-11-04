//! A review call run to its end, and what it spent

use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, CLEAN, Ending, Reviewer, ReviewerError, RoundEnd,
    RoundStage, Timestamp, read_review,
};
use crate::runner::report::{ReviewCall, ReviewResult, Reviewed, Spent};
use crate::work_item::{ReviewCallState, WorkItem};
use crate::worktree::Linked;

/// Runs `action` to its end on its own thread: a local round, or a fresh
/// session of a reviewer, which `ending` can end
///
/// A call stopped with the runner comes back as [`ReviewResult::Stopped`]
/// rather than an error, so `end_review` can tell it from a failed gate.
pub(in crate::runner) fn run_review_call(
    claude: &dyn Agents,
    reviewer: &dyn Reviewer,
    action: ReviewCall,
    ending: &Ending,
    watch: &(dyn Fn(RoundStage) + Sync),
) -> Reviewed {
    match action {
        ReviewCall::Local {
            local,
            repo,
            worktree,
            base,
            out,
            round,
            criteria,
        } => {
            let worktree = Linked {
                repo: &repo,
                worktree: &worktree,
            };
            match reviewer.round_watched(&local, worktree, &base, &out, round, &criteria, watch) {
                Err(ReviewerError::Stopped) => stopped(),
                result => Reviewed {
                    ended: Some(
                        result
                            .as_ref()
                            .map_or_else(ReviewerError::ended, |_| RoundEnd::Answered),
                    ),
                    result: ReviewResult::Findings(result.map_err(|e| e.to_string())),
                    spent: Some(Spent::Local),
                },
            }
        }
        ReviewCall::Session(call) => {
            let (reply, spent) = run_claude(claude, &call, ending);
            match reply {
                Err(AgentError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Findings(reply.map_err(|e| e.to_string()).and_then(
                        |reply| {
                            read_review(&reply.text).map_err(|text| {
                                format!(
                                    "the reviewer's reply is neither findings nor {CLEAN}: {text}"
                                )
                            })
                        },
                    )),
                    spent,
                    ended: None,
                },
            }
        }
    }
}

/// Adds what a review call spent to the work item's record, and marks no
/// call in flight
///
/// A local round's time runs from when the call was marked running.
pub(in crate::runner) fn record_spent(item: &mut WorkItem, spent: Option<Spent>, now: Timestamp) {
    match spent {
        Some(Spent::Claude {
            role,
            session,
            usage,
            session_cost,
            metered,
        }) => {
            let energy = metered.and_then(|m| m.cost);
            item.record_metered_call(role, now, session, usage, session_cost, energy);
        }
        Some(Spent::Local) => {
            item.qwen.rounds += 1;
            if let ReviewCallState::Running { since } = item.review_call {
                item.qwen.seconds += now.0.saturating_sub(since.0);
            }
        }
        Some(Spent::Unanswered(_)) | None => {}
    }
    item.call_ended();
}

fn stopped() -> Reviewed {
    Reviewed {
        result: ReviewResult::Stopped,
        spent: None,
        ended: None,
    }
}

// A reply that came back cost something even if what it said is unusable.
fn run_claude(
    claude: &dyn Agents,
    call: &AgentCall,
    ending: &Ending,
) -> (Result<AgentReply, AgentError>, Option<Spent>) {
    let reply = claude.run(call, ending);
    let spent = match &reply {
        Ok(reply) => Some(Spent::Claude {
            role: call.role,
            session: call.session.id().clone(),
            usage: reply.usage,
            session_cost: reply.session_cost,
            metered: reply.metered,
        }),
        // Its settings, harness or session never got as far as a model.
        Err(_) => crate::usage::ended(&reply).map(Spent::Unanswered),
    };
    (reply, spent)
}
