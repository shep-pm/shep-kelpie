//! Rulings, and the merge a yes allows
//!
//! A ruling is saved before its comment is posted, so a comment that fails
//! loses nothing: the ruling stays in status and in the log. A yes merges
//! only the head it was asked about, while CI on it is still green and it
//! still has the latest `main`. Otherwise kelpie withdraws the yes, goes
//! back to CI, and asks again.

use std::fmt;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use crate::ports::{Checks, PullRequestState};
use crate::state::{Ruling, RulingKind, StateError};
use crate::work_item::{Phase, Turn, WorkItem};
use crate::worktree;

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
                (Answer::Yes, RulingKind::Merge { head }) => item.phase = Phase::Merge { head },
                (Answer::Yes, RulingKind::Rebase { .. } | RulingKind::StillRed { .. }) => {
                    item.phase = Phase::Ci {
                        head: None,
                        since: now,
                    };
                }
                (Answer::Yes, RulingKind::Closed) => item.phase = Phase::Done { merged: false },
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

    pub(super) fn merge(&mut self, head: String) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a merge is of a work item");
        let (issue, Some(number)) = (item.issue, item.pull_request) else {
            return Ok(Begin::Idle);
        };
        let forge = &self.ports.forge;
        let repo = &self.settings.forge;
        let pr = match forge.pull_request(repo, number) {
            Ok(pr) => pr,
            Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
        };
        match pr.state {
            PullRequestState::Open => {}
            PullRequestState::Merged => {
                self.update(|item| item.phase = Phase::Done { merged: true })?;
                return self.finish(true);
            }
            PullRequestState::Closed => return self.raise(number, RulingKind::Closed),
        }
        let stale = if pr.head != head {
            Some(format!("#{number} moved to {}", short(&pr.head)))
        } else if !matches!(pr.checks, Checks::Passed | Checks::None) {
            Some(format!("CI on #{number} is no longer green"))
        } else {
            match self.has_latest_base(&head) {
                Ok(true) => None,
                Ok(false) => Some("main moved since the question".to_owned()),
                Err(reason) => return Ok(self.gate_failed(reason)),
            }
        };
        if let Some(reason) = stale {
            return self.withdraw(issue, number, reason);
        }
        if pr.draft
            && let Err(e) = forge.mark_ready(repo, number)
        {
            return Ok(self.gate_failed(format!("cannot mark #{number} ready: {e}")));
        }
        if let Err(e) = forge.merge(repo, number, &head) {
            return Ok(self.gate_failed(format!("cannot merge #{number}: {e}")));
        }
        self.update(|item| item.phase = Phase::Done { merged: true })?;
        self.finish(true)
    }

    fn withdraw(&mut self, issue: u64, number: u64, reason: String) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.phase = Phase::Ci { head: None, since })?;
        Ok(Begin::Report(StepReport::YesWithdrawn {
            issue,
            pull_request: number,
            reason,
        }))
    }

    // Removes the worktree, branch and build folder, then the work item.
    pub(super) fn finish(&mut self, merged: bool) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a finish is of a work item");
        let removed = worktree::remove(
            &self.settings.repo,
            &item.worktree,
            &item.branch,
            &item.build,
            merged,
        );
        if let Err(e) = removed {
            return Ok(self.gate_failed(format!("cannot clean up: {e}")));
        }
        let report = StepReport::Finished {
            issue: item.issue,
            pull_request: item.pull_request,
            merged,
        };
        let mut next = self.state.clone();
        next.work_item = None;
        // Rulings about this work item's pull request go with it.
        next.rulings
            .retain(|r| r.pull_request.is_none() || r.pull_request != item.pull_request);
        self.save(next)?;
        Ok(Begin::Report(report))
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
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::Session;
    use crate::runner::step;
    use crate::test::{Rig, Scripted, git};

    // A project parked on merge ruling 1 about `head`
    fn parked(project: &str) -> (Rig, Mutex<Runner>, String) {
        let (rig, runner, head) = Rig::with_pull_request(project);
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        (rig, runner, head)
    }

    fn finished() -> Option<StepReport> {
        Some(StepReport::Finished {
            issue: 7,
            pull_request: Some(71),
            merged: true,
        })
    }

    #[test]
    fn nothing_merges_without_a_yes() {
        let (rig, runner, _) = parked("shep");
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
    fn a_yes_merges_the_ruled_head_and_removes_the_branch_worktree_and_build_folder() {
        let (rig, runner, head) = parked("koji");
        let status = rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(status["rulings"], json!([]));
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "merge", "head": head })
        );

        assert_eq!(step(&runner).unwrap(), finished());
        assert_eq!(rig.forge.readied(), [71]);
        assert_eq!(rig.forge.merges(), [(71, head)]);
        assert_eq!(rig.forge.head_of("kelpie/7"), None);
        assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");
        assert!(!rig.worktree_7().exists());
        assert!(!rig.home.path().join("kelpie/targets/koji/7").exists());
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            (&status["work_item"], &status["rulings"]),
            (&json!(null), &json!([]))
        );
    }

    #[test]
    fn a_no_sends_the_note_to_the_worker_and_a_new_ruling_follows_its_next_green_push() {
        let (rig, runner, _) = parked("rotom");
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
            step(&runner).unwrap(),
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
    fn a_yes_on_a_head_that_moved_since_the_question_is_withdrawn() {
        let (rig, runner, _) = parked("golbat");
        let worktree = rig.worktree_7();
        std::fs::write(worktree.join("late.txt"), "late\n").unwrap();
        git(&worktree, &["add", "late.txt"]);
        git(&worktree, &["commit", "--quiet", "-m", "late"]);
        git(&worktree, &["push", "--quiet", "origin", "HEAD"]);
        let moved = rig.forge.head_of("kelpie/7").unwrap();

        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::YesWithdrawn {
                issue: 7,
                pull_request: 71,
                reason: format!("#71 moved to {}", &moved[..7]),
            })
        );
        assert_eq!(rig.forge.merges(), []);
        rig.forge.set_checks(&moved, Checks::Passed);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ruling { id: 2, .. })
        ));
    }

    #[test]
    fn a_yes_after_ci_stopped_being_green_is_withdrawn() {
        let (rig, runner, head) = parked("chelone");
        rig.forge.set_checks(&head, Checks::Pending);
        rig.ask(&runner, "rule", Some("1 yes"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::YesWithdrawn { reason, .. }) if reason == "CI on #71 is no longer green"
        ));
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn a_yes_after_main_moved_is_withdrawn_and_the_branch_rebased() {
        let (rig, runner, _) = parked("rotom");
        rig.land_on_origin("landed.txt");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::YesWithdrawn { reason, .. }) if reason == "main moved since the question"
        ));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Rebased { .. })
        ));
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn a_pull_request_closed_after_the_yes_is_not_merged_and_parks_the_worker() {
        let (rig, runner, _) = parked("koji");
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.set_state(71, PullRequestState::Closed);
        let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
            panic!("no ruling was raised");
        };
        assert_eq!(id, 2);
        assert!(
            question.starts_with("Pull request #71 was closed"),
            "{question}"
        );
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn a_runner_restarted_after_a_yes_still_merges() {
        let (rig, runner, head) = parked("zeus");
        rig.ask(&runner, "rule", Some("1 yes"));
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(step(&runner).unwrap(), finished());
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    #[test]
    fn a_merge_the_forge_refuses_is_tried_again_later() {
        let (rig, runner, head) = parked("reactmap");
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.set_merges_down(true);
        let report = step(&runner).unwrap().expect("a report");
        assert_eq!(
            report,
            StepReport::GateFailed {
                issue: 7,
                reason: "cannot merge #71: gh failed: merges are down".into()
            }
        );
        assert!(report.waits());
        assert!(rig.worktree_7().exists());

        rig.forge.set_merges_down(false);
        assert_eq!(step(&runner).unwrap(), finished());
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    #[test]
    fn ruling_ids_are_never_given_twice() {
        let (rig, runner, _) = parked("xilriws");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), finished());
        drop(runner);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(72, "kelpie/7", &[7]);
        rig.claude.script([Scripted::Push("again.txt", "again\n")]);
        step(&runner).unwrap();
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
    }
}
