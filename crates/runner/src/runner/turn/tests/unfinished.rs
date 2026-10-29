//! A turn that ends without finishing: sent back to commit or push, or parked as stopped short

use super::*;

#[test]
fn a_turn_that_ends_with_no_pull_request_and_no_question_sends_the_worker_back_once() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Say("cargo test failed in the sandbox, so I stopped."),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(7)
    ));
    let [first, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::Resume(first.session.id().clone()));
    assert_eq!(again.prompt, SENT_BACK);
}

#[test]
fn a_turn_that_ends_with_uncommitted_files_and_no_push_is_sent_back_naming_them() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Push("fix.txt", "fixed\n"),
    ]);
    step(&runner).unwrap();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(7)
    ));
    let [first, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::Resume(first.session.id().clone()));
    assert!(
        again
            .prompt
            .starts_with("Your last turn ended with uncommitted changes in your worktree"),
        "{}",
        again.prompt
    );
    assert!(again.prompt.contains("fix.txt"), "{}", again.prompt);
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "the worker was sent back before any ruling"
    );
}

#[test]
fn a_worker_sent_back_for_uncommitted_files_is_not_sent_back_for_them_again() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Plant("fix.txt", "fixed\n"),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    let [_, dirty, stopped] = rig.claude.calls().try_into().unwrap();
    assert!(dirty.prompt.contains("fix.txt"), "{}", dirty.prompt);
    assert_eq!(stopped.prompt, SENT_BACK);
}

// Turn 1 pushes the branch and opens no pull request. Turn 2 opens one, pushes
// nothing and leaves a file uncommitted, so it is sent back to commit. The
// pull request was found at turn 2's end, so the review is still owed.
#[test]
fn a_pull_request_found_on_a_turn_sent_back_to_commit_still_goes_to_review_first() {
    let (rig, runner) = with_issue_7("acme");
    let hold = Hold::default();
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Hold(hold.clone()),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        assert!(hold.entered(Duration::from_secs(30)), "turn 2 never ran");
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        write_in(&rig.worktree_7(), "left.txt", "left\n");
        hold.release();
        turn.join().unwrap().unwrap();
    });
    step(&runner).unwrap();
    let [_, _, commit] = rig.claude.calls().try_into().unwrap();
    assert!(commit.prompt.contains("left.txt"), "{}", commit.prompt);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["pull_request"], 71);
    assert_eq!(status["work_item"]["phase"]["state"], "review");
}

// A worktree whose git kelpie will not trust cannot be listed, and the turn
// ends as it did before, with no follow-up and no hold-up.
#[test]
fn a_worktree_git_cannot_be_read_in_ends_the_turn_without_a_follow_up() {
    let (rig, runner) = with_issue_7("acme");
    let hold = Hold::default();
    rig.claude.script([
        Scripted::Hold(hold.clone()),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    let worktree = rig.worktree_7();
    let ended = std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        assert!(hold.entered(Duration::from_secs(30)), "the turn never ran");
        write_in(&worktree, "left.txt", "left\n");
        // The clone no longer says whose objects it borrows.
        fs::remove_file(rig.git_dir_7().join("objects/info/alternates")).unwrap();
        hold.release();
        turn.join().unwrap().unwrap()
    });
    assert!(matches!(ended, Some(StepReport::Ended { issue, .. }) if issue == ticket(7)));
    let _ = step(&runner);
    for call in rig.claude.calls() {
        assert!(
            !call
                .prompt
                .starts_with("Your last turn ended with uncommitted"),
            "{}",
            call.prompt
        );
    }
}

#[test]
fn a_change_only_staged_counts_as_uncommitted() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Stage("staged.txt", "staged\n"),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [_, again] = rig.claude.calls().try_into().unwrap();
    assert!(again.prompt.contains("staged.txt"), "{}", again.prompt);
}

#[test]
fn a_turn_that_ends_with_a_clean_worktree_is_not_sent_back_for_uncommitted_files() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Reply(usage(1), Cost(1)),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [_, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.prompt, SENT_BACK);
}

#[test]
fn a_second_turn_that_stops_short_parks_the_worker_on_a_ruling() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([
        Scripted::Reply(usage(1), Cost(1)),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let Some(StepReport::Failed {
        issue,
        pull_request: None,
        id,
        question,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the worker was not parked");
    };
    assert_eq!(id, ticket(7));
    assert_eq!(issue, ticket(7));
    assert!(
        question.starts_with(
            "The worker's turn on issue #7 failed: \
             it ended twice with no pull request and no question."
        ),
        "{question}"
    );
    assert!(question.ends_with("stops the work item."), "{question}");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls().len(), 2);

    // A yes sends it back again, into the same session.
    rig.ask(&runner, "rule", Some("7 yes"));
    rig.claude.script([Scripted::Reply(usage(1), Cost(1))]);
    step(&runner).unwrap();
    let [first, _, third] = rig.claude.calls().try_into().unwrap();
    assert_eq!(third.session, Session::Resume(first.session.id().clone()));
    assert_eq!(third.prompt, SENT_BACK);
}

#[test]
fn a_no_on_a_worker_that_stopped_short_stops_the_work_item() {
    let (rig, runner) = with_issue_7("rotom");
    rig.claude.script([
        Scripted::Reply(usage(1), Cost(1)),
        Scripted::Reply(usage(1), Cost(1)),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    rig.ask(&runner, "rule", Some("7 no the issue is already fixed"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue,
            merged: false,
            ..
        })
     if issue == ticket(7)));
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_pull_request_the_turns_end_missed_goes_to_review_instead() {
    let (rig, runner) = with_issue_7("acme");
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.forge.set_board_down(true);
    step(&runner).unwrap();
    // The forge cannot be asked, so nothing is sent back yet.
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::GateFailed { issue, .. }) if issue == ticket(7)
    ));
    rig.forge.set_board_down(false);
    rig.forge.open_pull_request(70, "kelpie/7", &[7]);
    rig.next_look();
    step(&runner).unwrap();
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["pull_request"], 70);
    assert_eq!(item["phase"]["state"], "review");
    assert_eq!(rig.claude.calls().len(), 1, "the worker was not sent back");
}
