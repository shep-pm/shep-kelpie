//! The swap a `taken` record owes, finished
//!
//! The issue check holds a maintainer's `ready-for-agent` issue that fails it off
//! the board and owes it the triager, and takes back none that passes. What is
//! left here is the swap an older build saved at [`TriageStep::Taken`]: the record
//! is already there, so `ready-for-agent` comes off, `needs-triage` goes on, and
//! the poll's listing is changed to match, so the issue is untouched in the same
//! poll and its research starts ahead of the board's pick.

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::ticket;
use crate::board::{READY, TRIAGE};
use crate::ports::{ForgeError, OpenIssue, OpenPull};
use crate::state::{StateError, TriageStep, Triaged};

fn carries(labels: &[String], label: &str) -> bool {
    labels.iter().any(|l| l == label)
}

// The labels `issue` carries once the swap is done
fn swapped(labels: &[String]) -> Vec<String> {
    let mut left: Vec<String> = (labels.iter())
        .filter(|l| l.as_str() != READY)
        .cloned()
        .collect();
    if !carries(&left, TRIAGE) {
        left.push(TRIAGE.to_owned());
    }
    left
}

impl Runner {
    // Whether `issue`'s first pass is still to be made: it has no record, or a
    // record that only says kelpie took it back
    pub(super) fn first_pass_owed(&self, issue: u64) -> bool {
        // An edit the check refused after a pass that ended is owed a new
        // one, unless a limit held the issue: a hold is final.
        let edited = |t: &Triaged| {
            matches!(t.step, TriageStep::Done | TriageStep::Ready)
                && t.held.is_none()
                && self.check_edited(issue)
        };
        self.triage_record(issue)
            .is_none_or(|t| t.step == TriageStep::Taken || edited(t))
    }

    // Whether `issue` has the record that says it was taken back and a swap the
    // listing shows not done
    fn swap_owed(&self, issue: &OpenIssue) -> bool {
        self.taken_seen(issue.number).is_some()
            && (carries(&issue.labels, READY) || !carries(&issue.labels, TRIAGE))
    }

    // The labels a taken-back issue's pass begins with, which are the ones the
    // swap leaves, whatever a listing read before it has settled says
    pub(super) fn taken_seen(&self, issue: u64) -> Option<Vec<String>> {
        let held = self.triage_record(issue)?;
        (held.step == TriageStep::Taken).then(|| held.seen.clone())
    }

    // Finishes the swap of each issue in `open` a `taken` record owes, and
    // changes `open` to the labels it leaves. A failed swap keeps its record and
    // is tried at the next poll, undoing only the half the listing shows: a stop
    // between the two writes leaves the issue with neither label.
    pub(super) fn take_back_ready(
        &mut self,
        open: &mut [OpenIssue],
        pulls: Option<&Result<Vec<OpenPull>, ForgeError>>,
    ) -> Result<(), StateError> {
        let owed: Vec<usize> = (open.iter().enumerate())
            .filter(|(_, i)| {
                self.swap_owed(i)
                    && !self.triage_given_up(i.number)
                    && !self.has_work_item(i.number)
            })
            .map(|(at, _)| at)
            .collect();
        if owed.is_empty() {
            return Ok(());
        }
        // Where no App covers the repo no pass is started, so a label taken off
        // would not come back. Asking costs a token, so it is asked only of an
        // issue that owes a swap.
        if !self.app_covers() {
            return Ok(());
        }
        // A pull request that closes an issue is the issue's work, so its swap
        // is left undone. Where they cannot be listed, or the poll read none,
        // none is.
        let Some(Ok(pulls)) = pulls else {
            return Ok(());
        };
        for at in owed {
            let number = open[at].number;
            if pulls.iter().any(|pull| pull.closes.contains(&number)) {
                continue;
            }
            let labels = swapped(&open[at].labels);
            if self.swap_ready(number, &open[at].labels) {
                open[at].labels = labels;
            }
        }
        Ok(())
    }

    // Whether the board has, or had, a work item on `issue`
    pub(super) fn has_work_item(&self, issue: u64) -> bool {
        let Ok(id) = ticket(issue) else { return true };
        self.state.item(id).is_some()
            || self.state.finished.contains(&id)
            || self.state.dropped.contains(&id)
    }

    // Swaps `ready-for-agent` for `needs-triage` on `issue`, as `listed` says it
    // stands, the first coming off before the second goes on so a stop between
    // them leaves it unready and still untouched. Each half is done only where
    // the listing shows it undone. Whether it is done.
    fn swap_ready(&mut self, issue: u64, listed: &[String]) -> bool {
        let off = carries(listed, READY);
        let on = !carries(listed, TRIAGE);
        if on && let Err(why) = self.make_waiting_label() {
            self.triage_failed(issue, &format!("cannot make `{TRIAGE}`: {why}"), true);
            return false;
        }
        // Kelpie's take-back is not the maintainer's, so the label is not seen
        // when it comes off.
        if off && let Err(e) = self.check_ready_unseen(issue) {
            let why = format!("cannot save that `{READY}` is not seen: {e}");
            self.triage_failed(issue, &why, true);
            return false;
        }
        if off && let Err(e) = self.label_issue(issue, READY, false) {
            let why = format!("cannot take `{READY}` off: {e}");
            self.triage_failed(issue, &why, !forge_is_busy(&e));
            return false;
        }
        if on && let Err(e) = self.label_waiting(issue) {
            let why = format!("cannot label it `{TRIAGE}`: {e}");
            self.triage_failed(issue, &why, !forge_is_busy(&e));
            return false;
        }
        true
    }
}
