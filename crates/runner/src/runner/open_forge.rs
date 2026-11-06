//! The forge calls a runner's start makes, after its socket opened
//!
//! Reading the owner's pull request template, telling the log a CodeRabbit the
//! repo's visibility leaves out, putting each open work item's `in-progress`
//! label right, and making the labels the repo lacks: every one asks the forge.
//! A restart runs every project's runner at once, and a login answers 60 calls
//! a minute, so a start that made them before its socket opened answered no
//! trigger for minutes. [`Runner::opened`] opens and posts a status first, and
//! these come after it, before the loop's first pass. A call that fails is a
//! note and the runner goes on.

use super::{Runner, coderabbit_left_out, left_in, owner_template};

impl Runner {
    /// Makes the forge calls a start makes, which [`Self::opened`] left for here
    pub fn settle_on_forge(&mut self) {
        self.refresh_owner_template();
        let named = left_in(
            self.settings.named_reviewers(&self.book, &self.home),
            &self.lineup,
        );
        self.notes
            .extend(coderabbit_left_out(&self.remote, &named, &self.ports));
        self.settle_labels();
        self.make_agent_labels();
        self.make_priority_labels();
        self.make_kind_labels();
    }

    // The owner's `.github` template as of now, keeping the last read when this one fails
    pub(super) fn refresh_owner_template(&mut self) {
        let (template, note) = owner_template::refreshed(
            self.ports.forge.as_ref(),
            self.remote.owner(),
            self.owner_template.take(),
        );
        self.owner_template = template;
        self.notes.extend(note);
    }
}
