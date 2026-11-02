//! Closing an issue another open issue or pull request already covers
//!
//! The research can name the other item. Kelpie keeps the name only when the
//! forge agrees: the item is open, is the kind the reply gave, and is not the
//! issue itself. Anything else is dropped, logged, and the issue triaged as usual.
//! What is kept ends the pass: the representative words a short comment naming
//! the other item, the record's `closed` step is saved, and only then is the issue
//! closed as a duplicate. A restart goes on from that step and posts nothing again.
//! No label goes on a closed duplicate, and a reopened one is not triaged again
//! unless `shep kelpie triage` is asked.

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::research::Addressed;
use crate::ports::{ForgeError, IssueCloseReason};
use crate::state::{StateError, TriageStep};

/// What kelpie made of the item the research named as covering the issue
pub(super) enum Cover {
    /// It stands: open on the forge, and not the issue itself
    Yes,
    /// It does not, with the reason for the log
    No(String),
    /// The forge could not be asked, so nothing was decided and the pass is tried again
    Unknown,
}

impl Runner {
    // Checks `named`, which the research gave for `issue`, against the listing
    // its call was given and the forge as it stands now. A listing that cannot
    // be read fails the pass rather than close an issue against a cover kelpie
    // could not read.
    pub(super) fn cover_of(&mut self, issue: u64, named: &Addressed, shown: &[u64]) -> Cover {
        if named.number == issue {
            return Cover::No("it is the issue itself".to_owned());
        }
        if self.is_red_fix(issue) {
            return Cover::No(
                "the issue is the fix work item of a workflow red on main, which stays open \
                 whatever covers it"
                    .to_owned(),
            );
        }
        if !shown.contains(&named.number) {
            return Cover::No(
                "no issue or pull request of that number was in its listing".to_owned(),
            );
        }
        let open = match self.ports.tickets.open_issues(&self.remote) {
            Ok(open) => open,
            Err(e) => return self.cover_unknown(issue, &e),
        };
        let pulls = match self.ports.forge.open_pulls(&self.remote) {
            Ok(pulls) => pulls,
            Err(e) => return self.cover_unknown(issue, &e),
        };
        let open = match named.item {
            super::research::Covered::Issue => (open.iter()).any(|i| i.number == named.number),
            super::research::Covered::PullRequest => {
                (pulls.iter()).any(|p| p.number == named.number)
            }
        };
        match open {
            true => Cover::Yes,
            false => Cover::No(format!(
                "no open {} holds #{}",
                named.item.word(),
                named.number
            )),
        }
    }

    // Whether a red workflow's record names `issue` as its fix work item. The record wins
    // against any cover the research names, and the older issue is the one closed.
    fn is_red_fix(&self, issue: u64) -> bool {
        let fix = super::ticket(issue).ok();
        fix.is_some() && self.state.red.iter().any(|r| r.fix() == fix)
    }

    // Closes `issue` as a duplicate of `of`, from a record at the `closed`
    // step: what a restart leaves after the comment is already up
    pub(super) fn close_duplicate(&mut self, issue: u64, of: u64) -> Result<(), StateError> {
        if self.is_red_fix(issue) {
            self.notes.push(format!(
                "triage #{issue}: it became a red workflow's fix work item after it was to be \
                 closed as a duplicate of #{of}, so it stays open"
            ));
            self.triage_stepped(issue, TriageStep::Done)?;
            self.triage_tries.remove(&issue);
            return Ok(());
        }
        let closed = super::ticket(issue).and_then(|ticket| {
            self.ports
                .tickets
                .close_issue_as(&self.remote, ticket, IssueCloseReason::Duplicate)
        });
        match closed {
            Ok(()) => {
                self.triage_stepped(issue, TriageStep::Done)?;
                self.triage_tries.remove(&issue);
                Ok(())
            }
            Err(e) => {
                let why = format!("cannot close it as a duplicate of #{of}: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                Ok(())
            }
        }
    }

    fn cover_unknown(&mut self, issue: u64, e: &ForgeError) -> Cover {
        let why = format!("cannot read the open issues and pull requests: {e}");
        self.triage_failed(issue, &why, !forge_is_busy(e));
        Cover::Unknown
    }
}
