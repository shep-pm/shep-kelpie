//! A new project's review, through the runner and its stand-ins

use std::sync::Mutex;

use serde_json::Value;

use crate::ports::{AgentError, Cost, Role, SessionId, Tools, Usage};
use crate::runner::report::{ReviewResult, Spent};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, Seen};
use crate::work_item::CallKind;

// A new project whose worker opened pull request 71 and whose local round
// found nothing, so the deep round is next.
fn at_the_deep_round() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.deep_review();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    (rig, runner)
}

fn state(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let status = rig.ask(runner, "status", None);
    status["work_item"]["phase"]["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

// Steps until the work item leaves review, or for any ruling, and returns what
// the steps reported.
fn until_it_leaves_review(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    let mut reports = Vec::new();
    for _ in 0..40 {
        if state(rig, runner) != "review" {
            return reports;
        }
        let report = step(runner).unwrap();
        let ruling = matches!(report, Some(StepReport::Ruling { .. }));
        reports.extend(report);
        if ruling {
            return reports;
        }
    }
    panic!("the review never ended: {reports:#?}");
}

fn roles(rig: &Rig) -> Vec<Role> {
    rig.claude.all_calls().iter().map(|c| c.role).collect()
}

fn deep_calls(rig: &Rig) -> Vec<Seen> {
    let seen = rig.claude.all_seen();
    seen.into_iter()
        .filter(|s| s.call.role == Role::DeepReviewer)
        .collect()
}

const MEDIUM: &str =
    "MEDIUM|work.txt:1|the flag is read before it is set|a caller sees the old value";
const OTHER: &str = "MEDIUM|src/other.rs:9|an empty list panics|the reviewer sees a crash";
// What the confirming session writes, and what a worker commits with its fix.
const FAILING_TEST: &str = "#[test]\nfn fails() { panic!() }\n";
const HIGH: &str = "HIGH|work.txt:1|the value is read before it is set|a caller gets nothing back";

#[test]
fn a_new_projects_review_is_one_deep_round_one_fix_turn_and_one_recheck_of_the_fix() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text(OTHER),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("FIXED|1|work.txt:1 sets it first\nFIXED|2|the empty list returns early"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);

    assert_eq!(
        state(&rig, &runner),
        "ci",
        "the loop ends after the re-check"
    );
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,       // opens the pull request
            Role::DeepReviewer, // the first reader
            Role::DeepReviewer, // the second reader, shown the first's findings
            Role::Worker,       // the one fix turn, for both lists
            Role::DeepReviewer, // the re-check of the fix
        ]
    );
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepRechecked {
                fixed: 2,
                unfixed: 0,
                ..
            }
        )),
        "{reports:#?}"
    );

    let deep = deep_calls(&rig);
    for seen in &deep {
        assert_eq!(seen.call.model, "claude-opus-5-5");
        assert_eq!(seen.call.effort, Effort::High);
    }
    let sessions: Vec<String> = deep.iter().map(|s| s.call.session.id().0.clone()).collect();
    assert_eq!(
        sessions
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "every session is a fresh one"
    );
    assert_eq!(deep[0].call.tools, Tools::Review);
    assert_eq!(deep[1].call.tools, Tools::Review);
    assert!(
        deep[0]
            .call
            .prompt
            .contains("for defects: bugs a careful maintainer")
    );
    assert!(
        !deep[0]
            .call
            .prompt
            .contains("Another reader has already read")
    );
    assert!(
        deep[1]
            .call
            .prompt
            .contains("Another reader has already read")
    );
    assert!(
        deep[1].call.prompt.contains(MEDIUM),
        "the second reader is shown the first's"
    );
    assert!(
        deep[0].call.prompt.contains("#7 "),
        "the issue it checks the change against: {}",
        deep[0].call.prompt
    );

    let fix = rig.claude.calls().pop().unwrap();
    assert!(fix.prompt.contains("held 2 finding(s)"), "{}", fix.prompt);
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains(MEDIUM) && held.contains(OTHER),
        "both lists go to the fix turn: {held}"
    );

    assert_eq!(
        deep[2].call.tools,
        Tools::Work,
        "the re-check runs commands"
    );
    let shown = deep[2]
        .call
        .prompt
        .split_once("--- the fix, diff since")
        .unwrap()
        .1;
    assert!(
        shown.contains("+++ b/fixed.txt"),
        "it is shown the fix commits: {shown}"
    );
    assert!(!shown.contains("work.txt"), "and only those: {shown}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["by_role"]["deep_reviewer"]["calls"], 3);
}

#[test]
fn two_readers_that_find_nothing_end_the_review_with_no_fix_turn() {
    let (rig, runner) = at_the_deep_round();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig),
        [Role::Worker, Role::DeepReviewer, Role::DeepReviewer]
    );
    assert!(
        reports.contains(&StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 2,
            held: 0,
            clean: true,
        }),
        "{reports:#?}"
    );
    let second = &deep_calls(&rig)[1].call.prompt;
    assert!(second.contains("--- the first reader's findings ---\nCLEAN\n--- end ---"));
}

#[test]
fn a_high_is_confirmed_with_a_failing_test_that_the_fix_is_rechecked_by_running() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            FAILING_TEST,
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        Scripted::PushMany(&[("fixed.txt", "fixed\n"), ("tests/high.rs", FAILING_TEST)]),
        Scripted::Text("FIXED|1|`cargo test --test high` passes: 1 passed"),
    ]);
    until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,
            Role::DeepReviewer,
            Role::DeepReviewer,
            Role::DeepReviewer, // the confirmation
            Role::Worker,
            Role::DeepReviewer, // the re-check
        ]
    );

    let deep = deep_calls(&rig);
    let confirm = &deep[2].call;
    assert_eq!(confirm.tools, Tools::Work, "it may run commands");
    assert!(confirm.prompt.contains(HIGH), "{}", confirm.prompt);
    assert!(confirm.prompt.contains("failing test"));
    let fence = confirm.reach.fence.as_ref().expect("it is fenced");
    let (worktree, build) = (confirm.cwd.clone(), rig.build_7());
    assert_eq!(
        fence.write,
        [worktree, build],
        "it writes its test and builds, and can commit nothing"
    );

    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("failing test, in tests/high.rs: `cargo test --test high`"),
        "{held}"
    );
    let recheck = &deep[3].call;
    assert_eq!(
        recheck.tools,
        Tools::Work,
        "it runs the test, which it may do"
    );
    assert!(
        recheck.prompt.contains("`cargo test --test high`")
            && recheck.prompt.contains("tests/high.rs"),
        "it is told which test to run: {}",
        recheck.prompt
    );
    assert!(recheck.prompt.contains("run that test now"));
    let fence = recheck.reach.fence.as_ref().expect("it is fenced");
    assert_eq!(
        fence.write,
        [rig.build_7()],
        "it writes its builds and not the source it verifies"
    );
}

// The worker's fix turn of a HIGH the confirming session wrote `FAILING_TEST` for,
// scripted as `fix`, parks on the ruling that says why, and nothing re-checks it.
fn parks_with(fix: Scripted, says: &str) {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            FAILING_TEST,
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        fix,
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { question, .. }) = reports.last() else {
        panic!("a fix that left the review's test behind raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("ended its fix for the deep review (round 2), but "),
        "{question}"
    );
    assert!(
        !question.contains("without pushing"),
        "a fix that pushed is not one that pushed nothing: {question}"
    );
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::Worker),
        "what is not pushed is not verified"
    );
    assert!(
        question.contains(says),
        "the question names its cause: {question}"
    );
    let status = rig.ask(&runner, "status", None);
    let prompt = status["rulings"][0]["kind"]["prompt"].as_str().unwrap();
    assert!(prompt.contains(says), "{prompt}");
}

#[test]
fn a_test_the_review_wrote_and_the_worker_left_uncommitted_counts_as_a_fix_not_pushed() {
    parks_with(
        Scripted::Push("fixed.txt", "fixed\n"),
        "tests/high.rs are changed or new and not committed",
    );
}

#[test]
fn a_test_the_worker_removed_is_not_in_the_pushed_head() {
    parks_with(
        Scripted::PushAndRemove("fixed.txt", "fixed\n", "tests/high.rs"),
        "the review's test for work.txt:1, in tests/high.rs, is not in the pushed head",
    );
}

#[test]
fn a_file_a_session_wrote_beside_its_test_is_pinned_too() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::WriteMany(
            &[
                ("tests/high.rs", FAILING_TEST),
                ("tests/fixture.txt", "the input\nthat fails\n"),
            ],
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        // The worker commits both, and changes what the fixture holds.
        Scripted::PushMany(&[
            ("fixed.txt", "fixed\n"),
            ("tests/high.rs", FAILING_TEST),
            ("tests/fixture.txt", "another input\n"),
        ]),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { question, .. }) = reports.last() else {
        panic!("a fixture changed after the review wrote it raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("in tests/fixture.txt, is not in the pushed head as it was written"),
        "{question}"
    );
}

const ONE: &str = "#[test]\nfn one() { panic!() }\n";

// Two HIGHs, one confirming session each, whose tests go in the same file.
fn two_highs_in_one_file(second: Scripted, committed: Scripted) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(
            "HIGH|work.txt:1|the value is read before it is set|a caller gets nothing back\n\
             HIGH|src/other.rs:9|an empty list panics|the reviewer sees a crash",
        ),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/shared.rs",
            ONE,
            "CONFIRMED|tests/shared.rs|cargo test one",
        ),
        second,
        committed,
        Scripted::Text("FIXED|1|passes\nFIXED|2|passes"),
    ]);
    (rig, runner)
}

#[test]
fn two_highs_whose_tests_go_in_the_same_file_are_rechecked_once_the_worker_commits_them() {
    let both = "#[test]\nfn one() { panic!() }\n#[test]\nfn two() { panic!() }\n";
    let (rig, runner) = two_highs_in_one_file(
        Scripted::Write(
            "tests/shared.rs",
            both,
            "CONFIRMED|tests/shared.rs|cargo test two",
        ),
        // The worker commits the file with its own line above, which is fine.
        Scripted::PushMany(&[
            ("fixed.txt", "fixed\n"),
            (
                "tests/shared.rs",
                "// the worker's note\n#[test]\nfn one() { panic!() }\n#[test]\nfn two() { panic!() }\n",
            ),
        ]),
    );
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci", "{reports:#?}");
    assert!(
        !reports
            .iter()
            .any(|r| matches!(r, StepReport::Ruling { .. })),
        "{reports:#?}"
    );
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::DeepReviewer),
        "the re-check ran"
    );
}

#[test]
fn a_later_session_that_rewrites_an_earlier_sessions_test_takes_its_pin_along() {
    const REWRITTEN: &str =
        "#[test]\nfn one() { assert!(false) }\n#[test]\nfn two() { panic!() }\n";
    let (rig, runner) = two_highs_in_one_file(
        Scripted::Write(
            "tests/shared.rs",
            REWRITTEN,
            "CONFIRMED|tests/shared.rs|cargo test two",
        ),
        Scripted::PushMany(&[("fixed.txt", "fixed\n"), ("tests/shared.rs", REWRITTEN)]),
    );
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci", "{reports:#?}");
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::DeepReviewer),
        "the re-check ran"
    );
}

#[test]
fn a_session_that_does_not_confirm_leaves_nothing_for_the_worker_to_commit() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        // It half writes a test and edits a tracked file, then gives up.
        Scripted::WriteMany(
            &[
                ("tests/half.rs", "#[test]\nfn half() {"),
                ("work.txt", "edited by the session\n"),
            ],
            "UNCONFIRMED|it could not be made to fail",
        ),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("FIXED|1|it sets the value first"),
    ]);
    step(&runner).unwrap(); // the first reader
    step(&runner).unwrap(); // the second reader
    step(&runner).unwrap(); // the confirmation, which fails
    let tree = rig.worktree_7();
    assert_eq!(
        crate::test::git(&tree, &["status", "--porcelain"]),
        "",
        "its new file is gone and its edit reverted"
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("work.txt")).unwrap(),
        "work\n"
    );

    // So the worker's fix is not parked for a file it was never meant to commit.
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci", "{reports:#?}");
    assert!(
        !reports
            .iter()
            .any(|r| matches!(r, StepReport::Ruling { .. })),
        "{reports:#?}"
    );
}

#[test]
fn a_session_whose_reply_cannot_be_read_puts_back_what_it_found_not_the_head() {
    let (rig, runner) = at_the_deep_round();
    let tree = rig.worktree_7();
    // The worker's own file, not committed, before the session ran.
    std::fs::write(tree.join("notes.txt"), "mine\n").unwrap();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::WriteMany(
            &[("tests/half.rs", "#[test]"), ("notes.txt", "theirs\n")],
            "I think it is real.",
        ),
    ]);
    step(&runner).unwrap(); // the first reader
    step(&runner).unwrap(); // the second reader
    let report = step(&runner).unwrap(); // the confirmation, whose reply says nothing
    assert!(
        matches!(
            report,
            Some(StepReport::DeepConfirmed { backed: false, .. })
        ),
        "{report:#?}"
    );
    assert!(!tree.join("tests/half.rs").exists());
    assert_eq!(
        std::fs::read_to_string(tree.join("notes.txt")).unwrap(),
        "mine\n",
        "what was there before the session stays"
    );
}

#[test]
fn a_confirmation_stopped_with_the_runner_is_restored_to_what_its_first_try_found() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        // The runner stops with the session after it wrote a file.
        Scripted::WriteThenFail("tests/half.rs", "#[test]", AgentError::Stopped),
    ]);
    step(&runner).unwrap(); // the first reader
    step(&runner).unwrap(); // the second reader
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "a stopped call reports nothing"
    );
    let half = rig.worktree_7().join("tests/half.rs");
    assert!(half.exists(), "the stopped session left its file");
    drop(runner);

    // The retry must not take its snapshot over what the first try left.
    let runner = rig.open().unwrap();
    rig.claude
        .script([Scripted::Text("UNCONFIRMED|it could not be made to fail")]);
    let report = step(&runner).unwrap();
    assert!(
        matches!(
            report,
            Some(StepReport::DeepConfirmed { backed: false, .. })
        ),
        "{report:#?}"
    );
    assert!(!half.exists(), "what the first try wrote is gone");
}

#[test]
fn a_deep_call_that_ends_after_its_stage_moved_on_still_keeps_its_cost_and_its_end() {
    let (rig, runner) = at_the_deep_round(); // its stage is the next round's, not a deep step
    let report = {
        let mut runner = runner.lock().unwrap();
        runner.mark_review_call_running(CallKind::Deep).unwrap();
        let spent = Spent::Claude {
            role: Role::DeepReviewer,
            session: SessionId("late".into()),
            usage: Usage::default(),
            session_cost: Some(Cost(7)),
        };
        runner
            .end_deep(ReviewResult::Deep(Ok("CLEAN".into())), Some(spent))
            .unwrap()
    };
    assert_eq!(report, None, "there is no step left for it to answer");
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["by_role"]["deep_reviewer"]["calls"], 1, "{item}");
    assert_ne!(item["review_call"]["state"], "running", "{item}");
}

#[test]
fn a_deletion_the_worker_had_not_committed_stays_deleted_when_a_session_does_not_confirm() {
    let (rig, runner) = at_the_deep_round();
    let tree = rig.worktree_7();
    std::fs::remove_file(tree.join("work.txt")).unwrap(); // a tracked file, deleted and not committed
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::WriteMany(
            &[("tests/half.rs", "#[test]")],
            "UNCONFIRMED|it could not be made to fail",
        ),
    ]);
    step(&runner).unwrap(); // the first reader
    step(&runner).unwrap(); // the second reader
    step(&runner).unwrap(); // the confirmation, which does not confirm
    assert!(
        !tree.join("tests/half.rs").exists(),
        "the session's file is gone"
    );
    assert!(
        !tree.join("work.txt").exists(),
        "the worktree is as it was before the session, deletion and all"
    );
}

#[test]
fn a_pin_that_cannot_be_read_leaves_the_high_unconfirmed() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        // The session breaks the worktree's link to its repo, so git cannot say what it added.
        Scripted::WriteAndBreakGit(
            "tests/high.rs",
            FAILING_TEST,
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
    ]);
    step(&runner).unwrap(); // the first reader
    step(&runner).unwrap(); // the second reader
    let report = step(&runner).unwrap(); // the confirmation
    assert!(
        matches!(
            report,
            Some(StepReport::DeepConfirmed { backed: false, .. })
        ),
        "{report:#?}"
    );
    let status = rig.ask(&runner, "status", None);
    let backing = &status["work_item"]["phase"]["stage"]["held"][0]["backing"];
    assert_eq!(backing["backing"], "unconfirmed", "{status}");
    assert!(
        backing["why"]
            .as_str()
            .unwrap()
            .contains("kelpie could not read what the session wrote"),
        "{backing}"
    );
}

#[test]
fn a_test_the_worker_edited_before_committing_it_counts_as_a_fix_not_pushed() {
    parks_with(
        Scripted::PushMany(&[
            ("fixed.txt", "fixed\n"),
            ("tests/high.rs", "#[test]\nfn fails() {}\n"),
        ]),
        "the review's test for work.txt:1, in tests/high.rs, is not in the pushed head as it was written: `fn fails() { panic!() }` is gone",
    );
}

#[test]
fn any_other_new_file_left_in_the_worktree_counts_as_a_fix_not_pushed() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        Scripted::PushAndPlant("fixed.txt", "fixed\n", "notes.txt"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert!(
        matches!(reports.last(), Some(StepReport::Ruling { .. })),
        "{reports:#?}"
    );
    assert_eq!(roles(&rig).last(), Some(&Role::Worker));
    let status = rig.ask(&runner, "status", None);
    let prompt = status["rulings"][0]["kind"]["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("notes.txt are changed or new and not committed"),
        "{prompt}"
    );
}

#[test]
fn a_finding_the_worker_deferred_is_resolved_at_the_recheck_not_unfixed() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text(OTHER),
        Scripted::Push("fixed.txt", "fixed\n"),
        // Only the finding the worker did not defer is asked about.
        Scripted::Text("FIXED|1|it sets the flag first"),
    ]);
    // The worker copied the second finding's line to the file kelpie files issues from.
    std::fs::create_dir_all(rig.build_7()).unwrap();
    // The worker's copy is not byte for byte: a different why, and spaces after it.
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        "MEDIUM|src/other.rs:9|an empty list panics|reworded by the worker  \n",
    )
    .unwrap();
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(
        state(&rig, &runner),
        "ci",
        "the deferred finding is not sent back"
    );
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepRechecked {
                fixed: 1,
                deferred: 1,
                unfixed: 0,
                ..
            }
        )),
        "{reports:#?}"
    );
    let recheck = &deep_calls(&rig)[2].call.prompt;
    assert!(!recheck.contains("src/other.rs"), "{recheck}");
}

#[test]
fn a_high_backed_by_a_failing_test_can_be_deferred_by_taking_its_test_out() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            FAILING_TEST,
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        // The worker leaves the finding, and the failing test with it, out of its commit.
        Scripted::PushAndRemove("fixed.txt", "fixed\n", "tests/high.rs"),
    ]);
    std::fs::create_dir_all(rig.build_7()).unwrap();
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        format!("{HIGH}\n"),
    )
    .unwrap();
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci", "{reports:#?}");
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepRechecked {
                fixed: 0,
                deferred: 1,
                unfixed: 0,
                ..
            }
        )),
        "{reports:#?}"
    );
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::Worker),
        "nothing is left to re-check"
    );
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("remove that test from your worktree and do not commit it"),
        "the findings file says what to do with a deferred test: {held}"
    );
}

#[test]
fn a_worker_that_defers_every_finding_has_nothing_to_push_and_the_review_goes_on() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text(OTHER),
        // It defers both and pushes nothing, which is all it has to do.
        Scripted::Say("Both are out of scope."),
    ]);
    std::fs::create_dir_all(rig.build_7()).unwrap();
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        format!("{MEDIUM}\n{OTHER}\n"),
    )
    .unwrap();
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci", "{reports:#?}");
    assert!(
        !reports
            .iter()
            .any(|r| matches!(r, StepReport::Ruling { .. })),
        "an unmoved head is no failure when nothing was to be pushed: {reports:#?}"
    );
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepRechecked {
                fixed: 0,
                deferred: 2,
                unfixed: 0,
                ..
            }
        )),
        "{reports:#?}"
    );
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::Worker),
        "there is nothing left to re-check"
    );
}

#[test]
fn deferring_every_finding_still_leaves_no_test_in_the_worktree() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            FAILING_TEST,
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        // It defers the finding but leaves the test it was told to remove.
        Scripted::Say("Out of scope."),
        // Kelpie sends a turn that ends with uncommitted files back once.
        Scripted::Say("Still out of scope."),
    ]);
    std::fs::create_dir_all(rig.build_7()).unwrap();
    std::fs::write(
        rig.build_7().join("deferred-findings.md"),
        format!("{HIGH}\n"),
    )
    .unwrap();
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { question, .. }) = reports.last() else {
        panic!("a test left behind raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("but tests/high.rs are changed or new and not committed"),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    let prompt = status["rulings"][0]["kind"]["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("deferred every finding") && prompt.contains("there is nothing to push"),
        "{prompt}"
    );
}

#[test]
fn a_fix_pushed_in_part_is_not_rechecked_but_parks_like_one_that_pushed_nothing() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        // work.txt is the worker's own, tracked file: its edit stays uncommitted.
        Scripted::PushLeaving("fix.txt", "fix\n", "work.txt"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { .. }) = reports.last() else {
        panic!("a fix left in part raised no ruling: {reports:#?}");
    };
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::Worker),
        "the worktree is not what was pushed, so nothing verifies it"
    );
    let status = rig.ask(&runner, "status", None);
    let prompt = status["rulings"][0]["kind"]["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("not all of your fix: work.txt are changed or new and not committed"),
        "{prompt}"
    );
}

#[test]
fn a_fix_the_failing_test_still_fails_for_goes_back_to_the_worker_once_and_then_to_a_ruling() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            FAILING_TEST,
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        Scripted::PushMany(&[("one.txt", "1\n"), ("tests/high.rs", FAILING_TEST)]),
        Scripted::Text("UNFIXED|1|`cargo test --test high` still fails: the value is read first"),
        Scripted::Push("two.txt", "2\n"),
        Scripted::Text("UNFIXED|1|it still fails the same way"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { id, question, .. }) = reports.last().cloned() else {
        panic!("the second unfixed re-check raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("re-checked the worker's fix twice and still finds these unfixed"),
        "{question}"
    );
    assert!(
        question.contains("it still fails the same way"),
        "{question}"
    );
    assert_eq!(
        roles(&rig).iter().filter(|r| **r == Role::Worker).count(),
        3,
        "the first turn, the fix, and the one trip back"
    );
    let again = rig.claude.calls()[2].prompt.clone();
    assert!(
        again.starts_with("Kelpie's re-check of your fix"),
        "{again}"
    );
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("still unfixed after your fix: it still fails the same way"),
        "the worker is told what is still wrong: {held}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "deep-review");

    // A yes sends it the findings once more, and the re-check of that fix ends the loop.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([
        Scripted::Push("three.txt", "3\n"),
        Scripted::Text("FIXED|1|`cargo test --test high` passes"),
    ]);
    until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig).iter().filter(|r| **r == Role::Worker).count(),
        4
    );
}

#[test]
fn a_second_recheck_whose_ruling_cannot_be_written_still_keeps_what_the_call_cost() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        Scripted::Push("one.txt", "1\n"),
        Scripted::Text("UNFIXED|1|it still panics"),
        Scripted::Push("two.txt", "2\n"),
        Scripted::Text("UNFIXED|1|it still panics"),
    ]);
    // Step to where the second fix turn has ended, and take away what the ruling is written to.
    for _ in 0..30 {
        if roles(&rig).len() == 6 {
            break;
        }
        step(&runner).unwrap();
    }
    assert_eq!(roles(&rig).len(), 6, "{:?}", roles(&rig));
    let held = rig.build_7().join("review-findings.md");
    std::fs::remove_file(&held).unwrap();
    std::fs::create_dir(&held).unwrap();
    step(&runner).unwrap(); // the fix is seen to have pushed
    let report = step(&runner).unwrap(); // the second re-check, which cannot raise its ruling
    assert!(
        matches!(&report, Some(StepReport::GateFailed { reason, .. }) if reason.contains("re-check")),
        "{report:#?}"
    );

    let status = rig.ask(&runner, "status", None);
    let item = &status["work_item"];
    assert_ne!(
        item["review_call"]["state"], "running",
        "the call ended: {item}"
    );
    assert_eq!(
        item["by_role"]["deep_reviewer"]["calls"], 4,
        "both readers and both re-checks are counted"
    );
    assert_eq!(item["phase"]["stage"]["step"], "rechecking");
}

#[test]
fn a_high_no_session_could_confirm_goes_to_the_fix_turn_marked_unconfirmed() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Text("UNCONFIRMED|a caller already guards it"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("FIXED|1|the value is set first now"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepConfirmed { backed: false, finding, .. } if finding == "work.txt:1"
        )),
        "{reports:#?}"
    );
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("unconfirmed, no failing test could be made: a caller already guards it"),
        "{held}"
    );
    assert!(held.contains(HIGH), "it still goes to the worker: {held}");
}

#[test]
fn a_fix_turn_that_pushes_nothing_parks_instead_of_being_rechecked() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        Scripted::Say("I could not read the findings."),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { question, .. }) = reports.last() else {
        panic!("a fix with nothing pushed raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("ended its fix for the deep review (round 2) without pushing"),
        "{question}"
    );
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::Worker),
        "no re-check of a fix that is not there"
    );
}

#[test]
fn a_recheck_that_says_nothing_of_the_findings_is_a_failed_gate_and_runs_again() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("Looks good to me."),
        Scripted::Text("FIXED|1|it sets the flag first"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert!(
        reports.iter().any(
            |r| matches!(r, StepReport::GateFailed { reason, .. } if reason.contains("re-check"))
        ),
        "{reports:#?}"
    );
    assert_eq!(state(&rig, &runner), "ci");
}

#[test]
fn the_deep_round_is_its_own_phase_in_status_and_timings() {
    let (rig, runner) = at_the_deep_round();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // the first reader
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"]["stage"],
        serde_json::json!({ "stage": "deep", "step": "missed", "first": [] }),
        "{status}"
    );
    let timings = rig.ask(&runner, "timings", None);
    let seconds: &Value = &timings["seconds"];
    assert!(seconds.get("deep_round").is_some(), "{timings}");
}
