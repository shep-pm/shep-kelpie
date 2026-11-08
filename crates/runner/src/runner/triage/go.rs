//! The maintainer's go under `triage.pick = "label"`
//!
//! `needs-triage` on an outside issue asks for its triage. A pass that began
//! under the go keeps it in its record's `seen`. Once that pass has ended, the go
//! is spent and kelpie takes it off, so a label left on never asks twice. A go
//! that comes after, with no mark in `seen`, is a new ask: the maintainer puts it
//! on again after an edit, and `shep kelpie triage` asks without it.

use super::super::Runner;
use crate::board::TRIAGE_GO;
use crate::ports::OpenIssue;

impl Runner {
    // Whether the last listing shows the go on `issue`, while `pick` is `label`
    pub(super) fn go_listed(&self, issue: u64) -> bool {
        let Some(listing) = self.triage_run.listing.as_ref() else {
            return false;
        };
        (listing.open.iter()).any(|i| i.number == issue && self.go_asked(&i.labels))
    }

    // Whether the pass on `issue` began under the go: its record, or its pass
    // in flight, saw the label
    pub(super) fn go_seen(&self, issue: u64) -> bool {
        let tries = self
            .triage_tries
            .get(&issue)
            .and_then(|t| t.seen.as_deref());
        let held = self.triage_record(issue).map(|t| t.seen.as_slice());
        let has = |seen: &[String]| seen.iter().any(|l| l == TRIAGE_GO);
        self.settings.triage.pick == crate::settings::TriagePick::Label
            && tries.or(held).is_some_and(has)
    }

    // Whether `issue` carries a go that asks for a new pass: it has a record, and
    // that record's pass did not begin under a go
    pub(super) fn go_renewed(&self, labels: &[String], issue: u64) -> bool {
        self.go_asked(labels) && self.triage_record(issue).is_some() && !self.go_seen(issue)
    }

    // `go_renewed` for `issue` as the last listing shows it
    pub(super) fn go_renewed_listed(&self, issue: u64) -> bool {
        let listing = self.triage_run.listing.as_ref();
        (listing.into_iter().flat_map(|l| &l.open))
            .any(|i| i.number == issue && self.go_renewed(&i.labels, issue))
    }

    // Takes the go off each issue in `open` whose record's pass began under it
    // and is owed no new pass, and off the listing, which the poll goes on to
    // read. A write that fails is noted and tried at the next poll.
    pub(super) fn take_spent_gos(&mut self, open: &mut [OpenIssue]) {
        for issue in open.iter_mut() {
            let number = issue.number;
            let marked =
                (self.triage_record(number)).is_some_and(|t| t.seen.iter().any(|l| l == TRIAGE_GO));
            // A go gone from the issue while its record still holds it: a save
            // that failed after the label came off, or the maintainer's take-off
            if marked && !issue.labels.iter().any(|l| l == TRIAGE_GO) {
                if let Err(e) = self.forget_go(number) {
                    let why = format!("triage #{number}: cannot save the go as taken: {e}");
                    self.notes.push(why);
                }
                continue;
            }
            let spent = self.go_asked(&issue.labels) && marked && !self.first_pass_owed(number);
            if !spent {
                continue;
            }
            let taken = self.label_issue(number, TRIAGE_GO, false);
            if let Err(e) = taken {
                let why = format!("triage #{number}: cannot take `{TRIAGE_GO}` off: {e}");
                self.notes.push(why);
                continue;
            }
            issue.labels.retain(|l| l != TRIAGE_GO);
            if let Err(e) = self.forget_go(number) {
                self.notes.push(format!(
                    "triage #{number}: cannot save the go as taken: {e}"
                ));
            }
        }
    }

    // Takes the go out of `issue`'s record, so one put on again reads as new
    fn forget_go(&mut self, issue: u64) -> Result<(), crate::state::StateError> {
        let mut next = self.state.clone();
        let records = next.triage.iter_mut().flat_map(|t| t.issues.iter_mut());
        for record in records.filter(|t| t.issue == issue) {
            record.seen.retain(|l| l != TRIAGE_GO);
        }
        self.save(next)
    }
}
