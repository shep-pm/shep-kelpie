//! Which tests the review pins, what a session leaves in the worktree, and what the worker defers

use super::*;

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
