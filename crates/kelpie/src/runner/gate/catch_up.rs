//! Keeping a pull request's branch current with `main` before its merge
//!
//! A branch behind `main` is rebased and pushed, or merged if it is someone
//! else's. A conflict is the worker's next turn, a few times at most, and
//! then the maintainer's ruling.

use crate::runner::Runner;
use crate::runner::report::{Begin, StepReport};
use crate::state::{RulingKind, StateError};
use crate::work_item::{Conflict, Phase, Turn};
use crate::worktree::{self, Rebase};

// The most conflict turns one work item gets before the maintainer decides,
// so a `main` that moves before every check cannot loop the worker.
const CONFLICT_TURNS: u32 = 3;

impl Runner {
    // `main` moved since the branch was cut or last rebased: rebase and push,
    // and CI runs again on the new head before anyone is asked.
    pub(super) fn rebase(&mut self, number: u64, head: &str) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a rebase is of a work item");
        let issue = item.issue;
        // An adopted branch's commits are someone else's, so it is merged, never rewritten.
        let rewrite = !item.adopted;
        let outcome = worktree::rebase(
            &self.settings.repo,
            &item.worktree,
            &item.branch,
            head,
            rewrite,
        );
        match outcome {
            Ok(Rebase::Pushed(rebased)) => {
                let (seen, since) = (Some(rebased.clone()), self.ports.clock.now());
                self.update(|item| {
                    item.known.head.clone_from(&seen);
                    item.phase = Phase::Ci { head: seen, since };
                })?;
                Ok(Begin::Report(StepReport::Rebased {
                    issue,
                    pull_request: number,
                    head: rebased,
                }))
            }
            Ok(Rebase::Conflicts { main, files }) => {
                self.conflicted(number, head.to_owned(), main, files)
            }
            Ok(Rebase::Refused(reason)) => self.raise(number, RulingKind::Rebase { reason }),
            Err(e) => Ok(self.gate_failed(format!("cannot rebase #{number}: {e}"))),
        }
    }

    // The worker merges `main` in, so nothing is force-pushed. A conflict the
    // worker was already sent for this head, or for this `main`, is one it
    // did not resolve, and the maintainer decides. So does a work item that
    // has had `CONFLICT_TURNS` of them, whatever `main` does.
    fn conflicted(
        &mut self,
        number: u64,
        head: String,
        main: String,
        files: Vec<String>,
    ) -> Result<Begin, StateError> {
        let item = self
            .state
            .work_item
            .as_ref()
            .expect("a conflict is of a work item");
        if item.conflict.as_ref().is_some_and(|sent| {
            sent.head == head || sent.main == main || sent.turns >= CONFLICT_TURNS
        }) {
            let reason = format!("it conflicts with main in {}", files.join(", "));
            return self.raise(number, RulingKind::Rebase { reason });
        }
        let issue = item.issue;
        let prompt = conflict_prompt(number, &files);
        let turns = item.conflict.as_ref().map_or(0, |sent| sent.turns) + 1;
        let sent = Conflict {
            head: head.clone(),
            main,
            turns,
        };
        self.update(|item| {
            item.conflict = Some(sent);
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Implement;
        })?;
        Ok(Begin::Report(StepReport::Conflicted {
            issue,
            pull_request: number,
            head,
            files,
        }))
    }
}

fn conflict_prompt(number: u64, files: &[String]) -> String {
    format!(
        "Your pull request #{number} conflicts with main in {}. Run \
         `git fetch origin main` and `git merge origin/main`, resolve the conflicts, \
         and run the checks. Then commit the merge and push with `git push origin HEAD`. \
         Merge rather than rebase, and do not force-push.",
        files.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::ruling_report;
    use crate::ports::{Checks, Cost, Session, Usage};
    use crate::runner::{StepReport, step};
    use crate::test::{Rig, Scripted, git};

    #[test]
    fn a_conflict_with_main_is_the_workers_next_turn_naming_the_files() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        let main = rig.land_on_origin("work.txt");
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::Conflicted {
                issue: 7,
                pull_request: 71,
                head: head.clone(),
                files: vec!["work.txt".into()],
            })
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head.clone()));
        assert_eq!(git(&rig.worktree_7(), &["status", "--porcelain"]), "");

        rig.claude.script([Scripted::MergeMain]);
        step(&runner).unwrap();
        let [first, conflict] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            conflict.session,
            Session::Resume(first.session.id().clone())
        );
        let named = "Your pull request #71 conflicts with main in work.txt. ";
        assert!(conflict.prompt.starts_with(named), "{}", conflict.prompt);
        assert!(
            conflict.prompt.contains("`git merge origin/main`")
                && conflict.prompt.contains("do not force-push"),
            "{}",
            conflict.prompt
        );
        let merged = rig.forge.head_of("kelpie/7").unwrap();
        assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD"]), merged);
        assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD^2"]), main);
        assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD^1"]), head);
    }

    #[test]
    fn a_conflict_the_worker_resolves_goes_on_to_the_merge_ruling_with_no_ruling_between() {
        let (rig, runner, _) = Rig::with_pull_request("rotom");
        rig.land_on_origin("work.txt");
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Conflicted { .. })
        ));
        rig.claude.script([Scripted::MergeMain]);
        step(&runner).unwrap();
        let merged = rig.forge.head_of("kelpie/7").unwrap();

        assert_eq!(step(&runner).unwrap(), None, "CI on the merge is pending");
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        rig.forge.set_checks(&merged, Checks::Passed);
        let (id, question) = ruling_report(rig.verdict(&runner));
        assert_eq!(id, 1);
        assert!(question.starts_with("Merge pull request #71"), "{question}");
        assert_eq!(rig.claude.calls().len(), 2, "one conflict turn went out");
    }

    #[test]
    fn a_branch_holding_the_workers_merge_is_caught_up_by_merging_never_rebased() {
        let (rig, runner, _) = Rig::with_pull_request("chelone");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude.script([Scripted::MergeMain]);
        step(&runner).unwrap();
        let resolved = rig.forge.head_of("kelpie/7").unwrap();
        assert_eq!(
            git(&rig.worktree_7(), &["show", "HEAD:work.txt"]),
            "landed elsewhere"
        );

        // Main deletes the file the worker resolved. Replaying the branch's
        // own commits over that applies cleanly, and would bring back the
        // branch's side of the conflict the worker settled.
        let other = rig.home.path().join("other");
        git(&other, &["pull", "--quiet", "origin", "main"]);
        git(&other, &["rm", "--quiet", "work.txt"]);
        git(&other, &["commit", "--quiet", "-m", "drop work.txt"]);
        git(&other, &["push", "--quiet", "origin", "main"]);

        let Some(StepReport::Rebased {
            head: caught_up, ..
        }) = step(&runner).unwrap()
        else {
            panic!("the branch was not caught up");
        };
        let worktree = rig.worktree_7();
        assert_eq!(rig.forge.head_of("kelpie/7").as_ref(), Some(&caught_up));
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), caught_up);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD^1"]), resolved);
        assert!(!git(&worktree, &["ls-tree", "--name-only", "HEAD"]).contains("work.txt"));
        assert_eq!(rig.claude.calls().len(), 2, "no turn for a clean catch-up");
    }

    #[test]
    fn a_conflict_the_worker_left_unresolved_parks_it_on_the_rebase_ruling() {
        let (rig, runner, head) = Rig::with_pull_request("shep");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();

        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with(
                "Kelpie cannot rebase pull request #71 onto main: \
                 it conflicts with main in work.txt. Once the branch is fixed, \
                 `shep trigger shep rule '1 yes'` has kelpie look again"
            ),
            "{question}"
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
        assert_eq!(rig.claude.calls().len(), 2, "the conflict went out once");
    }

    #[test]
    fn a_second_conflict_on_the_same_main_after_a_worker_turn_parks_it() {
        let (rig, runner, _) = Rig::with_pull_request("golbat");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        // The worker pushes something, so the head moved, but not the merge.
        rig.claude.script([Scripted::Push("extra.txt", "extra\n")]);
        step(&runner).unwrap();
        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&pushed, Checks::Passed);

        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.contains("it conflicts with main in work.txt"),
            "{question}"
        );
        assert_eq!(rig.claude.calls().len(), 2, "no third turn, no loop");
    }

    #[test]
    fn a_main_that_moves_before_every_check_gets_the_worker_three_conflict_turns_then_parks() {
        let (rig, runner, _) = Rig::with_pull_request("xilriws");
        rig.land_on_origin("work.txt");
        let other = rig.home.path().join("other");
        for (turn, file) in [(1, "one.txt"), (2, "two.txt"), (3, "three.txt")] {
            assert!(
                matches!(step(&runner).unwrap(), Some(StepReport::Conflicted { .. })),
                "conflict turn {turn}"
            );
            // The worker pushes, so the head moved, and main moves again.
            rig.claude.script([Scripted::Push(file, "extra\n")]);
            step(&runner).unwrap();
            git(&other, &["pull", "--quiet", "origin", "main"]);
            std::fs::write(other.join("work.txt"), format!("landed {turn}\n")).unwrap();
            git(&other, &["commit", "--quiet", "-am", "landed again"]);
            git(&other, &["push", "--quiet", "origin", "main"]);
        }
        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.contains("it conflicts with main in work.txt"),
            "{question}"
        );
        assert_eq!(rig.claude.calls().len(), 4, "the first turn and three more");
    }

    #[test]
    fn a_conflict_with_a_main_that_moved_again_goes_to_the_worker_again() {
        let (rig, runner, _) = Rig::with_pull_request("zeus");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude.script([Scripted::Push("extra.txt", "extra\n")]);
        step(&runner).unwrap();

        let other = rig.home.path().join("other");
        git(&other, &["pull", "--quiet", "origin", "main"]);
        std::fs::write(other.join("work.txt"), "landed again\n").unwrap();
        git(&other, &["commit", "--quiet", "-am", "landed again"]);
        git(&other, &["push", "--quiet", "origin", "main"]);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Conflicted { .. })
        ));
    }

    #[test]
    fn a_yes_after_the_maintainer_fixes_a_conflict_looks_at_ci_again() {
        let (rig, runner, _) = Rig::with_pull_request("rotom");
        let landed = rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
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
}
