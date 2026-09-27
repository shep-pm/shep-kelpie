//! Rulings: raising one, and the maintainer's answer
//!
//! A ruling is saved before its comment is posted, so a comment that fails
//! loses nothing: the ruling stays in status and in the log. Its question
//! carries the exact triggers that answer it. What a yes does depends on
//! the ruling; a no's note is always the worker's next turn.

use std::fmt;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use crate::state::{Ruling, RulingKind, StateError};
use crate::work_item::{Phase, Review, Turn, WorkItem};

/// The maintainer's answer to a ruling
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead: what that means depends on the ruling
    Yes,
    /// Do not, and send the worker this note
    No(String),
}

/// Why an answer was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// No pending ruling has this id
    NoSuchRuling(u64),
    /// The answer could not be saved
    State(StateError),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchRuling(id) => write!(f, "no ruling {id} is pending"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RuleError {}

impl Runner {
    /// Answers ruling `id`. A no's note is the worker's next turn.
    ///
    /// # Errors
    ///
    /// [`RuleError`] when no such ruling is pending or the answer cannot be
    /// saved. Nothing changes then.
    pub fn rule(&mut self, id: u64, answer: Answer) -> Result<(), RuleError> {
        let at = self.state.rulings.iter().position(|r| r.id == id);
        let at = at.ok_or(RuleError::NoSuchRuling(id))?;
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let ruling = next.rulings.remove(at);
        // Only the ruling the work item is parked on moves it. Any other,
        // which nothing leaves behind today, is answered by clearing it.
        let parked_on = |item: &WorkItem| item.phase == Phase::Ruling { id };
        if let Some(item) = next.work_item.as_mut().filter(|item| parked_on(item)) {
            match (answer, ruling.kind) {
                (Answer::Yes, RulingKind::Merge { head }) => {
                    item.phase = Phase::Merge {
                        head,
                        readied: None,
                    }
                }
                (Answer::Yes, RulingKind::Rebase { .. } | RulingKind::StillRed { .. }) => {
                    item.phase = Phase::Ci {
                        head: None,
                        since: now,
                    };
                }
                (Answer::Yes, RulingKind::Closed) => item.phase = Phase::Done { merged: false },
                (Answer::Yes, RulingKind::ReviewGuard { review }) => {
                    item.phase = Phase::Review(Review {
                        guard_cleared: true,
                        ..review
                    });
                }
                (Answer::No(note), _) => {
                    item.turn = Turn::Next {
                        prompt: note_prompt(item.pull_request, &note),
                    };
                    item.phase = Phase::Implement;
                }
            }
        }
        self.save(next).map_err(RuleError::State)
    }

    // Saves the ruling and parks the worker on it, then posts it on pull
    // request `number`.
    pub(super) fn raise(&mut self, number: u64, kind: RulingKind) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a ruling is about a work item");
        let issue = item.issue;
        let id = self.state.last_ruling + 1;
        let question = question(self.project.as_str(), id, number, &kind);
        let mut next = self.state.clone();
        next.last_ruling = id;
        next.rulings.push(Ruling {
            id,
            question: question.clone(),
            pull_request: Some(number),
            kind,
        });
        next.work_item.as_mut().expect("checked above").phase = Phase::Ruling { id };
        self.save(next)?;
        let posted = self
            .ports
            .forge
            .comment(&self.settings.forge, number, &question);
        Ok(Begin::Report(StepReport::Ruling {
            issue,
            pull_request: number,
            id,
            question,
            comment_failed: posted.err().map(|e| e.to_string()),
        }))
    }

    pub(super) fn update(&mut self, change: impl FnOnce(&mut WorkItem)) -> Result<(), StateError> {
        let mut next = self.state.clone();
        change(
            next.work_item
                .as_mut()
                .expect("a change to the work item in flight"),
        );
        self.save(next)
    }
}

fn question(project: &str, id: u64, number: u64, kind: &RulingKind) -> String {
    let trigger = |answer: &str| format!("`shep trigger {project} rule '{id} {answer}'`");
    let (yes, no) = (trigger("yes"), trigger("no <note>"));
    let ask = match kind {
        RulingKind::Merge { head } => {
            format!(
                "Merge pull request #{number} at {} into main? {yes} merges it",
                short(head)
            )
        }
        RulingKind::Rebase { reason } => format!(
            "Kelpie cannot rebase pull request #{number} onto main: {reason}. \
             Once the branch is fixed, {yes} has kelpie look again"
        ),
        RulingKind::StillRed { head, checks } => format!(
            "CI failed again on pull request #{number} at {}, and the worker pushed \
             no fix: {}. {yes} has kelpie look again",
            short(head),
            checks.join(", ")
        ),
        RulingKind::Closed => format!(
            "Pull request #{number} was closed without merging. {yes} drops the work \
             item and keeps its branch on the forge"
        ),
        RulingKind::ReviewGuard { review } => format!(
            "The qwen-review loop on pull request #{number} has run {} rounds without \
             settling. {yes} lets it keep going",
            review.round.saturating_sub(1)
        ),
    };
    format!("{ask}, and {no} sends the worker your note.")
}

fn note_prompt(number: Option<u64>, note: &str) -> String {
    let about = number.map_or_else(
        || "your work item".to_owned(),
        |n| format!("pull request #{n}"),
    );
    format!("The maintainer answered no on {about}, with this note:\n\n{note}\n")
}

#[cfg(test)]
mod tests {

    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Session};
    use crate::runner::gate::CHECKS_SETTLE;
    use crate::runner::step;
    use crate::test::{Rig, Scripted};

    #[test]
    fn nothing_merges_without_a_yes() {
        let (rig, runner, _) = Rig::parked("shep");
        for _ in 0..5 {
            rig.clock.advance(3600);
            assert_eq!(step(&runner).unwrap(), None);
        }
        let refused = [
            ("2 yes", "no ruling 2 is pending"),
            (
                "1 no",
                "`rule` takes `<id> yes` or `<id> no <note>`, not \"1 no\"",
            ),
            (
                "1 yes please",
                "`rule` takes `<id> yes` or `<id> no <note>`, not \"1 yes please\"",
            ),
            (
                "one yes",
                "`rule` takes `<id> yes` or `<id> no <note>`, not \"one yes\"",
            ),
            (
                "1 maybe",
                "`rule` takes `<id> yes` or `<id> no <note>`, not \"1 maybe\"",
            ),
        ];
        for (params, error) in refused {
            assert_eq!(
                rig.ask(&runner, "rule", Some(params)),
                json!({ "error": error }),
                "{params}"
            );
            assert_eq!(step(&runner).unwrap(), None, "{params}");
        }
        assert_eq!(rig.forge.merges(), []);
        assert!(rig.forge.readied().is_empty());
        assert!(rig.worktree_7().exists());
    }

    #[test]
    fn a_no_sends_the_note_to_the_worker_and_a_new_ruling_follows_its_next_green_push() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 no  rename the flag to --dry-run "));
        rig.claude
            .script([Scripted::Push("rename.txt", "renamed\n")]);
        step(&runner).unwrap();
        let [first, noted] = rig.claude.calls().try_into().unwrap();
        assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
        assert_eq!(
            noted.prompt,
            "The maintainer answered no on pull request #71, with this note:\n\n\
             rename the flag to --dry-run\n"
        );

        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "CI on the new head is pending"
        );
        rig.forge.set_checks(&pushed, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        let rulings = &rig.ask(&runner, "status", None)["rulings"];
        assert_eq!(
            rulings[0]["kind"],
            json!({ "kind": "merge", "head": pushed })
        );
        assert_eq!(rulings.as_array().unwrap().len(), 1);
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn ruling_ids_are_never_given_twice() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 yes"));
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished { merged: true, .. })
        ));
        drop(runner);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(72, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("again.txt", "again\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
    }
}
