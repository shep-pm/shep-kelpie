//! The triager's rewrite: the issue's kind label, and whether the issue check
//! passes the issue as the pass leaves it
//!
//! A pass puts one `kind:<type>` label on, made where the repo lacks it. It
//! puts none on an issue that already carries a kind label. The check decides
//! whether the maintainer's rewritten issue goes ready. It reads the listed
//! title and the new body. Its labels are the pass's start and its own.

use super::super::Runner;
use crate::board::{self, check};
use crate::ports::NewLabel;

/// The kind label `name`, as `kind:feat`, as kelpie makes it on a repo that lacks it
pub(in crate::runner) fn new_kind_label(name: &str) -> NewLabel<'_> {
    NewLabel {
        name,
        color: "c5def5",
        description: "The kind of work, as a conventional commit type",
    }
}

impl Runner {
    // The `kind:<type>` label for `work` on `issue`, made where `have` lacks it.
    // `None` where the issue carries a kind label, which stands, or where it
    // cannot be made, which the log says.
    pub(super) fn kind_label(
        &self,
        issue: u64,
        work: Option<&str>,
        have: &[String],
    ) -> Option<String> {
        let work = work?;
        let carried = self.triage_seen(issue);
        if carried.iter().any(|l| check::kind_of(l).is_some()) {
            return None;
        }
        let name = format!("{}{work}", check::KIND);
        if let Some(had) = board::repo_label(have, &name) {
            return Some(had.to_owned());
        }
        match self.make_missing_labels([new_kind_label(&name)]) {
            Ok(()) => Some(name),
            Err(why) => {
                self.notes.push(format!(
                    "triage #{issue}: {why}, so it goes on with no kind"
                ));
                None
            }
        }
    }

    // Whether the issue check passes `issue` with `body` written over it and
    // the pass's `labels` put on
    pub(super) fn passes_with(&self, issue: u64, body: &str, labels: &[String]) -> bool {
        let listed = (self.triage_run.listing.as_ref())
            .and_then(|l| l.open.iter().find(|i| i.number == issue))
            .map(|i| i.title.clone());
        let title = listed.or_else(|| {
            let read = super::ticket(issue).and_then(|t| self.ports.tickets.issue(&self.remote, t));
            read.ok().map(|found| found.title)
        });
        let mut after = self.triage_seen(issue);
        after.extend(labels.iter().cloned());
        check::check(&title.unwrap_or_default(), body, &after).is_empty()
    }
}
