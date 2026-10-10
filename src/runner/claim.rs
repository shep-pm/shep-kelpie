//! The `in-progress` label on an issue a work item holds, and the repo's
//! `agent:` labels
//!
//! It goes on when a work item opens and comes off when one ends, so a
//! reader of GitHub can see a worker has the issue. The label is a courtesy:
//! a forge that refuses it leaves a note and never stops the work item.
//! Each listed implementer's `agent:` label is made where the repo lacks it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::Runner;

/// The label on an issue while a work item holds it
///
/// `shep kelpie add` makes it with the other labels. The control session
/// puts the same name on this repo's issues by hand.
pub const IN_PROGRESS: &str = "in-progress";

impl Runner {
    // Labels `issue` as held, or takes the label off, and notes a refusal
    pub(super) fn mark_held(&mut self, issue: u64, held: bool) {
        if let Err(e) = self.label_issue(issue, IN_PROGRESS, held) {
            let verb = if held { "add" } else { "remove" };
            self.notes.push(format!(
                "cannot {verb} `{IN_PROGRESS}` on issue #{issue}: {e}"
            ));
        }
    }

    // Puts the label right after a start: on for each work item held, off
    // for each finished issue, whose own removal a refusal may have missed
    pub(super) fn settle_labels(&mut self) {
        let held = self.state.open_issues();
        let finished = self.state.finished.clone();
        for issue in held {
            self.mark_held(issue, true);
        }
        for issue in finished {
            self.mark_held(issue, false);
        }
    }

    // Makes each listed implementer's `agent:` label the repo lacks. Only
    // adds: removing a label would strip it from every issue, closed ones too.
    pub(super) fn make_agent_labels(&mut self) {
        let labels = crate::issues::agent_labels(&self.agents);
        let wanted = labels.iter().map(crate::issues::AgentLabel::new_label);
        let made = self.make_missing_labels(wanted);
        if let Err(e) = made {
            self.notes.push(e);
        }
    }

    /// What the runner could not do and carried on without, oldest first
    ///
    /// Each note is handed out once.
    pub fn take_notes(&mut self) -> Vec<String> {
        self.notes.take()
    }
}

/// What the runner carried on without, oldest first, kept in memory until
/// `take_notes` hands it out, and shared with the forge's rate-limit hold
#[derive(Debug, Clone, Default)]
pub(super) struct Notes(Arc<Mutex<Vec<String>>>);

impl Notes {
    fn lock(&self) -> MutexGuard<'_, Vec<String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn push(&self, note: String) {
        self.lock().push(note);
    }

    pub(super) fn extend(&self, notes: impl IntoIterator<Item = String>) {
        self.lock().extend(notes);
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.lock())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Forge, Usage};
    use crate::runner::{CHECKS_SETTLE, step};
    use crate::test::{Rig, Scripted};

    fn running(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        (rig, runner)
    }

    fn held(rig: &Rig, issue: u64) -> bool {
        (rig.forge.issue_labels(issue).iter()).any(|l| l == IN_PROGRESS)
    }

    fn notes(runner: &Mutex<Runner>) -> Vec<String> {
        runner.lock().unwrap().take_notes()
    }

    #[test]
    fn an_issue_added_by_hand_gains_the_label_and_a_dropped_one_loses_it() {
        let (rig, runner) = running("koji");
        rig.ask(&runner, "add", Some("7"));
        assert!(held(&rig, 7));

        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        rig.ask(&runner, "drop", None);
        assert!(!held(&rig, 7));
        assert_eq!(notes(&runner), Vec::<String>::new());
    }

    #[test]
    fn an_issue_the_board_dispatches_gains_the_label() {
        let (rig, runner) = running("golbat");
        rig.forge.list_ready(6, false);
        step(&runner).unwrap();
        assert!(held(&rig, 6));
    }

    #[test]
    fn a_merged_work_items_issue_loses_the_label() {
        let (rig, runner, _) = Rig::parked("acme");
        assert!(held(&rig, 7));
        rig.ask(&runner, "rule", Some("1 yes"));
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        step(&runner).unwrap();
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
        assert!(!held(&rig, 7));
    }

    #[test]
    fn a_forge_that_refuses_the_label_is_noted_and_the_work_item_still_opens() {
        let (rig, runner) = running("chelone");
        rig.forge.set_labels_down(true);
        let item = &rig.ask(&runner, "add", Some("7"))["work_item"];
        assert_eq!(item["issue"], json!(7));
        assert!(!held(&rig, 7));
        assert_eq!(
            notes(&runner),
            ["cannot add `in-progress` on issue #7: gh failed: labels are down"]
        );
        assert_eq!(notes(&runner), Vec::<String>::new(), "a note is given once");

        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        rig.ask(&runner, "drop", None);
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
        assert_eq!(
            notes(&runner),
            ["cannot remove `in-progress` on issue #7: gh failed: labels are down"]
        );
    }

    #[test]
    fn a_restart_puts_the_label_back_on_an_issue_a_work_item_holds() {
        let (rig, runner) = running("xilriws");
        rig.ask(&runner, "add", Some("7"));
        let repo = rig.settings().git.remote.unwrap();
        rig.forge
            .set_issue_label(&repo, 7, IN_PROGRESS, false)
            .unwrap();
        drop(runner);

        let runner = rig.open().unwrap();
        assert!(held(&rig, 7));
        assert_eq!(notes(&runner), Vec::<String>::new());
    }

    #[test]
    fn a_restart_takes_the_label_off_an_issue_whose_removal_was_refused() {
        let (rig, runner) = running("rotom");
        rig.ask(&runner, "add", Some("7"));
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        rig.forge.set_labels_down(true);
        rig.ask(&runner, "drop", None);
        assert!(held(&rig, 7));
        drop(runner);

        rig.forge.set_labels_down(false);
        let runner = rig.open().unwrap();
        assert!(!held(&rig, 7));
        assert_eq!(notes(&runner), Vec::<String>::new());
    }
}
