//! A maintainer's hand-labelled `ready-for-agent` issue taken back for triage
//!
//! With `triage.hand_ready` on, the pick rule's listing finds each open issue the
//! maintainer opened after `triage.since` that carries `ready-for-agent` and no
//! other triage label but `needs-triage`, has no record, work item or open pull
//! request, and is above `pruned_through` (below it a prune may have dropped the
//! record of an issue kelpie made ready). The record is saved at
//! [`TriageStep::Taken`] first, so a restart finds the swap owed. Then
//! `ready-for-agent` comes off, `needs-triage` goes on, and the poll's listing is
//! changed to match, so its research starts ahead of the board's pick.

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::comment::NEEDS_TRIAGE;
use super::{TRIAGE_LABELS, is_app, ticket};
use crate::board::{READY, TRIAGE};
use crate::ports::{ForgeError, OpenIssue, OpenPull, Timestamp};
use crate::state::{StateError, TriageStep, Triaged};

// Whether `issue` is the maintainer's or the App's, carries no triage decision
// other than `ready-for-agent` and `needs-triage`, and is opened since `since`
// or carries `ready-for-agent`, which the board holds whatever its age
fn maintainers_undecided(
    issue: &OpenIssue,
    since: Timestamp,
    pruned_through: u64,
    maintainer: &str,
    app: Option<&str>,
) -> bool {
    let decided = (issue.labels.iter())
        .any(|l| l != READY && l != TRIAGE && TRIAGE_LABELS.contains(&l.as_str()));
    !decided
        && issue.number > pruned_through
        && (carries(&issue.labels, READY) || issue.created > since)
        && (issue.author.eq_ignore_ascii_case(maintainer) || is_app(&issue.author, app))
}

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

    // Whether `issue` is hand-labelled `ready-for-agent` with no record, or has
    // the record that says it was taken back and a swap not yet seen done
    fn swap_owed(&self, issue: &OpenIssue) -> bool {
        let taken = self.taken_seen(issue.number).is_some();
        (carries(&issue.labels, READY) || (taken && !carries(&issue.labels, TRIAGE)))
            && self.first_pass_owed(issue.number)
    }

    // The labels a taken-back issue's pass begins with, which are the ones the
    // swap leaves, whatever a listing read before it has settled says
    pub(super) fn taken_seen(&self, issue: u64) -> Option<Vec<String>> {
        let held = self.triage_record(issue)?;
        (held.step == TriageStep::Taken).then(|| held.seen.clone())
    }

    // Takes back each hand-labelled issue in `open` and changes `open` to the
    // labels the swap left. An issue whose swap fails keeps its record and is
    // tried at the next poll, doing only the half the listing shows undone: a
    // stop between the two writes leaves it with neither label.
    pub(super) fn take_back_ready(
        &mut self,
        open: &mut [OpenIssue],
        pulls: Option<&Result<Vec<OpenPull>, ForgeError>>,
        since: Timestamp,
        pruned_through: u64,
    ) -> Result<(), StateError> {
        // Where no App covers the repo no pass is started, so the label taken
        // off would not come back
        if !self.settings.triage.hand_ready || !self.app_covers() {
            return Ok(());
        }
        let Ok(maintainer) = self.maintainer() else {
            return Ok(());
        };
        let app = self.app_login();
        let candidates: Vec<usize> = (open.iter().enumerate())
            .filter(|(_, i)| {
                maintainers_undecided(i, since, pruned_through, &maintainer, app.as_deref())
                    && self.check_owed(i.number)
                    && self.swap_owed(i)
                    && !self.triage_given_up(i.number)
                    && !self.has_work_item(i.number)
            })
            .map(|(at, _)| at)
            .collect();
        if candidates.is_empty() {
            return Ok(());
        }
        // A pull request that closes an issue is the issue's work, so it is not
        // taken back. Where they cannot be listed, or the poll read none, none is.
        let Some(Ok(pulls)) = pulls else {
            return Ok(());
        };
        for at in candidates {
            let number = open[at].number;
            if pulls.iter().any(|pull| pull.closes.contains(&number)) {
                continue;
            }
            let labels = swapped(&open[at].labels);
            if self.triage_record(number).is_none() {
                self.save_taken(&open[at], &labels)?;
            }
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

    // Saves the record that says `issue` was taken back, with the labels its
    // pass will begin with
    fn save_taken(&mut self, issue: &OpenIssue, labels: &[String]) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(triage) = next.triage.as_mut() else {
            return Ok(());
        };
        triage.issues.retain(|t| t.issue != issue.number);
        triage.issues.push(Triaged {
            issue: issue.number,
            comment: 0,
            step: TriageStep::Taken,
            at: self.ports.clock.now(),
            author: issue.author.clone(),
            held: None,
            calls: Vec::new(),
            labels: Vec::new(),
            duplicate_of: None,
            ready_body: None,
            seen: labels.to_vec(),
            asked: None,
            questions: None,
            local: None,
            spam: None,
            begun: None,
        });
        self.save(next)?;
        self.notes.push(format!(
            "triage #{}: the maintainer labelled it `{READY}` by hand, so kelpie takes it back \
             for triage: `{TRIAGE}` in its place",
            issue.number
        ));
        Ok(())
    }

    // Swaps `ready-for-agent` for `needs-triage` on `issue`, as `listed` says it
    // stands, the first coming off before the second goes on so a stop between
    // them leaves the issue unready and still untouched. Each half is done only
    // where the listing shows it undone. Whether it is done.
    fn swap_ready(&mut self, issue: u64, listed: &[String]) -> bool {
        let off = carries(listed, READY);
        let on = !carries(listed, TRIAGE);
        if on && let Err(why) = self.make_missing_labels([NEEDS_TRIAGE]) {
            self.triage_failed(issue, &format!("cannot make `{TRIAGE}`: {why}"), true);
            return false;
        }
        if off && let Err(e) = self.label_issue(issue, READY, false) {
            let why = format!("cannot take `{READY}` off: {e}");
            self.triage_failed(issue, &why, !forge_is_busy(&e));
            return false;
        }
        if on && let Err(e) = self.label_issue(issue, TRIAGE, true) {
            let why = format!("cannot label it `{TRIAGE}`: {e}");
            self.triage_failed(issue, &why, !forge_is_busy(&e));
            return false;
        }
        true
    }
}
