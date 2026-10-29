//! Keeping a pull request's branch current with `main` before its merge
//!
//! A branch behind `main` is rebased and pushed, or merged if it is someone
//! else's. A conflict is the worker's next turn, in a merge kelpie started, a
//! few times at most, and then the maintainer's ruling. The gate's catch-up
//! goes on to CI; one from a review's wait for green goes back to that review.

use crate::runner::Runner;
use crate::runner::report::{Begin, StepReport};
use crate::runner::worktree_jobs::Caught;
use crate::state::{StateError, Stuck};
use crate::work_item::{Conflict, Phase, Review, Turn};
use crate::worktree::{self, Rebase};

// The most conflict turns one work item gets before the maintainer decides,
// so a `main` that moves before every check cannot loop the worker.
const CONFLICT_TURNS: u32 = 3;

impl Runner {
    // `main` moved since the branch was cut or last rebased: rebase and push,
    // and CI runs again on the new head before anyone is asked. From a
    // `review` waiting on its head's run, the new head is that review's to
    // wait on and read.
    pub(in crate::runner) fn rebase(
        &mut self,
        number: u64,
        head: &str,
        review: Option<Review>,
    ) -> Result<Begin, StateError> {
        let Some(change) = self.rebasing(number, head, review) else {
            return Ok(Begin::Idle);
        };
        self.changing(change, true)
    }

    // The branch was caught up, or could not be: the step's end, after the push
    pub(in crate::runner) fn caught_up(&mut self, caught: Caught) -> Result<Begin, StateError> {
        let Caught {
            number,
            head,
            review,
            outcome,
        } = caught;
        let head = head.as_str();
        let Some(issue) = self.current().map(|item| item.issue) else {
            return Ok(Begin::Idle);
        };
        match outcome {
            Ok(Rebase::Pushed(rebased)) => {
                let (seen, since) = (Some(rebased.clone()), self.ports.clock.now());
                // Kelpie's own catch-up carries what vouched for the head it rebased.
                let vouched = self.current().is_some_and(|item| item.vouches_for(head));
                self.update(|item| {
                    if vouched {
                        item.send_unread(rebased.clone());
                    }
                    item.caught_up(seen.clone());
                    item.phase = match review.clone() {
                        Some(review) => Phase::Review(Review {
                            reading: seen,
                            ci: None,
                            ..review
                        }),
                        None => Phase::Ci { head: seen, since },
                    };
                })?;
                Ok(Begin::Report(StepReport::Rebased {
                    issue,
                    pull_request: number,
                    head: rebased,
                }))
            }
            Ok(Rebase::Conflicts { main, files }) => {
                self.conflicted(number, head.to_owned(), main, files, review)
            }
            Ok(Rebase::Refused(why)) => self.raise(number, Stuck::Rebase { why, review }.into()),
            Err(e) => Ok(self.gate_failed(format!("cannot rebase #{number}: {e}"))),
        }
    }

    // Kelpie has started the merge of `main`, so the worker only resolves it.
    // Nothing is force-pushed. A conflict the worker was already sent for is
    // one it did not resolve. The maintainer decides then, and so does a work
    // item that has had `CONFLICT_TURNS` of them.
    fn conflicted(
        &mut self,
        number: u64,
        head: String,
        main: String,
        files: Vec<String>,
        review: Option<Review>,
    ) -> Result<Begin, StateError> {
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        if item.conflict.as_ref().is_some_and(|sent| {
            sent.head == head || sent.main == main || sent.turns >= CONFLICT_TURNS
        }) {
            // No turn goes out for the merge kelpie just started, and a worktree
            // left in it could not be caught up again after the maintainer's yes.
            let (repo, tree) = (&self.settings.git.checkout, &item.worktree);
            if let Err(e) = worktree::abort_merge(repo, tree) {
                eprintln!("cannot abort the merge in #{number}'s worktree: {e}");
            }
            let why = format!("it conflicts with main in {}", files.join(", "));
            return self.raise(number, Stuck::Rebase { why, review }.into());
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
            if let Some(review) = review {
                item.resume = Some(Phase::Review(review));
            }
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
        "Your pull request #{number} conflicts with main in {}. A merge of main is already \
         in progress in your worktree, with main's other changes staged. Resolve those files, \
         run `git add` on each, and run the checks. Then finish the merge with \
         `git commit --no-edit` and push with `git push origin HEAD`. Do not start another \
         merge or rebase, and do not force-push.",
        files.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use crate::test::ticket;
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
                issue: ticket(7),
                pull_request: 71,
                head: head.clone(),
                files: vec!["work.txt".into()],
            })
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head.clone()));
        assert_eq!(
            git(&rig.worktree_7(), &["status", "--porcelain"]),
            "AA work.txt"
        );

        rig.claude.script([Scripted::FinishMerge]);
        step(&runner).unwrap();
        let [first, conflict] = rig.claude.calls().try_into().unwrap();
        assert_eq!(
            conflict.session,
            Session::Resume(first.session.id().clone())
        );
        let named = "Your pull request #71 conflicts with main in work.txt. ";
        assert!(conflict.prompt.starts_with(named), "{}", conflict.prompt);
        assert!(
            conflict.prompt.contains("do not force-push"),
            "{}",
            conflict.prompt
        );
        let merged = rig.forge.head_of("kelpie/7").unwrap();
        assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD"]), merged);
        assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD^2"]), main);
        assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD^1"]), head);
    }

    #[test]
    fn kelpie_starts_the_merge_so_the_worker_finds_mains_agent_files_staged_and_only_the_conflict_left()
     {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.land(".claude/settings.json", "{\"enabledPlugins\":{}}\n");
        let main = rig.land_on_origin("work.txt");
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Conflicted { .. })
        ));

        let worktree = rig.worktree_7();
        assert_eq!(git(&worktree, &["rev-parse", "MERGE_HEAD"]), main);
        assert_eq!(
            git(&worktree, &["diff", "--name-only", "--diff-filter=U"]),
            "work.txt"
        );
        assert_eq!(
            git(
                &worktree,
                &["diff", "--cached", "--name-only", "--diff-filter=A"]
            ),
            ".claude/settings.json"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join(".claude/settings.json")).unwrap(),
            "{\"enabledPlugins\":{}}\n"
        );

        rig.claude.script([Scripted::FinishMerge]);
        step(&runner).unwrap();
        let merged = rig.forge.head_of("kelpie/7").unwrap();
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), merged);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD^1"]), head);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD^2"]), main);
    }

    #[test]
    fn the_conflict_prompt_names_the_files_and_says_to_finish_the_merge_not_start_one() {
        let (rig, runner, _) = Rig::with_pull_request("koji");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude.script([Scripted::FinishMerge]);
        step(&runner).unwrap();
        let prompt = rig.claude.calls().pop().unwrap().prompt;
        assert!(
            prompt.contains("merge of main is already in progress")
                && prompt.contains("work.txt")
                && prompt.contains("`git add`")
                && prompt.contains("`git commit --no-edit`")
                && prompt.contains("`git push origin HEAD`"),
            "{prompt}"
        );
        assert!(
            !prompt.contains("git fetch") && !prompt.contains("`git merge"),
            "{prompt}"
        );
    }

    #[test]
    fn a_worktree_left_mid_merge_is_not_merged_again() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        let first = rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        // The worker's turn ends before it commits the merge.
        rig.claude.script([
            Scripted::Reply(Usage::default(), Cost(1)),
            Scripted::Reply(Usage::default(), Cost(1)),
        ]);
        step(&runner).unwrap();
        step(&runner).unwrap(); // sent back once to commit the merge
        rig.land("more.txt", "more\n");

        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(question.contains("in the middle of a merge"), "{question}");
        let worktree = rig.worktree_7();
        assert_eq!(git(&worktree, &["rev-parse", "MERGE_HEAD"]), first);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
        assert!(!worktree.join("more.txt").exists());
    }

    #[test]
    fn a_rebase_that_conflicts_where_a_merge_would_not_is_merged_and_pushed_with_no_worker_turn() {
        let (rig, runner, _) = Rig::with_pull_request("koji");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        // The worker gives the first conflict up and pushes a second commit.
        rig.claude
            .script([Scripted::AbortAndPush("work.txt", "final\n")]);
        step(&runner).unwrap();
        let head = rig.forge.head_of("kelpie/7").unwrap();
        // Main ends where the branch does, by another road: replaying the
        // branch's first commit conflicts, and merging the two does not.
        let main = rig.land("work.txt", "final\n");
        let worktree = rig.worktree_7();

        let reported = step(&runner).unwrap();
        let Some(StepReport::Rebased { head: merged, .. }) = reported else {
            panic!("the branch was not caught up: {reported:?}");
        };
        assert_eq!(rig.forge.head_of("kelpie/7").as_ref(), Some(&merged));
        assert_eq!(git(&worktree, &["rev-parse", "HEAD^1"]), head);
        assert_eq!(git(&worktree, &["rev-parse", "HEAD^2"]), main);
        assert_eq!(rig.claude.calls().len(), 2, "no second conflict turn");
    }

    #[test]
    fn a_clean_merge_that_cannot_be_committed_is_aborted_so_the_next_look_can_retry() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::AbortAndPush("work.txt", "final\n")]);
        step(&runner).unwrap();
        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        rig.land("work.txt", "final\n");
        // Signing fails, so the merge cannot be committed.
        let worktree = rig.worktree_7();
        git(&worktree, &["config", "commit.gpgsign", "true"]);
        git(&worktree, &["config", "gpg.program", "false"]);

        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::GateFailed { .. })
        ));
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(pushed.clone()));
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), pushed);
        assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
        assert_ne!(pushed, head);
        assert!(
            !rig.git_dir_7().join("MERGE_HEAD").exists(),
            "the merge was left in progress"
        );
    }

    #[test]
    fn a_conflict_turn_does_not_ask_for_the_body() {
        let (rig, runner, _) = Rig::with_pull_request("koji");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude.script([Scripted::FinishMerge]);
        step(&runner).unwrap();
        let conflict = rig.claude.calls().pop().unwrap();
        assert!(
            conflict.prompt.contains("conflicts with main"),
            "{}",
            conflict.prompt
        );
        assert!(!conflict.prompt.contains("body"), "{}", conflict.prompt);
    }

    #[test]
    fn a_conflict_the_worker_resolves_goes_on_to_the_merge_ruling_with_no_ruling_between() {
        let (rig, runner, _) = Rig::with_pull_request("rotom");
        rig.land_on_origin("work.txt");
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Conflicted { .. })
        ));
        rig.claude.script([Scripted::FinishMerge]);
        step(&runner).unwrap();
        let merged = rig.forge.head_of("kelpie/7").unwrap();

        assert_eq!(step(&runner).unwrap(), None, "CI on the merge is pending");
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        rig.forge.set_checks(&merged, Checks::Passed);
        let (id, question) = ruling_report(rig.verdict(&runner));
        assert_eq!(id, ticket(71));
        assert!(question.starts_with("Merge pull request #71"), "{question}");
        assert_eq!(rig.claude.calls().len(), 2, "one conflict turn went out");
    }

    #[test]
    fn a_branch_holding_the_workers_merge_is_caught_up_by_merging_never_rebased() {
        let (rig, runner, _) = Rig::with_pull_request("chelone");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude.script([Scripted::FinishMerge]);
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
        rig.claude.script([
            Scripted::Reply(Usage::default(), Cost(1)),
            Scripted::Reply(Usage::default(), Cost(1)),
        ]);
        step(&runner).unwrap();
        step(&runner).unwrap(); // sent back once to commit the merge

        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.starts_with(
                "Kelpie cannot rebase pull request #71 onto main: \
                 its worktree is in the middle of a merge. Once the branch is fixed, \
                 `shep kelpie rule 71 yes` has kelpie look again"
            ),
            "{question}"
        );
        assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
        assert_eq!(
            rig.claude.calls().len(),
            3,
            "the conflict went out once, and the worker was sent back once to commit it"
        );
    }

    #[test]
    fn a_second_conflict_on_the_same_main_after_a_worker_turn_parks_it() {
        let (rig, runner, _) = Rig::with_pull_request("golbat");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        // The worker pushes something, so the head moved, but not the merge.
        rig.claude
            .script([Scripted::AbortAndPush("extra.txt", "extra\n")]);
        step(&runner).unwrap();
        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&pushed, Checks::Passed);

        let (_, question) = ruling_report(step(&runner).unwrap());
        assert!(
            question.contains("it conflicts with main in work.txt"),
            "{question}"
        );
        assert_eq!(rig.claude.calls().len(), 2, "no third turn, no loop");
        // The merge kelpie started for a turn it will not send is undone, so
        // the maintainer's yes finds a worktree it can catch up again.
        let worktree = rig.worktree_7();
        assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), pushed);
        let merge_head = git(&worktree, &["rev-parse", "--git-path", "MERGE_HEAD"]);
        assert!(
            !worktree.join(&merge_head).exists(),
            "the merge was left in progress"
        );
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
            rig.claude.script([Scripted::AbortAndPush(file, "extra\n")]);
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
        let (rig, runner, _) = Rig::with_pull_request("acme");
        rig.land_on_origin("work.txt");
        step(&runner).unwrap();
        rig.claude
            .script([Scripted::AbortAndPush("extra.txt", "extra\n")]);
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
        rig.claude.script([
            Scripted::Reply(Usage::default(), Cost(1)),
            Scripted::Reply(Usage::default(), Cost(1)),
        ]);
        step(&runner).unwrap();
        step(&runner).unwrap(); // sent back once to commit the merge
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
        assert_eq!(next, id);
        assert!(question.starts_with("Merge pull request #71"), "{question}");
    }

    #[test]
    fn a_worktree_with_uncommitted_changes_is_not_rebased() {
        let (rig, runner, head) = Rig::with_pull_request("acme");
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
    fn a_worker_that_repoints_its_clones_objects_gets_no_rebase_run_there() {
        let (rig, runner, head) = Rig::with_pull_request("golbat");
        let wanted = rig.home.path().join("wanted");
        std::fs::create_dir_all(wanted.join("objects")).unwrap();
        let alternates = rig.git_dir_7().join("objects/info/alternates");
        std::fs::write(
            alternates,
            format!("{}\n", wanted.join("objects").display()),
        )
        .unwrap();
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
