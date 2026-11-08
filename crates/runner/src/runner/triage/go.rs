//! The maintainer's go under `triage.pick = "label"`
//!
//! `needs-triage` on an outside issue asks for its triage. Once the issue has a
//! record the go is spent, and kelpie takes it off, so a label left on never
//! asks twice. The maintainer asks again with the label put on once more after
//! an edit, or with `shep kelpie triage`.

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

    // Takes the go off each issue in `open` that has been triaged and is owed no
    // new pass, and off the listing, which the poll goes on to read. A write that
    // fails is noted and tried at the next poll.
    pub(super) fn take_spent_gos(&mut self, open: &mut [OpenIssue]) {
        for issue in open.iter_mut() {
            let spent = self.go_asked(&issue.labels)
                && self.triage_record(issue.number).is_some()
                && !self.first_pass_owed(issue.number);
            if !spent {
                continue;
            }
            let number = issue.number;
            match self.label_issue(number, TRIAGE_GO, false) {
                Ok(()) => issue.labels.retain(|l| l != TRIAGE_GO),
                Err(e) => self
                    .notes
                    .push(format!("triage #{number}: cannot take `{TRIAGE_GO}` off: {e}")),
            }
        }
    }
}
