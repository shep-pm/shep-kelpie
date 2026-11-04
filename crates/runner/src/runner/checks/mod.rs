//! A work item's gates, shown on its pull request as check runs
//!
//! GitHub lists an App's check runs beside CI. Kelpie writes one per gate and
//! a `kelpie` summary, and never reads them back: the state file is the
//! record, and what was posted is kept in memory only, so a restart posts
//! each check once more and GitHub shows the newest of a name.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::Runner;
use super::ruling::Answer;
use crate::ports::{
    CheckRunId, CheckState, Conclusion, ForgeError, GateCheck, REVIEW_CHECK as REVIEW,
    RULING_CHECK as RULING, SUMMARY_CHECK as SUMMARY,
};
use crate::state::Ruling;
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, WorkItem};

#[cfg(test)]
mod tests;

/// The longest title, in characters
const TITLE: usize = 120;

/// What was last sent for one check on one head
#[derive(Debug)]
struct Sent {
    // None until the forge numbered it
    id: Option<CheckRunId>,
    check: GateCheck,
}

/// The check runs this process posted, in memory only
#[derive(Debug, Default)]
pub(super) struct Book {
    // By work item and head, then by the check's name
    heads: BTreeMap<(TicketId, String), BTreeMap<String, Sent>>,
    // The head and pull request each work item's checks were last posted on
    last: BTreeMap<TicketId, (String, u64)>,
    // How the ruling each work item was parked on was answered, until the
    // check that waited on it is closed
    answers: BTreeMap<TicketId, Conclusion>,
    // Whether the installation refused the permission, so none is asked again
    off: bool,
}

/// What a work item shows on its pull request's head
struct Standing<'a> {
    phase: &'a Phase,
    // The ruling a `Phase::Ruling` waits on
    ruling: Option<&'a Ruling>,
    // The pull request's number
    pr: u64,
    // How the ruling that held the item was answered, if kelpie saw it
    answered: Option<Conclusion>,
}

/// The checks `at` shows
///
/// `posted` is where a check stands on this head, if kelpie posted it. A gate
/// the head never reached has none, and a gate the item has left is closed
/// only where it is still running.
fn gate_checks(at: &Standing<'_>, posted: impl Fn(&str) -> Option<CheckState>) -> Vec<GateCheck> {
    let pr = at.pr;
    let check = |name: &str, state: CheckState, title: &str, said: &str| GateCheck {
        name: name.to_owned(),
        state,
        title: title.to_owned(),
        summary: format!("{said}\n\nTo ask for another round, run shep kelpie rework {pr}."),
    };
    let passed = CheckState::Completed(Conclusion::Success);
    let failed = CheckState::Completed(Conclusion::Failure);
    let skipped = CheckState::Completed(Conclusion::Skipped);
    let running = CheckState::InProgress;
    let open = |name: &str| matches!(posted(name), Some(CheckState::InProgress));
    // Closes `name` as `state` when it still runs, and posts nothing else.
    let close = |name: &str, state, title: &str, said: &str| match open(name) {
        true => vec![check(name, state, title, said)],
        false => Vec::new(),
    };
    let review_over = || {
        close(
            REVIEW,
            passed,
            "Reviewed",
            "Kelpie's review of this head is over.",
        )
    };
    let ruling_over = || match at.answered {
        Some(Conclusion::Failure) => {
            let said = "The maintainer said no to the ruling on this head.";
            close(RULING, failed, "Answered no", said)
        }
        _ => close(
            RULING,
            passed,
            "Answered yes",
            "The ruling on this head was answered yes.",
        ),
    };
    let summary = |state, title: &str, said: &str| vec![check(SUMMARY, state, title, said)];
    match at.phase {
        Phase::Implement | Phase::Done { merged: false, .. } => {
            let sent_back = "The worker was sent back, so this head is no longer gated.";
            let ruling = match at.answered {
                Some(_) => ruling_over(),
                None => close(RULING, skipped, "Withdrawn", sent_back),
            };
            let (state, title) = match at.answered {
                Some(Conclusion::Failure) => (failed, "The maintainer said no"),
                _ => (skipped, "Sent back to the worker"),
            };
            [
                close(REVIEW, skipped, "Left", sent_back),
                ruling,
                close(SUMMARY, state, title, sent_back),
            ]
            .concat()
        }
        Phase::Review(review) => {
            let title = round_title(review);
            let said = "Kelpie is reviewing this head.";
            [
                vec![check(REVIEW, running, &title, said)],
                summary(running, "In review", said),
            ]
            .concat()
        }
        Phase::Ci { .. } => [
            review_over(),
            ruling_over(),
            summary(
                running,
                "Waiting for CI",
                "Kelpie waits for CI on this head.",
            ),
        ]
        .concat(),
        Phase::Ruling { .. } => {
            let question = at
                .ruling
                .map_or("Waiting on a ruling", |r| r.question.as_str());
            let said = "Kelpie waits for the maintainer's answer to a ruling on this head.";
            [
                review_over(),
                vec![check(
                    RULING,
                    running,
                    &clip(question.lines().next().unwrap_or("")),
                    said,
                )],
                summary(running, "Waiting on the maintainer", said),
            ]
            .concat()
        }
        Phase::Merge { .. } | Phase::Done { merged: true, .. } => [
            review_over(),
            ruling_over(),
            summary(
                passed,
                "Cleared to merge",
                "Every gate on this head passed.",
            ),
        ]
        .concat(),
    }
}

fn round_title(review: &Review) -> String {
    match &review.reviewer {
        Some(reviewer) => format!("Round {} with {reviewer}", review.round),
        None => format!("Round {}", review.round),
    }
}

fn clip(line: &str) -> String {
    line.chars().take(TITLE).collect()
}

impl Runner {
    /// The work item parked on ruling `id`, and whether `answer` is a no or a
    /// rework, read before the answer moves it
    pub(super) fn parked_answer(&self, id: TicketId, answer: &Answer) -> Option<(TicketId, bool)> {
        let parked = |item: &&WorkItem| item.phase == Phase::Ruling { id };
        let item = self.state.work_items.iter().find(parked)?;
        Some((
            item.issue,
            matches!(answer, Answer::No(_) | Answer::Rework(_)),
        ))
    }

    /// Notes that the ruling a work item was parked on was answered, with a
    /// no or a rework where the flag is set, so the checks that waited on it
    /// close as that answer
    pub(super) fn note_answer(&mut self, parked: Option<(TicketId, bool)>) {
        if let Some((issue, no)) = parked {
            let answer = if no {
                Conclusion::Failure
            } else {
                Conclusion::Success
            };
            self.checks.answers.insert(issue, answer);
        }
    }

    /// Posts the checks that changed since they were last posted, as the App
    ///
    /// A repo the App does not cover, or whose installation has not accepted
    /// Checks, gets none. Any other failure is tried again once the check
    /// changes.
    pub(super) fn tell_checks(&mut self) {
        if self.checks.off {
            return;
        }
        let mut wanted = Vec::new();
        let mut gone: Vec<TicketId> = self.checks.last.keys().copied().collect();
        for item in &self.state.work_items {
            gone.retain(|issue| *issue != item.issue);
            let Some(pr) = item.pull_request else {
                continue;
            };
            let head = match &item.phase {
                Phase::Merge { head, .. } => Some(head.clone()),
                _ => item.known.head.clone(),
            };
            let last = self
                .checks
                .last
                .get(&item.issue)
                .map(|(head, _)| head.clone());
            let Some(head) = head.or(last) else {
                continue;
            };
            let ruling = match item.phase {
                Phase::Ruling { id } => self.state.rulings.iter().find(|r| r.id == id),
                _ => None,
            };
            let at = Standing {
                phase: &item.phase,
                ruling,
                pr,
                answered: self.checks.answers.get(&item.issue).copied(),
            };
            wanted.extend(self.checks.changed(item.issue, &head, &at));
            // A head the item moved on from is not gated any more.
            let left = Standing {
                phase: &Phase::Implement,
                ..at
            };
            for old in self.checks.other_heads(item.issue, &head) {
                wanted.extend(self.checks.changed(item.issue, &old, &left));
            }
            self.checks.prune(item.issue, &item.phase);
        }
        // A work item that ended or was dropped leaves its running checks.
        for issue in gone {
            let Some((head, pr)) = self.checks.last.get(&issue).cloned() else {
                continue;
            };
            let merged = (self.state.history.iter().rev())
                .find(|done| done.issue == issue)
                .is_some_and(|done| done.merged);
            let phase = Phase::Done {
                merged,
                closed: false,
            };
            let at = Standing {
                phase: &phase,
                ruling: None,
                pr,
                answered: self.checks.answers.get(&issue).copied(),
            };
            wanted.extend(self.checks.changed(issue, &head, &at));
            self.checks.prune(issue, &phase);
        }
        if wanted.is_empty() || !self.app_covers() {
            return;
        }
        let voice = Arc::clone(&self.ports.voice);
        for (issue, pr, head, check) in wanted {
            let key = (issue, head.clone());
            let id = (self.checks.heads.get(&key))
                .and_then(|p| p.get(&check.name))
                .and_then(|s| s.id);
            let result = match id {
                Some(id) => voice
                    .update_check_run(&self.remote, id, &check)
                    .map(|()| Some(id)),
                None => voice.check_run(&self.remote, &head, &check).map(Some),
            };
            let id = match result {
                Ok(id) => id,
                Err(ForgeError::NoPermission(why)) => {
                    self.checks.off = true;
                    self.notes.push(format!(
                        "kelpie's App cannot show check runs on {}, so it posts none until \
                         the runner restarts: {why}",
                        self.remote.as_str()
                    ));
                    return;
                }
                // The forge is held or out of reach: the next pass asks again.
                Err(
                    e @ (ForgeError::Held(_) | ForgeError::Limited { .. } | ForgeError::AppBusy(_)),
                ) => {
                    self.notes
                        .push(format!("cannot show the check {}: {e}", check.name));
                    return;
                }
                Err(e) => {
                    self.notes
                        .push(format!("cannot show the check {}: {e}", check.name));
                    id
                }
            };
            self.checks.last.insert(issue, (head, pr));
            (self.checks.heads.entry(key).or_default())
                .insert(check.name.clone(), Sent { id, check });
        }
    }
}

impl Book {
    // The checks `at` wants on `head` that differ from what was last posted.
    fn changed(
        &self,
        issue: TicketId,
        head: &str,
        at: &Standing<'_>,
    ) -> Vec<(TicketId, u64, String, GateCheck)> {
        let posted = self.heads.get(&(issue, head.to_owned()));
        let state = |name: &str| posted.and_then(|p| p.get(name)).map(|s| s.check.state);
        let same = |check: &GateCheck| {
            posted.and_then(|p| p.get(&check.name)).map(|s| &s.check) == Some(check)
        };
        (gate_checks(at, state).into_iter())
            .filter(|check| !same(check))
            .map(|check| (issue, at.pr, head.to_owned(), check))
            .collect()
    }

    // The heads of `issue` that checks were posted on but `head`.
    fn other_heads(&self, issue: TicketId, head: &str) -> Vec<String> {
        (self.heads.keys())
            .filter(|(i, h)| *i == issue && h != head)
            .map(|(_, h)| h.clone())
            .collect()
    }

    // Drops an answer once no ruling check waits on it, or a new ruling does.
    fn prune(&mut self, issue: TicketId, phase: &Phase) {
        let waiting = (self.heads.iter()).any(|((i, _), p)| {
            *i == issue
                && matches!(p.get(RULING), Some(s) if s.check.state == CheckState::InProgress)
        });
        if !waiting || matches!(phase, Phase::Ruling { .. }) {
            self.answers.remove(&issue);
        }
    }
}
