//! The merge a yes or `auto` allows, and the cleanup after it
//!
//! A merge takes only the head it was asked about, while CI on it is still
//! green and it still has the latest `main`. Otherwise kelpie withdraws it,
//! goes back to CI, and asks or merges again. Under `auto` a refused merge
//! takes that path too, and a second refusal raises a ruling. Every step is
//! saved before the next, so a runner restarted mid-merge picks up where it
//! stopped.

use std::fmt;

use super::Runner;
use super::gate::{settled, short};
use super::report::{Begin, StepReport};
use super::trigger::WhichItem;
use crate::ports::{Checks, PullRequestState};
use crate::settings::MergeAuthority;
use crate::shots::publish;
use crate::state::{Notice, RulingKind, StateError};
use crate::work_item::{Phase, Review, ReviewCallState, Turn};
use crate::worktree::{self, Base};

/// Why `drop` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropError {
    /// The trigger named no work item, or one not open
    Which(WhichItem),
    /// The worker's turn is running, and the work item stays until it ends
    TurnRunning(u64),
    /// A review round or judge call is running, and the work item stays
    /// until it ends, since its result runs outside the runner's lock
    ReviewRunning(u64),
    /// A yes is being carried out, and the merge is not stopped halfway
    Merging(u64),
    /// Its worktree, branch or build folder could not be removed
    Cleanup(String),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for DropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Which(e) => e.fmt(f),
            Self::TurnRunning(issue) => write!(f, "the worker's turn on #{issue} is running"),
            Self::ReviewRunning(issue) => {
                write!(f, "the qwen-review loop's round on #{issue} is running")
            }
            Self::Merging(issue) => write!(f, "the work item for #{issue} is merging"),
            Self::Cleanup(reason) => f.write_str(reason),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for DropError {}

impl Runner {
    /// Ends a work item without merging it, the one `issue` names or the
    /// only one open
    ///
    /// Its worktree, local branch and build folder go, and its issue is
    /// recorded as finished. Its pull request and branch on the forge stay,
    /// the pull request labelled `ready-for-human`.
    /// It works whether the project runs or not.
    ///
    /// # Errors
    ///
    /// [`DropError`] when nothing is in flight, a turn, a review round or a
    /// merge is under way, or the cleanup fails. The work item stays then.
    pub fn drop_work_item(&mut self, issue: Option<u64>) -> Result<(), DropError> {
        self.choose(issue).map_err(DropError::Which)?;
        let item = self.current().expect("the work item chosen");
        if matches!(item.turn, Turn::Running { .. }) {
            return Err(DropError::TurnRunning(item.issue));
        }
        if matches!(item.review_call, ReviewCallState::Running { .. }) {
            return Err(DropError::ReviewRunning(item.issue));
        }
        if matches!(
            item.phase,
            Phase::Merge { .. } | Phase::Done { merged: true }
        ) {
            return Err(DropError::Merging(item.issue));
        }
        if matches!(item.phase, Phase::CodeRabbit(_)) {
            self.leave_round();
        }
        match self.finish(false).map_err(DropError::State)? {
            Begin::Report(StepReport::GateFailed { reason, .. }) => Err(DropError::Cleanup(reason)),
            _ => Ok(()),
        }
    }

    // Marking the draft ready can start a fresh CI run on the same head, so
    // that pass ends there. A later pass, once the checks have had time to
    // register, merges only on a green run.
    pub(super) fn merge(&mut self) -> Result<Begin, StateError> {
        let item = self.current().expect("a merge is of a work item");
        let (issue, Some(number)) = (item.issue, item.pull_request) else {
            return Ok(Begin::Idle);
        };
        let Phase::Merge {
            head,
            readied,
            auto,
        } = item.phase.clone()
        else {
            return Ok(Begin::Idle);
        };
        let tried = item.merge_tried.clone();
        let repo = self.settings.forge.clone();
        let pr = match self.ports.forge.pull_request(&repo, number) {
            Ok(pr) => pr,
            Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
        };
        match pr.state {
            PullRequestState::Open => {}
            // Kelpie's own merge at this head, when a restart or a lost
            // answer hid it, still gets its notice under `auto`.
            PullRequestState::Merged => {
                let notice = (auto && pr.head == head) || tried == Some(pr.head.clone());
                return self.merged(issue, number, pr.head, notice);
            }
            PullRequestState::Closed => return self.raise(number, RulingKind::Closed),
        }
        // The gate asks again once the project is no longer `auto`.
        if auto && self.settings.merge_authority != MergeAuthority::Auto {
            let reason = "the merge authority is no longer auto".to_owned();
            return self.withdraw(issue, number, auto, reason);
        }
        let now = self.ports.clock.now();
        let ci = self.settings.ci;
        let settling = ci && readied.is_some_and(|at| !settled(at, now));
        if pr.head != head {
            let reason = format!("#{number} moved to {}", short(&pr.head));
            return self.withdraw(issue, number, auto, reason);
        }
        // A run that marking the draft ready started is waited for; before
        // that, anything but green withdraws the yes.
        let green = match pr.checks {
            _ if !ci => true,
            Checks::Passed => true,
            Checks::None | Checks::Pending if readied.is_some() => false,
            Checks::None | Checks::Pending | Checks::Failed(_) => {
                let reason = format!("CI on #{number} is no longer green");
                return self.withdraw(issue, number, auto, reason);
            }
        };
        match self.base_of(&head) {
            Ok(Base::Current) => {}
            Ok(Base::Lagging) => return Ok(Begin::Idle),
            Ok(Base::Behind) => {
                let since = if auto {
                    "the gate passed"
                } else {
                    "the question"
                };
                let reason = format!("main moved since {since}");
                return self.withdraw(issue, number, auto, reason);
            }
            Err(reason) => return Ok(self.gate_failed(reason)),
        }
        if pr.draft {
            if let Err(e) = self.ports.forge.mark_ready(&repo, number) {
                return Ok(self.gate_failed(format!("cannot mark #{number} ready: {e}")));
            }
            let readied = Some(now);
            self.update(|item| {
                item.phase = Phase::Merge {
                    head,
                    readied,
                    auto,
                };
                item.known.ready = true;
            })?;
            return Ok(Begin::Report(StepReport::MarkedReady {
                issue,
                pull_request: number,
            }));
        }
        // The forge already shows it ready: either this pass just read that
        // back, or a restart landed between the call above and its update.
        // Either way it is kelpie's own doing, not a foreign change, and the
        // gate must not mistake it for one on its next look.
        if !item.known.ready {
            self.update(|item| item.known.ready = true)?;
        }
        if settling || !green {
            return Ok(Begin::Idle);
        }
        if let Err(e) = self.ports.forge.merge(&repo, number, &head) {
            let reason = format!("cannot merge #{number}: {e}");
            if !auto {
                return Ok(self.gate_failed(reason));
            }
            match self.ports.forge.pull_request(&repo, number) {
                Ok(pr) if pr.state == PullRequestState::Merged && pr.head == head => {}
                Ok(_) => return self.refused(issue, number, head, reason),
                Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
            }
        }
        self.merged(issue, number, head, auto)
    }

    // Nobody is asked before a merge under `auto`, so a head the gates never
    // saw is adopted into the worktree and goes back through every gate.
    pub(super) fn regate(&mut self, from: &str, tip: &str) -> Result<(), String> {
        let item = self.current().expect("a gate is of a work item");
        let (repo, branch) = (&self.settings.repo, &item.branch);
        worktree::adopt(repo, &item.worktree, branch, from, tip)
            .map_err(|e| format!("cannot bring the worktree to {}: {e}", short(tip)))?;
        let tip = Some(tip.to_owned());
        self.update(|item| {
            item.known.head = tip;
            item.coderabbit.satisfied = false;
            item.phase = Phase::Review(Review::first());
        })
        .map_err(|e| e.to_string())
    }

    // The notice is saved with the merge, so it goes out exactly once.
    pub(super) fn merged(
        &mut self,
        issue: u64,
        number: u64,
        head: String,
        notice: bool,
    ) -> Result<Begin, StateError> {
        let shots_failed = self.shots_failed(&head);
        let mut next = self.state.clone();
        let item = self
            .current_in(&mut next)
            .expect("a merge is of a work item");
        item.phase = Phase::Done { merged: true };
        if notice {
            next.notices.push(Notice {
                issue,
                pull_request: number,
                head,
                shots_failed,
            });
        }
        self.save(next)?;
        self.finish(true)
    }

    fn withdraw(
        &mut self,
        issue: u64,
        number: u64,
        auto: bool,
        reason: String,
    ) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.phase = Phase::Ci { head: None, since })?;
        Ok(Begin::Report(if auto {
            StepReport::MergeWithdrawn {
                issue,
                pull_request: number,
                reason,
            }
        } else {
            StepReport::YesWithdrawn {
                issue,
                pull_request: number,
                reason,
            }
        }))
    }

    // The first refusal under `auto` goes back to CI, which catches the
    // branch up with `main`. A second one, on any head, asks.
    fn refused(
        &mut self,
        issue: u64,
        number: u64,
        head: String,
        reason: String,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("a merge is of a work item");
        let refused_before = item.merge_refused;
        let tried = Some(head.clone());
        self.update(|item| {
            item.merge_refused = true;
            item.merge_tried = tried;
        })?;
        if refused_before {
            return self.raise(number, RulingKind::MergeRefused { head, reason });
        }
        self.withdraw(issue, number, true, reason)
    }

    // Removes the worktree, branch, build and shots folders, then the work
    // item, and records its issue so the board never takes it again. A pull
    // request left unmerged is handed back to the maintainer first.
    pub(super) fn finish(&mut self, merged: bool) -> Result<Begin, StateError> {
        // First, since the findings sit in the build folder this removes.
        if merged && let Some(begin) = self.follow_ups()? {
            return Ok(begin);
        }
        self.release()?;
        let item = self.current().expect("a finish is of a work item");
        // First, so a failure here leaves everything else for the retry.
        if let Some(number) = item.pull_request
            && let Err(e) = publish::delete(&self.settings.repo, &publish::branch(number))
        {
            return Ok(self.gate_failed(format!("cannot delete the shots branch: {e}")));
        }
        if let (false, Some(number)) = (merged, item.pull_request)
            && let Err(reason) = self.hand_back(number)
        {
            return Ok(self.gate_failed(reason));
        }
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
        for folder in [
            self.paths.shots(item.issue),
            self.paths.playwright(item.issue),
        ] {
            if let Err(e) = std::fs::remove_dir_all(&folder)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Ok(self.gate_failed(format!(
                    "cannot remove {}: {}",
                    folder.display(),
                    e.kind()
                )));
            }
        }
        let report = StepReport::Finished {
            issue: item.issue,
            pull_request: item.pull_request,
            merged,
            spend: item.spend(),
            qwen: item.qwen,
        };
        let mut next = self.state.clone();
        next.work_items.retain(|open| open.issue != item.issue);
        // Rulings about this work item go with it, a question asked before
        // its pull request included.
        next.rulings.retain(|r| r.issue != Some(item.issue));
        if !next.finished.contains(&item.issue) {
            next.finished.push(item.issue);
        }
        self.save(next)?;
        Ok(Begin::Report(report))
    }
}

#[cfg(test)]
mod auto;

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Usage};
    use crate::runner::gate::CHECKS_SETTLE;
    use crate::runner::step;
    use crate::test::{Rig, Scripted, git};

    fn finished(report: Option<StepReport>) -> bool {
        matches!(
            report,
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true,
                ..
            })
        )
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

        assert!(finished(ready_then_merge(&rig, &runner)));
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

    // A hook on the bare origin that refuses the worker branch's delete,
    // after the forge's own delete has taken the branch first when `forge_won`
    fn refuse_delete(rig: &Rig, forge_won: bool) {
        use std::os::unix::fs::PermissionsExt;
        let hook = rig.home.path().join("origin.git/hooks/pre-receive");
        let script = if forge_won {
            "#!/bin/sh\ngit update-ref -d refs/heads/kelpie/7\nexit 1\n"
        } else {
            "#!/bin/sh\nexit 1\n"
        };
        std::fs::write(&hook, script).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_branch_the_forge_deleted_first_still_counts_as_cleaned_up() {
        let (rig, runner, head) = Rig::parked("koji");
        rig.ask(&runner, "rule", Some("1 yes"));
        refuse_delete(&rig, true);

        assert!(finished(ready_then_merge(&rig, &runner)));
        assert_eq!(rig.forge.merges(), [(71, head)]);
        assert_eq!(rig.forge.head_of("kelpie/7"), None);
        assert!(!rig.worktree_7().exists());
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"], json!(null));
    }

    #[test]
    fn a_branch_that_cannot_be_deleted_for_another_reason_fails_the_cleanup() {
        let (rig, runner, _) = Rig::parked("koji");
        rig.ask(&runner, "rule", Some("1 yes"));
        refuse_delete(&rig, false);

        let Some(StepReport::GateFailed { reason, .. }) = ready_then_merge(&rig, &runner) else {
            panic!("the cleanup did not fail");
        };
        assert!(reason.starts_with("cannot clean up"), "{reason}");
        assert!(rig.forge.head_of("kelpie/7").is_some());
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["issue"], json!(7));
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
        assert!(finished(step(&runner).unwrap()));
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
        assert!(finished(step(&runner).unwrap()));
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
    fn marking_ready_is_kelpies_own_change_so_a_withdrawn_yes_is_not_parked_on_it() {
        let (rig, runner, head) = Rig::parked("koji");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(step(&runner).unwrap(), marked_ready());
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["test".into()]));
        rig.clock.advance(CHECKS_SETTLE);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::YesWithdrawn { .. })
        ));
        // The gate's next look must treat kelpie's own mark-ready as known,
        // not raise a foreign-change ruling that recurs forever.
        assert_eq!(
            rig.verdict(&runner),
            Some(StepReport::CiFailed {
                issue: 7,
                pull_request: 71,
                head: head.clone(),
                checks: vec!["test".into()],
            })
        );
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
        assert!(finished(step(&runner).unwrap()));
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
        assert!(finished(step(&runner).unwrap()));
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    // The work item the playground's board wrongly took: a turn that ended
    // with no pull request, in a project the maintainer paused.
    #[test]
    fn drop_clears_a_work_item_in_a_paused_project_and_the_board_never_retakes_it() {
        let rig = Rig::new("hazels-lab");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        rig.ask(&runner, "pause", None);
        let build = rig.home.path().join("kelpie/targets/hazels-lab/7");
        assert!(rig.worktree_7().exists() && build.exists());

        let status = rig.ask(&runner, "drop", None);
        assert_eq!(
            (&status["work_item"], &status["run"]),
            (&json!(null), &json!("paused"))
        );
        assert!(!rig.worktree_7().exists());
        assert!(!build.exists());
        assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");

        rig.forge.list_ready(7, false);
        rig.ask(&runner, "start", None);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn drop_refuses_with_nothing_in_flight_a_running_turn_or_a_merge() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "drop", None),
            json!({ "error": "no work item is in flight" })
        );
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Kill]);
        let _ = catch_unwind(AssertUnwindSafe(|| step(&runner)));
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "drop", None),
            json!({ "error": "the worker's turn on #7 is running" })
        );

        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(
            rig.ask(&runner, "drop", None),
            json!({ "error": "the work item for #7 is merging" })
        );
        assert!(rig.worktree_7().exists());
    }

    // A review round or judge call runs outside the runner's lock, the
    // same as a worker's turn. Dropping the work item out from under one
    // used to clear it while the call was still in flight, so the result
    // panicked the runner (`end_review`'s `.expect` on a work item that
    // was no longer there). Recording it in state the way a running turn
    // is, and refusing `drop` while it is set, keeps the call from ever
    // outliving its own work item. Unlike a turn, nothing ever reruns a
    // review call on restart, so the marker cannot be left running: `open`
    // clears it, rather than resuming it, before this drop can even ask.
    #[test]
    fn a_restart_clears_a_stale_review_call_so_drop_no_longer_refuses() {
        let (rig, runner, _) = Rig::with_pull_request("shep");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        saved["work_items"][0]["review_call"] = json!({ "state": "running", "since": Rig::EPOCH });
        std::fs::write(&state, saved.to_string()).unwrap();

        let runner = rig.open().unwrap();
        rig.ask(&runner, "pause", None);
        assert_eq!(
            rig.ask(&runner, "drop", None)["work_item"],
            json!(null),
            "the restart cleared the stale marker, so the drop goes through"
        );
        assert!(!rig.worktree_7().exists());
    }

    // Seen live on the playground: the board polled a second after the
    // merge, before GitHub closed the issue, and dispatched it again.
    #[test]
    fn an_issue_whose_work_item_finished_is_never_dispatched_again() {
        let (rig, runner, _) = Rig::parked("hazels-lab");
        rig.forge.list_ready(7, false);
        rig.forge.list_ready(8, true);
        rig.ask(&runner, "rule", Some("1 yes"));
        assert!(finished(ready_then_merge(&rig, &runner)));

        let calls = rig.claude.calls().len();
        assert_eq!(step(&runner).unwrap(), None);
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(step(&runner).unwrap(), None, "and not after a restart");
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"], json!(null));
        assert_eq!(rig.claude.calls().len(), calls);
    }
}
