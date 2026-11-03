//! What a reader of a pull request is told of a ruling kelpie raised for being stuck

use super::test_fails;
use crate::runner::gate::short;
use crate::state::Stuck;

// The fix turns a worker had on red runs. The cap is read live, so a
// lowered one can sit below the count.
pub(super) fn fix_turns_had(turns: u32) -> String {
    match turns {
        0 => "The worker has had no fix turn on red runs".to_owned(),
        1 => "The worker has had 1 fix turn on red runs".to_owned(),
        n => format!("The worker has had {n} fix turns on red runs"),
    }
}

pub(in crate::runner) fn stuck_comment(reason: &Stuck) -> String {
    match reason {
        Stuck::Rebase { why, .. } => {
            format!("This branch could not be rebased onto main: {why}.")
        }
        Stuck::StillRed {
            head,
            checks,
            fix_turns: None,
            ..
        } => format!(
            "CI failed again at {} and no fix was pushed: {}.",
            short(head),
            checks.join(", ")
        ),
        Stuck::StillRed {
            head,
            checks,
            fix_turns: Some(turns),
            ..
        } => format!(
            "CI failed at {}: {}. {}.",
            short(head),
            checks.join(", "),
            fix_turns_had(*turns)
        ),
        Stuck::CheckQueued {
            head,
            checks,
            run: None,
            ..
        } => format!(
            "A check from an app never finished at {}: {}.",
            short(head),
            checks.join(", ")
        ),
        Stuck::CheckQueued { head, checks, .. } => format!(
            "A check stayed queued at {} after its run was rerun: {}.",
            short(head),
            checks.join(", ")
        ),
        Stuck::NoChecks { head, .. } => {
            format!("No check has appeared at {}.", short(head))
        }
        // The forge's own words stay off a public pull request.
        Stuck::MergeRefused { head, .. } => {
            format!("Merging at {} was refused twice.", short(head))
        }
        Stuck::Closed => "This pull request was closed without merging.".to_owned(),
        Stuck::FixNotPushed { .. } => {
            "A fix for review findings ended without a push, so those findings still hold."
                .to_owned()
        }
        Stuck::TestStillFails { .. } => test_fails::PUBLIC_LINE.to_owned(),
        Stuck::Unpushed { .. } => {
            "Work in this pull request's worktree is not pushed, so its review waits.".to_owned()
        }
        // The fork's own words stay off the contributor's pull request.
        Stuck::PushRefused { .. } => {
            "The fork refused a push of the work on this pull request, so it waits.".to_owned()
        }
        Stuck::TurnTimeout { .. } => {
            "The work on this pull request ran too long and was stopped.".to_owned()
        }
        Stuck::AdoptFailed { .. } => "This pull request could not be adopted.".to_owned(),
        Stuck::TurnFailed { setup: true, .. } => {
            "Kelpie could not set up the work on this pull request on its own machine, so it \
             waits for the maintainer."
                .to_owned()
        }
        Stuck::TurnFailed { .. } => {
            "The work on this pull request hit an error and stopped.".to_owned()
        }
        Stuck::OverBudget { .. } => {
            "The work on this pull request is over its budget and waits for an answer.".to_owned()
        }
    }
}
