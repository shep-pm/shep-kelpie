//! The `in-progress` label on an issue a work item holds, and the repo's
//! `agent:` labels
//!
//! It goes on when a work item opens and comes off when one ends, so a
//! reader of GitHub can see a worker has the issue. The label is a courtesy:
//! a forge that refuses it leaves a note and never stops the work item.
//! Each agent file of `role: implementer` gets its `agent:` label made where
//! the repo lacks it, listed by the project or not.
//! A project on the local store does neither.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::Runner;
use crate::board::IN_PROGRESS;
use crate::settings::TicketSource;
use crate::ticket::TicketId;

/// The key of the standing warning that issue `issue`'s worker runs with an
/// `origin/main` kelpie last fetched, which `status` lists until a fetch lands
/// or the work item ends
pub(super) fn main_fetch_key(ticket: TicketId) -> String {
    format!("main-fetch-{ticket}")
}

impl Runner {
    /// Whether the project takes its tickets from the local store, and so
    /// writes nothing to its repo but the branch and the pull request
    pub(super) fn local_store(&self) -> bool {
        self.settings.tickets.source == TicketSource::Local
    }

    // Labels `ticket`'s issue as held, or takes the label off, and notes a
    // refusal. Only a GitHub ticket has an issue to label, and a project on the
    // local store puts no label on its repo's issues. It still takes one off,
    // for an issue held before the project changed its source.
    pub(super) fn mark_held(&mut self, ticket: TicketId, held: bool) {
        let Some(issue) = ticket.github().filter(|_| !(held && self.local_store())) else {
            return;
        };
        if let Err(e) = self.label_issue(issue, IN_PROGRESS, held) {
            let verb = if held { "add" } else { "remove" };
            self.notes.push(format!(
                "cannot {verb} `{IN_PROGRESS}` on issue #{issue}: {e}"
            ));
        }
    }

    // Puts the label right after a start: on for each work item held, off
    // for each finished or dropped ticket with no work item open, whose own
    // removal a refusal may have missed
    pub(super) fn settle_labels(&mut self) {
        let held = self.state.open_tickets();
        let state = &self.state;
        let finished: Vec<TicketId> = (state.finished.iter().chain(&state.dropped))
            .filter(|n| !held.contains(n))
            .copied()
            .collect();
        for issue in held {
            self.mark_held(issue, true);
        }
        for issue in finished {
            self.mark_held(issue, false);
        }
    }

    // Makes each implementer agent file's `agent:` label the repo lacks. Only
    // adds: removing a label would strip it from every issue, closed ones too.
    // A local ticket carries its `agent:` label in the store, so a project on
    // the local store makes none on the repo.
    pub(super) fn make_agent_labels(&mut self) {
        if self.local_store() {
            return;
        }
        let labels = crate::board::agent_labels(&self.implementer_files());
        let wanted = labels.iter().map(crate::board::AgentLabel::new_label);
        let made = self.make_missing_labels(wanted);
        if let Err(e) = made {
            self.notes.push(e);
        }
    }

    // Makes each priority label `priority: P0` to `priority: P4` the repo has under
    // neither that name nor the bare `P0` to `P4`, so an issue can be ranked and the
    // maintainer has the label to use. Only adds, like the agent labels, and a
    // project on the local store makes none.
    pub(super) fn make_priority_labels(&mut self) {
        if self.local_store() {
            return;
        }
        let have = match self.ports.forge.repo_labels(&self.remote) {
            Ok(have) => have,
            Err(e) => {
                self.notes
                    .push(format!("cannot read the repo's labels for priorities: {e}"));
                return;
            }
        };
        let wanted = ["P0", "P1", "P2", "P3", "P4"]
            .into_iter()
            .filter(|p| crate::board::repo_priority(&have, p).is_none())
            .filter_map(super::triage::comment::priority_label);
        if let Err(e) = self.make_missing_labels(wanted) {
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

impl crate::ports::Warnings for Notes {
    fn stands(&self, key: &str, line: String) {
        self.standing(key, line);
    }

    fn ends(&self, key: &str) {
        Notes::ends(self, key);
    }
}

/// What the runner carried on without, oldest first, kept in memory until
/// `take_notes` hands it out, and shared with the forge's rate-limit hold
///
/// A warning that stands as long as its cause does is told once with
/// [`Notes::standing`], however many calls meet it, and `status` lists it
/// until [`Notes::ends`].
#[derive(Debug, Clone, Default)]
pub(super) struct Notes {
    lines: Arc<Mutex<Vec<String>>>,
    standing: Arc<Mutex<BTreeMap<String, String>>>,
}

impl Notes {
    fn lock(&self) -> MutexGuard<'_, Vec<String>> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn standing_lock(&self) -> MutexGuard<'_, BTreeMap<String, String>> {
        self.standing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn push(&self, note: String) {
        self.lock().push(note);
    }

    pub(super) fn extend(&self, notes: impl IntoIterator<Item = String>) {
        self.lock().extend(notes);
    }

    pub(super) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.lock())
    }

    /// Notes `line`, and keeps it standing under `key`, unless `key` stands already
    pub(super) fn standing(&self, key: &str, line: String) {
        let mut all = self.standing_lock();
        if all.contains_key(key) {
            return;
        }
        all.insert(key.to_owned(), line.clone());
        drop(all);
        self.push(line);
    }

    /// The warning under `key` no longer stands, so it is told again if it returns
    pub(super) fn ends(&self, key: &str) {
        self.standing_lock().remove(key);
    }

    /// The warnings that stand now, by their keys
    pub(super) fn standing_lines(&self) -> Vec<String> {
        self.standing_lock().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Tickets, Usage};
    use crate::runner::{CHECKS_SETTLE, step};
    use crate::test::{Rig, Scripted, ticket};

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
    fn a_standing_warning_is_noted_once_stands_in_the_list_and_is_noted_again_after_it_ends() {
        let notes = Notes::default();

        for _ in 0..3 {
            notes.standing("graph", "no code graph".into());
        }
        notes.standing("app", "no permission".into());
        assert_eq!(notes.take(), ["no code graph", "no permission"]);
        assert_eq!(notes.standing_lines(), ["no permission", "no code graph"]);

        notes.ends("graph");
        assert_eq!(notes.standing_lines(), ["no permission"]);
        notes.standing("graph", "no code graph".into());
        assert_eq!(notes.take(), ["no code graph"]);
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
    fn a_dropped_issue_added_again_keeps_the_label_through_a_start() {
        let (rig, runner) = running("koji");
        rig.ask(&runner, "add", Some("7"));
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        rig.ask(&runner, "drop", None);
        rig.ask(&runner, "add", Some("7"));
        assert!(held(&rig, 7));

        runner.lock().unwrap().settle_labels();
        assert!(held(&rig, 7));
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
        rig.ask(&runner, "rule", Some("71 yes"));
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
        assert_eq!(item["issue"], json!("7"));
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
            .set_issue_label(&repo, ticket(7), IN_PROGRESS, false)
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
