//! The merge a yes allows, and the cleanup after it
//!
//! A yes merges only the head it was asked about, while CI on it is still
//! green and it still has the latest `main`. Otherwise kelpie withdraws the
//! yes, goes back to CI, and asks again. Every step is saved before the next,
//! so a runner restarted mid-merge picks up where it stopped.

use super::Runner;
use super::gate::{settled, short};
use super::report::{Begin, StepReport};
use crate::ports::{Checks, PullRequestState};
use crate::state::{RulingKind, StateError};
use crate::work_item::Phase;
use crate::worktree;

impl Runner {
    // Marking the draft ready can start a fresh CI run on the same head, so
    // that pass ends there. A later pass, once the checks have had time to
    // register, merges only on a green run.
    pub(super) fn merge(&mut self) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a merge is of a work item");
        let (issue, Some(number)) = (item.issue, item.pull_request) else {
            return Ok(Begin::Idle);
        };
        let Phase::Merge { head, readied } = item.phase.clone() else {
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
        let now = self.ports.clock.now();
        let ci = self.settings.ci;
        let settling = ci && readied.is_some_and(|at| !settled(at, now));
        if pr.head != head {
            let reason = format!("#{number} moved to {}", short(&pr.head));
            return self.withdraw(issue, number, reason);
        }
        // A run that marking the draft ready started is waited for; before
        // that, anything but green withdraws the yes.
        let green = match pr.checks {
            _ if !ci => true,
            Checks::Passed => true,
            Checks::None | Checks::Pending if readied.is_some() => false,
            Checks::None | Checks::Pending | Checks::Failed(_) => {
                let reason = format!("CI on #{number} is no longer green");
                return self.withdraw(issue, number, reason);
            }
        };
        match self.has_latest_base(&head) {
            Ok(true) => {}
            Ok(false) => {
                let reason = "main moved since the question".to_owned();
                return self.withdraw(issue, number, reason);
            }
            Err(reason) => return Ok(self.gate_failed(reason)),
        }
        if pr.draft {
            if let Err(e) = self.ports.forge.mark_ready(repo, number) {
                return Ok(self.gate_failed(format!("cannot mark #{number} ready: {e}")));
            }
            let readied = Some(now);
            self.update(|item| item.phase = Phase::Merge { head, readied })?;
            return Ok(Begin::Report(StepReport::MarkedReady {
                issue,
                pull_request: number,
            }));
        }
        if settling || !green {
            return Ok(Begin::Idle);
        }
        if let Err(e) = self.ports.forge.merge(repo, number, &head) {
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
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::runner::gate::CHECKS_SETTLE;
    use crate::runner::step;
    use crate::test::{Rig, git};

    fn finished() -> Option<StepReport> {
        Some(StepReport::Finished {
            issue: 7,
            pull_request: Some(71),
            merged: true,
        })
    }

    fn marked_ready() -> Option<StepReport> {
        Some(StepReport::MarkedReady {
            issue: 7,
            pull_request: 71,
        })
    }

    // After a yes: the pass that marks the draft ready, then the pass that
    // merges once CI has had time to settle
    fn ready_then_merge(rig: &Rig, runner: &Mutex<Runner>) -> Option<StepReport> {
        assert_eq!(step(runner).unwrap(), marked_ready());
        rig.clock.advance(CHECKS_SETTLE);
        step(runner).unwrap()
    }

    #[test]
    fn a_yes_merges_the_ruled_head_and_removes_the_branch_worktree_and_build_folder() {
        let (rig, runner, head) = Rig::parked("koji");
        let status = rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(status["rulings"], json!([]));
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "merge", "head": head, "readied": null })
        );

        assert_eq!(ready_then_merge(&rig, &runner), finished());
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
    fn a_yes_on_a_head_that_moved_since_the_question_is_withdrawn() {
        let (rig, runner, _) = Rig::parked("golbat");
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
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
    }

    #[test]
    fn a_yes_after_ci_stopped_being_green_is_withdrawn() {
        let (rig, runner, head) = Rig::parked("chelone");
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
        let (rig, runner, _) = Rig::parked("rotom");
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
        let (rig, runner, _) = Rig::parked("koji");
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
    fn a_draft_marked_ready_merges_only_on_a_later_pass_once_ci_settles() {
        let (rig, runner, head) = Rig::parked("golbat");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), marked_ready());
        assert_eq!(rig.forge.merges(), []);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(CHECKS_SETTLE - 1);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.forge.merges(), []);
        rig.clock.advance(1);
        assert_eq!(step(&runner).unwrap(), finished());
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    #[test]
    fn a_run_that_marking_ready_starts_is_waited_for() {
        let (rig, runner, head) = Rig::parked("xilriws");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), marked_ready());
        rig.forge.set_checks(&head, Checks::Pending);
        rig.clock.advance(3 * CHECKS_SETTLE);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.forge.merges(), []);
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), finished());
    }

    #[test]
    fn a_red_run_after_marking_ready_withdraws_the_yes() {
        let (rig, runner, head) = Rig::parked("chelone");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), marked_ready());
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".into()]));
        rig.clock.advance(CHECKS_SETTLE);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::YesWithdrawn { reason, .. }) if reason == "CI on #71 is no longer green"
        ));
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn a_runner_restarted_between_ready_and_merge_still_waits_then_merges() {
        let (rig, runner, head) = Rig::parked("zeus");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), marked_ready());
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "the settling survived the restart"
        );
        rig.clock.advance(CHECKS_SETTLE);
        assert_eq!(step(&runner).unwrap(), finished());
        assert_eq!(rig.forge.merges(), [(71, head)]);
        assert_eq!(rig.forge.readied(), [71]);
    }

    #[test]
    fn a_merge_the_forge_refuses_is_tried_again_later() {
        let (rig, runner, head) = Rig::parked("reactmap");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), marked_ready());
        rig.clock.advance(CHECKS_SETTLE);
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
}
