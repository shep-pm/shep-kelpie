//! The forge calls a runner's start makes, after its socket opened
//!
//! Reading the owner's pull request template, telling the log a CodeRabbit the
//! repo's visibility leaves out, putting each open work item's `in-progress`
//! label right, and making the labels the repo lacks: every one asks the forge.
//! A restart runs every project's runner at once, and a login answers 60 calls
//! a minute, so a start that made them before its socket opened answered no
//! trigger for minutes. [`Runner::opened`] opens and posts a status first, and
//! these come after it, each waiting its turn in the login's pace. A failed
//! call is a note and the runner goes on, and [`Self::stop_with`] ends them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Runner, coderabbit_left_out, left_in, owner_template};

impl Runner {
    /// Tells the runner that `stopping` ends its loop, so a start gives up the
    /// rest of its forge calls once it is set
    ///
    /// Each of those calls waits its turn in the login's pace, which a restart
    /// of every project can fill for minutes, and the runner's lock is held
    /// while it waits. A stop that reached no such runner left its work items'
    /// time unsaved, and the labels it did not make are made at the next start.
    pub fn stop_with(&mut self, stopping: Arc<AtomicBool>) {
        self.loop_stop = Some(stopping);
    }

    /// Makes the forge calls a start makes, which [`Self::opened`] left for here
    pub fn settle_on_forge(&mut self) {
        for call in [
            Runner::refresh_owner_template,
            Runner::note_coderabbit_left_out,
            Runner::settle_labels,
            Runner::make_agent_labels,
            Runner::make_priority_labels,
            Runner::make_kind_labels,
        ] {
            if stop_has_come(&self.loop_stop) {
                return;
            }
            call(self);
        }
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

    // The note telling a CodeRabbit the repo's visibility leaves out, when the project named one
    fn note_coderabbit_left_out(&mut self) {
        let named = left_in(
            self.settings.named_reviewers(&self.book, &self.home),
            &self.lineup,
        );
        self.notes
            .extend(coderabbit_left_out(&self.remote, &named, &self.ports));
    }
}

/// Whether the loop `loop_stop` belongs to has been asked to end
///
/// `None` is a runner with no loop, which gives up nothing.
pub(in crate::runner) fn stop_has_come(loop_stop: &Option<Arc<AtomicBool>>) -> bool {
    loop_stop
        .as_ref()
        .is_some_and(|stopping| stopping.load(Ordering::SeqCst))
}
