//! A review of a worktree whose worker pointed its git elsewhere
//!
//! The worker can write the worktree's `.git` file and its own git dir.
//! Git that follows either reads the config of the repo it names, and a
//! review's git runs outside the sandbox, so the review never follows them.

use std::sync::Mutex;

use crate::ports::Role;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Elsewhere, Rig, Scripted};

// A runner whose project lists qwen and then `second`, past the worker's
// turn, which pushed `file`, and qwen's round, so the next step is `second`'s.
fn at_round_2(rig: &Rig, second: &str, file: &'static str) -> Mutex<Runner> {
    rig.reviewers(&["qwen", second]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push(file, "work\n"), Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    runner
}

// The prompt of the reviewer's session, which carries the diff it read.
fn reviewer_prompt(rig: &Rig) -> String {
    let seen = rig.claude.all_seen();
    let reviewer = seen.iter().find(|s| s.call.role == Role::Reviewer);
    reviewer.expect("a reviewer round ran").call.prompt.clone()
}

#[test]
fn a_reviewer_reads_the_worktrees_own_diff_when_its_git_file_points_elsewhere() {
    let rig = Rig::new("shep");
    let runner = at_round_2(&rig, "claude", "work.txt");
    let worktree = rig.worktree_7();
    let elsewhere = Elsewhere::copy_of(&rig.repo(), rig.home.path());
    elsewhere.as_git_dir_of(&worktree);

    step(&runner).unwrap(); // round 2, claude
    assert!(!elsewhere.ran.exists(), "the review started the program");
    assert!(
        reviewer_prompt(&rig).contains("+work"),
        "the diff is the work item's own"
    );
    elsewhere.assert_plain_git_starts_it(&worktree);
}

#[test]
fn choosing_a_reviewer_by_its_paths_never_follows_a_git_file_pointed_elsewhere() {
    let rig = Rig::new("shep");
    rig.write_agent(
        "opus",
        "---\nrole: reviewer\nharness: claude-code\nmodel: claude-opus-5-5\neffort: high\n\
         paths: [\"src/**\"]\n---\nRead {{DIFF}} for defects.\n",
    );
    let runner = at_round_2(&rig, "opus", "src/work.rs");
    let worktree = rig.worktree_7();
    let elsewhere = Elsewhere::copy_of(&rig.repo(), rig.home.path());
    elsewhere.as_git_dir_of(&worktree);

    step(&runner).unwrap(); // round 2: opus, whose paths the change matches
    assert!(
        !elsewhere.ran.exists(),
        "choosing the reviewer started the program"
    );
    assert!(reviewer_prompt(&rig).contains("+work"), "opus was chosen");
    elsewhere.assert_plain_git_starts_it(&worktree);
}

// The step's fence check reads the same trusted git, and parks the worker.
#[test]
fn a_review_of_a_worktree_whose_commondir_points_elsewhere_is_refused() {
    let rig = Rig::new("shep");
    let runner = at_round_2(&rig, "claude", "work.txt");
    let worktree = rig.worktree_7();
    let elsewhere = Elsewhere::copy_of(&rig.repo(), rig.home.path());
    elsewhere.as_common_dir_of(&worktree);

    match step(&runner).unwrap() {
        Some(StepReport::Failed { question, .. }) => {
            assert!(
                question.contains("is not this work item's worktree"),
                "{question}"
            );
        }
        other => panic!("the round went ahead: {other:?}"),
    }
    assert!(!elsewhere.ran.exists(), "the review started the program");
    assert!(
        rig.claude
            .all_seen()
            .iter()
            .all(|s| s.call.role != Role::Reviewer),
        "no reviewer ran"
    );
    elsewhere.assert_plain_git_starts_it(&worktree);
}
