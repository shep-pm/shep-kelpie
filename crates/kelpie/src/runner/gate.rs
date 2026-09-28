//! The gate between the worker's pull request and the merge ruling
//!
//! Each step looks once at the pull request's head. A label or ready change
//! kelpie did not make parks the worker first, before anything else. A
//! branch without the latest `main` is rebased and pushed, and a conflict
//! parks the worker on a ruling. A pending run, or none yet, waits for the
//! next step. A red run is the worker's next turn, naming the checks that
//! failed. A green run starts a CodeRabbit round while one is owed, and
//! otherwise raises the merge ruling. A project without CI skips the checks.

use super::Runner;
use super::report::{Begin, StepReport};
use crate::ports::{Checks, PullRequestState, Timestamp};
use crate::state::{RulingKind, StateError};
use crate::work_item::{Phase, Turn, foreign_change};
use crate::worktree::{self, Base, Rebase};

// GitHub registers the checks a push or a ready pull request starts within
// seconds, one at a time. Kelpie trusts a rollup once two minutes have passed.
pub(crate) const CHECKS_SETTLE: u64 = 120;

impl Runner {
    pub(super) fn check_ci(&mut self) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("CI runs on a work item");
        let Some(number) = item.pull_request else {
            return Ok(Begin::Idle);
        };
        let Phase::Ci { head: seen, since } = item.phase.clone() else {
            return Ok(Begin::Idle);
        };
        let pr = match self.ports.forge.pull_request(&self.settings.forge, number) {
            Ok(pr) => pr,
            Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
        };
        match pr.state {
            PullRequestState::Open => {}
            // The maintainer merged it by hand, which is their own ruling.
            PullRequestState::Merged => {
                self.update(|item| item.phase = Phase::Done { merged: true })?;
                return self.finish(true);
            }
            PullRequestState::Closed => return self.raise(number, RulingKind::Closed),
        }
        if let Some((known, description)) = foreign_change(&item.known, &pr.labels, !pr.draft) {
            return self.raise(number, RulingKind::ForeignChange { description, known });
        }
        let now = self.ports.clock.now();
        let since = if seen.as_deref() == Some(pr.head.as_str()) {
            since
        } else {
            let head = Some(pr.head.clone());
            self.update(|item| item.phase = Phase::Ci { head, since: now })?;
            now
        };
        match self.base_of(&pr.head) {
            Ok(Base::Current) => {}
            Ok(Base::Lagging) => return Ok(Begin::Idle),
            Ok(Base::Behind) => return self.rebase(number, &pr.head),
            Err(reason) => return Ok(self.gate_failed(reason)),
        }
        if !self.settings.ci {
            return self.passed(number, pr.head);
        }
        // A check set registers a check at a time, so a verdict waits until
        // it has had time to register whole.
        if !settled(since, now) {
            return Ok(Begin::Idle);
        }
        match pr.checks {
            Checks::None | Checks::Pending => Ok(Begin::Idle),
            Checks::Passed => self.passed(number, pr.head),
            Checks::Failed(checks) => self.ci_failed(number, pr.head, checks),
        }
    }

    // Green CI goes to a CodeRabbit round while one is owed, then to the merge.
    fn passed(&mut self, number: u64, head: String) -> Result<Begin, StateError> {
        if self.coderabbit_due() {
            return self.start_round(head);
        }
        if let Some(begin) = self.shots_before_merge(number, &head)? {
            return Ok(begin);
        }
        self.raise(number, RulingKind::Merge { head })
    }

    // A worker that pushed nothing after its last red run would get the
    // same run again and again, so the second time the maintainer decides.
    fn ci_failed(
        &mut self,
        number: u64,
        head: String,
        checks: Vec<String>,
    ) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("CI runs on a work item");
        if item.red_head.as_deref() == Some(head.as_str()) {
            return self.raise(number, RulingKind::StillRed { head, checks });
        }
        let issue = item.issue;
        let prompt = red_prompt(number, &head, &checks);
        let red = head.clone();
        self.update(|item| {
            item.red_head = Some(red);
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Implement;
        })?;
        Ok(Begin::Report(StepReport::CiFailed {
            issue,
            pull_request: number,
            head,
            checks,
        }))
    }

    /// Where `head` stands against `origin`, or why git could not say
    pub(super) fn base_of(&self, head: &str) -> Result<Base, String> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a base is of a work item");
        worktree::base_of(&self.settings.repo, &item.branch, head)
            .map_err(|e| format!("cannot fetch main: {e}"))
    }

    // `main` moved since the branch was cut or last rebased: rebase and push,
    // and CI runs again on the new head before anyone is asked.
    fn rebase(&mut self, number: u64, head: &str) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a rebase is of a work item");
        let issue = item.issue;
        let outcome = worktree::rebase(&self.settings.repo, &item.worktree, &item.branch, head);
        match outcome {
            Ok(Rebase::Pushed(rebased)) => {
                let (seen, since) = (Some(rebased.clone()), self.ports.clock.now());
                self.update(|item| item.phase = Phase::Ci { head: seen, since })?;
                Ok(Begin::Report(StepReport::Rebased {
                    issue,
                    pull_request: number,
                    head: rebased,
                }))
            }
            Ok(Rebase::Refused(reason)) => self.raise(number, RulingKind::Rebase { reason }),
            Err(e) => Ok(self.gate_failed(format!("cannot rebase #{number}: {e}"))),
        }
    }

    pub(super) fn gate_failed(&self, reason: String) -> Begin {
        let issue = self.state.work_item.as_ref().map_or(0, |item| item.issue);
        Begin::Report(StepReport::GateFailed { issue, reason })
    }
}

pub(super) fn settled(since: Timestamp, now: Timestamp) -> bool {
    now.0.saturating_sub(since.0) >= CHECKS_SETTLE
}

/// The first seven characters of a commit hash, as git shows it
pub(super) fn short(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}

fn red_prompt(number: u64, head: &str, checks: &[String]) -> String {
    format!(
        "CI failed on your pull request #{number} at {}: {}. `gh pr checks {number}` \
         lists the checks, and `gh run view <run id> --log-failed` shows what failed. \
         Fix it on this branch, commit, and push with `git push origin HEAD`.",
        short(head),
        checks.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::{Cost, Session, Usage};
    use crate::runner::step;
    use crate::test::{Rig, Scripted, git};

    fn ruling_report(report: Option<StepReport>) -> (u64, String) {
        match report {
            Some(StepReport::Ruling { id, question, .. }) => (id, question),
            other => panic!("no ruling was raised: {other:?}"),
        }
    }

    #[test]
    fn a_pending_run_waits_and_a_green_one_raises_the_merge_ruling() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

        rig.forge.set_checks(&head, Checks::Passed);
        let (id, question) = ruling_report(rig.verdict(&runner));
        assert_eq!(id, 1);
        assert_eq!(
            question,
            format!(
                "Merge pull request #71 at {} into main? `shep trigger shep rule '1 yes'` \
                 merges it, and `shep trigger shep rule '1 no <note>'` sends the worker your note.",
                &head[..7]
            )
        );
        assert_eq!(rig.forge.comments(), [(71, question.clone())]);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            status["rulings"],
            json!([{
                "id": 1,
                "question": question,
                "pull_request": 71,
                "kind": { "kind": "merge", "head": head },
                "alerted": false,
            }])
        );
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        assert_eq!(step(&runner).unwrap(), None, "a parked worker waits");
    }

    #[test]
    fn a_red_run_is_the_workers_next_turn_naming_the_failed_checks() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        let failed = vec!["lint".to_owned(), "test".to_owned()];
        rig.forge.set_checks(&head, Checks::Failed(failed.clone()));
        assert_eq!(
            rig.verdict(&runner),
            Some(StepReport::CiFailed {
                issue: 7,
                pull_request: 71,
                head: head.clone(),
                checks: failed,
            })
        );

        rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
        step(&runner).unwrap();
        let [first, fix] = rig.claude.calls().try_into().unwrap();
        assert_eq!(fix.session, Session::Resume(first.session.id().clone()));
        let named = format!(
            "CI failed on your pull request #71 at {}: lint, test. ",
            &head[..7]
        );
        assert!(fix.prompt.starts_with(&named), "{}", fix.prompt);

        let fixed = rig.forge.head_of("kelpie/7").unwrap();
        assert_ne!(fixed, head);
        rig.forge.set_checks(&fixed, Checks::Passed);
        let (id, question) = ruling_report(rig.verdict(&runner));
        assert_eq!(id, 1);
        assert!(question.contains(&fixed[..7]), "{question}");
    }

    #[test]
    fn a_second_red_run_on_a_head_the_worker_left_alone_parks_it() {
        let (rig, runner, head) = Rig::with_pull_request("rotom");
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["lint".into()]));
        rig.verdict(&runner);
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();

        let (_, question) = ruling_report(rig.verdict(&runner));
        let parked = format!(
            "CI failed again on pull request #71 at {}, and the worker pushed no fix: lint.",
            &head[..7]
        );
        assert!(question.starts_with(&parked), "{question}");
        assert_eq!(rig.claude.calls().len(), 2, "the red run went out once");
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Alerted { .. })
        ));
        assert_eq!(step(&runner).unwrap(), None);
    }

    #[test]
    fn a_branch_behind_main_is_rebased_and_asked_about_only_after_ci_on_the_new_head() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        let landed = rig.land_on_origin("landed.txt");
        rig.forge.set_checks(&head, Checks::Passed);
        let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
            panic!("the branch was not rebased");
        };
        assert_eq!(rig.forge.head_of("kelpie/7").as_ref(), Some(&rebased));
        let worktree = rig.worktree_7();
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), rebased);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD~1"]), landed);
        assert_eq!(git(&worktree, &["show", "HEAD:work.txt"]), "work");

        assert_eq!(
            step(&runner).unwrap(),
            None,
            "CI on the rebased head is pending"
        );
        assert_eq!(rig.forge.comments(), []);
        rig.forge.set_checks(&rebased, Checks::Passed);
        ruling_report(rig.verdict(&runner));
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"],
            json!({ "kind": "merge", "head": rebased })
        );
    }

    // Seen live on the playground: the step after a rebase read the head
    // from before the push, and parked the worker on a false ruling.
    #[test]
    fn a_forge_still_showing_the_head_from_before_a_push_is_waited_out() {
        let (rig, runner, head) = Rig::with_pull_request("hazels-lab");
        rig.land_on_origin("landed.txt");
        let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
            panic!("the branch was not rebased");
        };
        rig.forge.set_lagging(71, Some(&head));
        rig.forge.set_checks(&head, Checks::Passed);
        rig.clock.advance(CHECKS_SETTLE);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(rebased.clone()));

        rig.forge.set_lagging(71, None);
        rig.forge.set_checks(&rebased, Checks::Passed);
        ruling_report(rig.verdict(&runner));
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"],
            json!({ "kind": "merge", "head": rebased })
        );
    }

    #[test]
    fn a_rebase_that_conflicts_parks_the_worker_and_leaves_the_branch_as_it_was() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.land_on_origin("work.txt");
        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with(
                "Kelpie cannot rebase pull request #71 onto main: \
                 it conflicts with main in work.txt. Once the branch is fixed, \
                 `shep trigger koji rule '1 yes'` has kelpie look again"
            ),
            "{question}"
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head.clone()));
        let worktree = rig.worktree_7();
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
        assert_eq!(rig.claude.calls().len(), 1);
    }

    #[test]
    fn a_yes_after_the_maintainer_fixes_a_conflict_looks_at_ci_again() {
        let (rig, runner, _) = Rig::with_pull_request("rotom");
        let landed = rig.land_on_origin("work.txt");
        let (id, _) = ruling_report(step(&runner).unwrap());

        let worktree = rig.worktree_7();
        git(&worktree, &["fetch", "--quiet", "origin", "main"]);
        git(&worktree, &["reset", "--quiet", "--hard", &landed]);
        std::fs::write(worktree.join("work.txt"), "both\n").unwrap();
        git(&worktree, &["commit", "--quiet", "-am", "resolve"]);
        git(&worktree, &["push", "--quiet", "--force", "origin", "HEAD"]);
        let fixed = rig.forge.head_of("kelpie/7").unwrap();

        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "CI on the fixed head is pending"
        );
        rig.forge.set_checks(&fixed, Checks::Passed);
        let (next, question) = ruling_report(rig.verdict(&runner));
        assert_eq!(next, id + 1);
        assert!(question.starts_with("Merge pull request #71"), "{question}");
    }

    #[test]
    fn a_worktree_with_uncommitted_changes_is_not_rebased() {
        let (rig, runner, head) = Rig::with_pull_request("zeus");
        std::fs::write(rig.worktree_7().join("work.txt"), "half done\n").unwrap();
        rig.land_on_origin("landed.txt");
        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.contains("onto main: its worktree has changes that are not committed."),
            "{question}"
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
    }

    #[test]
    fn a_worker_that_repoints_its_common_git_dir_gets_no_rebase_run_there() {
        let (rig, runner, head) = Rig::with_pull_request("golbat");
        let wanted = rig.home.path().join("wanted");
        std::fs::create_dir_all(wanted.join("hooks")).unwrap();
        let own = rig.repo().join(".git/worktrees/7");
        std::fs::write(own.join("commondir"), format!("{}\n", wanted.display())).unwrap();
        rig.land_on_origin("landed.txt");
        let Some(StepReport::GateFailed { reason, .. }) = step(&runner).unwrap() else {
            panic!("kelpie ran git in a folder the worker chose");
        };
        assert!(
            reason.ends_with("is not this work item's worktree"),
            "{reason}"
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
    }

    #[test]
    fn with_ci_on_a_head_without_checks_waits_however_long_it_takes() {
        let (rig, runner, head) = Rig::with_pull_request("chelone");
        rig.forge.set_checks(&head, Checks::None);
        for _ in 0..5 {
            rig.clock.advance(3600);
            assert_eq!(step(&runner).unwrap(), None);
        }
        assert_eq!(rig.forge.comments(), []);
    }

    #[test]
    fn a_verdict_waits_for_the_check_set_to_settle() {
        let (rig, runner, head) = Rig::with_pull_request("zeus");
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(CHECKS_SETTLE - 1);
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(1);
        ruling_report(step(&runner).unwrap());
    }

    #[test]
    fn with_ci_off_the_checks_are_never_read() {
        let rig = Rig::new("hazels-lab");
        rig.edit_settings(|s| s.replace("ci = true", "ci = false"));
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge
            .set_checks(&head, Checks::Failed(vec!["lint".into()]));

        let (id, question) = ruling_report(step(&runner).unwrap());
        assert!(question.starts_with("Merge pull request #71"), "{question}");
        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::MarkedReady { .. })
        ));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true
            })
        );
        assert_eq!(rig.forge.merges(), [(71, head)]);
    }

    #[test]
    fn a_ruling_whose_comment_fails_still_stands_in_status_and_the_log() {
        let (rig, runner, head) = Rig::with_pull_request("golbat");
        rig.forge.set_checks(&head, Checks::Passed);
        rig.forge.set_comments_down(true);
        let Some(StepReport::Ruling { comment_failed, .. }) = rig.verdict(&runner) else {
            panic!("no ruling was raised");
        };
        assert_eq!(
            comment_failed.as_deref(),
            Some("gh failed: comments are down")
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], 1);
    }

    #[test]
    fn a_pull_request_merged_by_hand_ends_the_work_item_with_no_merge_by_kelpie() {
        let (rig, runner, _) = Rig::with_pull_request("zeus");
        rig.forge.set_state(71, PullRequestState::Merged);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: true
            })
        );
        assert_eq!(rig.forge.merges(), []);
        assert!(!rig.worktree_7().exists());
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_closed_pull_request_parks_the_worker_and_a_yes_keeps_its_branch() {
        let (rig, runner, head) = Rig::with_pull_request("reactmap");
        rig.forge.set_state(71, PullRequestState::Closed);
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with("Pull request #71 was closed without merging."),
            "{question}"
        );
        rig.ask(&runner, "rule", Some(&format!("{id} yes")));
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Finished {
                issue: 7,
                pull_request: Some(71),
                merged: false
            })
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
        assert!(!rig.worktree_7().exists());
        assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");
    }

    #[test]
    fn a_label_added_outside_kelpie_parks_the_worker_naming_it() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        rig.forge.label_pull_request(71, "bug");
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 1);
        assert_eq!(
            question,
            "Pull request #71 changed outside kelpie: the `bug` label was added. \
             `shep trigger shep rule '1 yes'` accepts it and kelpie carries on, and \
             `shep trigger shep rule '1 no <note>'` sends the worker your note."
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["kind"],
            json!({
                "kind": "foreign-change",
                "description": "the `bug` label was added",
                "known": { "labels": ["bug"], "ready": false },
            })
        );

        // A yes accepts it, and kelpie carries on watching the same pull request.
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.set_checks(&head, Checks::Passed);
        let (next, merge_question) = ruling_report(rig.verdict(&runner));
        assert_eq!(next, 2);
        assert!(merge_question.starts_with("Merge pull request #71"));
        // Kelpie's own record now matches, so the accepted label is not asked again.
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn marking_a_pull_request_ready_outside_kelpie_parks_the_worker() {
        let (rig, runner, _) = Rig::with_pull_request("koji");
        rig.forge.ready_pull_request(71);
        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with(
                "Pull request #71 changed outside kelpie: it was marked ready for review."
            ),
            "{question}"
        );
    }

    #[test]
    fn a_no_on_a_foreign_change_sends_the_worker_a_note_and_asks_again_once_it_reaches_ci() {
        let (rig, runner, head) = Rig::with_pull_request("rotom");
        rig.forge.label_pull_request(71, "bug");
        ruling_report(step(&runner).unwrap());
        rig.ask(&runner, "rule", Some("1 no  remove that label yourself "));
        rig.claude.script([
            Scripted::Push("rename.txt", "renamed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the noted turn: pushes, enters round 1
        let [first, noted] = rig.claude.calls().try_into().unwrap();
        assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
        assert_eq!(
            noted.prompt,
            "The maintainer answered no on pull request #71, with this note:\n\n\
             remove that label yourself\n"
        );
        assert_ne!(rig.forge.head_of("kelpie/7"), Some(head));

        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        // The label is still there, and unaccepted, so the next look parks again.
        let (id, _) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 2);
    }

    #[test]
    fn a_label_removed_after_a_yes_accepted_it_parks_the_worker_again() {
        let (rig, runner, _) = Rig::with_pull_request("chelone");
        rig.forge.label_pull_request(71, "bug");
        ruling_report(step(&runner).unwrap());
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.forge.unlabel_pull_request(71, "bug");
        let (id, question) = ruling_report(step(&runner).unwrap());
        assert_eq!(id, 2);
        assert!(
            question.starts_with(
                "Pull request #71 changed outside kelpie: the `bug` label was removed."
            ),
            "{question}"
        );
    }

    #[test]
    fn a_normal_gate_run_with_nothing_changed_outside_kelpie_asks_nothing_about_it() {
        let (rig, runner, head) = Rig::with_pull_request("zeus");
        rig.forge.set_checks(&head, Checks::Passed);
        let (_, question) = ruling_report(rig.verdict(&runner));
        assert!(question.starts_with("Merge pull request #71"), "{question}");
    }

    #[test]
    fn a_work_item_saved_before_the_gate_existed_is_left_alone() {
        let (rig, runner, head) = Rig::with_pull_request("xilriws");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        let item = saved["work_item"].as_object_mut().unwrap();
        item.remove("phase");
        item.remove("red_head");
        std::fs::write(&state, saved.to_string()).unwrap();

        let runner = rig.open().unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(rig.forge.comments(), []);
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "implement" })
        );
    }
}
