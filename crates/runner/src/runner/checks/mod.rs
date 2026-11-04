//! A work item's gates, shown on its pull request as check runs
//!
//! GitHub lists an App's check runs beside CI. Kelpie writes one per gate and
//! a `kelpie` summary, and never reads them back: the state file is the
//! record, and what was posted is kept in memory only, so a restart posts
//! each check once more and GitHub shows the newest of a name.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::Runner;
use crate::ports::{CheckRunId, CheckState, Conclusion, ForgeError, GateCheck};
use crate::state::Ruling;
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review};

#[cfg(test)]
mod tests;

const REVIEW: &str = "kelpie / review";
const RULING: &str = "kelpie / ruling";
const SUMMARY: &str = "kelpie";

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
    // The head each work item's checks were last posted on
    last: BTreeMap<TicketId, String>,
    // Whether the installation refused the permission, so none is asked again
    off: bool,
}

/// The checks `phase` shows on its pull request `pr`
///
/// `opened` says whether a check was already posted on this head: a gate the
/// head never reached has none, and one it passed is no longer in its phase.
/// `ruling` is the one a `Phase::Ruling` waits on.
fn gate_checks(
    phase: &Phase,
    ruling: Option<&Ruling>,
    pr: u64,
    opened: impl Fn(&str) -> bool,
) -> Vec<GateCheck> {
    let check = |name: &str, state: CheckState, title: &str, said: &str| GateCheck {
        name: name.to_owned(),
        state,
        title: title.to_owned(),
        summary: format!("{said}\n\nTo ask for another round, run shep kelpie rework {pr}."),
    };
    let passed = CheckState::Completed(Conclusion::Success);
    let failed = CheckState::Completed(Conclusion::Failure);
    let running = CheckState::InProgress;
    let (reviewed, ruled) = (opened(REVIEW), opened(RULING));
    let review_over = || {
        let said = "Kelpie's review of this head is over.";
        check(REVIEW, passed, "Reviewed", said).into_iter_if(reviewed)
    };
    let ruling_over = || {
        let said = "The ruling on this head was answered yes.";
        check(RULING, passed, "Answered yes", said).into_iter_if(ruled)
    };
    let summary = |state, title: &str, said: &str| vec![check(SUMMARY, state, title, said)];
    match phase {
        Phase::Implement if ruled => {
            let said = "The maintainer said no, so the worker is sent back to change it.";
            let no = check(RULING, failed, "Answered no", said);
            [vec![no], summary(failed, "The maintainer said no", said)].concat()
        }
        Phase::Implement | Phase::Done { merged: false, .. } => Vec::new(),
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
            let title = ruling.map_or("Waiting on a ruling", |r| {
                r.question.lines().next().unwrap_or("")
            });
            let said = "Kelpie waits for the maintainer's answer to a ruling on this head.";
            [
                review_over(),
                vec![check(RULING, running, &clip(title), said)],
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

trait IntoIterIf {
    fn into_iter_if(self, yes: bool) -> Vec<GateCheck>;
}

impl IntoIterIf for GateCheck {
    fn into_iter_if(self, yes: bool) -> Vec<GateCheck> {
        if yes { vec![self] } else { Vec::new() }
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
        for item in &self.state.work_items {
            let Some(pr) = item.pull_request else {
                continue;
            };
            let head = match &item.phase {
                Phase::Merge { head, .. } => Some(head.clone()),
                _ => item.known.head.clone(),
            };
            let Some(head) = head.or_else(|| self.checks.last.get(&item.issue).cloned()) else {
                continue;
            };
            let ruling = match item.phase {
                Phase::Ruling { id } => self.state.rulings.iter().find(|r| r.id == id),
                _ => None,
            };
            let posted = self.checks.heads.get(&(item.issue, head.clone()));
            let opened = |name: &str| posted.is_some_and(|p| p.contains_key(name));
            for check in gate_checks(&item.phase, ruling, pr, opened) {
                let same =
                    posted.and_then(|p| p.get(&check.name)).map(|s| &s.check) == Some(&check);
                if !same {
                    wanted.push((item.issue, head.clone(), check));
                }
            }
        }
        if wanted.is_empty() || !self.app_covers() {
            return;
        }
        let voice = Arc::clone(&self.ports.voice);
        for (issue, head, check) in wanted {
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
            self.checks.last.insert(issue, head);
            (self.checks.heads.entry(key).or_default())
                .insert(check.name.clone(), Sent { id, check });
        }
    }
}
