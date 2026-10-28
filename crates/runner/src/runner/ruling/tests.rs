//! Rulings through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::{CLEAN, Checks, Cost, Session, Usage};
use crate::runner::gate::CHECKS_SETTLE;
use crate::runner::step;
use crate::test::ticket;
use crate::test::{Rig, Scripted, ScriptedRound};

// The pull request is public: it gets what happened and who it waits on,
// and never a command, which the webhook carries instead.
#[test]
fn a_ruling_on_the_pull_request_names_no_command_and_a_merge_says_nothing() {
    let review = Review::first();
    let known = Known {
        labels: vec![],
        ready: false,
        head: None,
    };
    let kinds = [
        RulingKind::from(Stuck::Rebase {
            why: "conflict in a.txt".into(),
            review: None,
        }),
        RulingKind::from(Stuck::StillRed {
            head: "abcdef123".into(),
            checks: vec!["test".into(), "lint".into()],
            fix_turns: None,
            review: None,
            rerun: false,
        }),
        RulingKind::from(Stuck::Closed),
        RulingKind::from(Stuck::FixNotPushed {
            fix: Fix::Review(review),
            prompt: "fix it".into(),
            said: None,
        }),
        RulingKind::Question {
            asked: "Which flag?".into(),
            resume: Resume::Nothing,
        },
        RulingKind::from(Stuck::TurnTimeout { phase: None }),
        RulingKind::from(Stuck::Unpushed {
            files: vec!["late.txt".into()],
            head: "a1".into(),
            pushed: "b2".into(),
            review: Review::first(),
        }),
        RulingKind::from(Stuck::TurnFailed {
            why: "boom".into(),
            phase: Phase::Implement,
            retry: Turn::Next { prompt: "x".into() },
            restore: Vec::new(),
            setup: false,
        }),
        RulingKind::ForeignChange {
            description: "the `bug` label was added".into(),
            known,
        },
    ];
    for kind in kinds {
        let said = comment(&kind).unwrap_or_default();
        assert!(said.ends_with("\n\nWaiting on the maintainer."), "{said}");
        for internal in [
            "shep trigger",
            "shep kelpie",
            "rule '",
            "ruling",
            "yes",
            "<note>",
        ] {
            assert!(!said.contains(internal), "{internal} in {said}");
        }
    }
    let merge = RulingKind::Merge {
        head: "abc".into(),
        unreviewed: None,
        open_threads: None,
        held_by: Vec::new(),
        unread_head: false,
        note_fix: false,
        late_fix: false,
        nits: None,
        disputes: None,
        rivals: Vec::new(),
    };
    assert_eq!(comment(&merge), None);
}

#[test]
fn nothing_merges_without_a_yes() {
    let (rig, runner, _) = Rig::parked("shep");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    for _ in 0..5 {
        rig.clock.advance(3600);
        assert_eq!(step(&runner).unwrap(), None);
    }
    let refused = [
        ("2 yes", "ruling 2 is already answered, or was never raised"),
        (
            "71 no",
            "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`, not \"71 no\"",
        ),
        (
            "71 yes please",
            "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`, not \"71 yes please\"",
        ),
        (
            "one yes",
            "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`, not \"one yes\"",
        ),
        (
            "71 maybe",
            "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`, not \"71 maybe\"",
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

// A merge ruling's no is a fix of what the maintainer read: it goes to CI
// and back to the merge ruling, with no new pass of the review, and its
// head is the worker's own fix, so the ruling does not call it unread.
#[test]
fn a_no_on_a_merge_ruling_sends_the_note_and_its_fix_goes_to_ci_and_back_to_the_ruling() {
    let (rig, runner, _) = Rig::parked("rotom");
    let reads = (rig.reviewer.seen().len(), rig.claude.all_calls().len());
    rig.ask(
        &runner,
        "rule",
        Some("71 no  rename the flag to --dry-run "),
    );
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);
    step(&runner).unwrap(); // the noted turn: pushes, and goes to CI
    let calls = rig.claude.calls();
    let (first, noted) = (&calls[0], calls.last().unwrap());
    assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
    assert_eq!(
        noted.prompt,
        "The maintainer answered no on pull request #71, with this note:\n\n\
         rename the flag to --dry-run\n\nUpdate the pull request's body so it matches the \
         head you push: what changed, the files touched, the test evidence, and the \
         decisions you made.\n"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci",
        "a no's fix goes to CI with no pass of the review"
    );

    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI on the new head is pending"
    );
    rig.forge.set_checks(&pushed, Checks::Passed);
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("no second merge ruling");
    };
    assert_eq!(id, ticket(71));
    assert!(
        question.contains(
            "into main? This head is the worker's fix for a note or comment, and no reviewer read it."
        ),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": pushed, "note_fix": true })
    );
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert!(
        item.noted_from.is_some(),
        "kept until ruling 71 is answered"
    );
    assert_eq!(
        (rig.reviewer.seen().len(), rig.claude.all_calls().len()),
        (reads.0, reads.1 + 1),
        "no reviewer, local or Claude, read the fix: the only call was the worker's"
    );
    assert_eq!(rig.forge.merges(), []);
}

// A rework is a change that needs the whole review: it goes back through a
// pass of the review, not straight to CI, before the next ruling.
#[test]
fn a_rework_sends_the_note_to_the_worker_and_it_goes_through_review_before_the_next_ruling() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(
        &runner,
        "rule",
        Some("71 rework  rename the flag to --dry-run "),
    );
    rig.claude.script([
        Scripted::Push("rename.txt", "renamed\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap(); // the noted turn: pushes, enters round 1
    let [first, noted] = rig.claude.calls().try_into().unwrap();
    assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
    assert_eq!(
        noted.prompt,
        "The maintainer asked for a rework of pull request #71, with this note:\n\n\
         rename the flag to --dry-run\n\nOnce you push it, the whole review reads your \
         change again. Update the pull request's body so it matches the head you push: \
         what changed, the files touched, the test evidence, and the decisions you made.\n"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review",
        "a rework starts a pass of the review, not CI directly"
    );

    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&pushed, Checks::Passed);
    rig.verdict(&runner); // review round 1, qwen: clean by default, once its head is green
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the new head's run has not settled"
    );
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(71)));
    let rulings = &rig.ask(&runner, "status", None)["rulings"];
    assert_eq!(
        rulings[0]["kind"],
        json!({ "kind": "merge", "head": pushed })
    );
    assert_eq!(rulings.as_array().unwrap().len(), 1);
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_merge_rulings_no_note_and_a_rework_ask_for_the_body_to_be_kept_true() {
    let ask = "Update the pull request's body so it matches the head you push";
    for answer in ["71 no  fix the name ", "71 rework  fix the name "] {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some(answer));
        rig.claude
            .script([Scripted::Push("rename.txt", "renamed\n")]);
        step(&runner).unwrap(); // the noted turn
        let noted = rig.claude.calls().pop().unwrap();
        assert!(noted.prompt.contains(ask), "{answer}: {}", noted.prompt);
    }
}

#[test]
fn a_ruling_answered_is_refused_when_its_number_is_named_again() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(&runner, "rule", Some("71 yes"));
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { merged: true, .. })
    ));
    drop(runner);
    let runner = rig.open().unwrap();
    rig.forge.reopen_issue(7);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(72, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("again.txt", "again\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // review round 1, qwen: clean by default, once its head is green
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(72)
    ));
    assert_eq!(
        rig.ask(&runner, "rule", Some("71 yes")),
        json!({ "error": "ruling 71 is already answered, or was never raised" })
    );
}

#[test]
fn a_no_on_a_commit_pushed_by_hand_has_the_worker_build_on_it_without_force() {
    let (rig, runner, _) = Rig::with_pull_request("rotom");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    assert_eq!(
        rig.ask(&runner, "rule", Some("71 rework revert it")),
        json!({ "error": "ruling 71 is not a merge ruling, so it takes a yes, or a no with a note" }),
        "only a merge ruling takes a rework"
    );
    rig.ask(&runner, "rule", Some("71 no revert it"));
    // A plain push from the worktree: the stand-in panics if it is refused.
    rig.claude.script([
        Scripted::Push("revert.txt", "reverted\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap();
    let noted = rig.claude.calls().pop().unwrap();
    assert_eq!(
        noted.prompt,
        format!(
            "Someone other than you pushed commit {} to pull request #71, and the \
             maintainer declined it, with this note:\n\nrevert it\n\nYour worktree is \
             now at that commit. Revert or change it with a new commit on top, and \
             push with `git push origin HEAD`. Do not force-push. Update the pull \
             request's body so it matches the head you push: what changed, the files \
             touched, the test evidence, and the decisions you made.\n",
            &by_hand[..7]
        )
    );
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    let parent = crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD^"]);
    assert_eq!(parent, by_hand);

    rig.forge.set_checks(&pushed, Checks::Passed);
    rig.verdict(&runner); // review round 1, qwen: clean by default, once its head is green
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("no second ruling");
    };
    assert_eq!(id, ticket(71));
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_yes_on_a_head_the_branch_moved_past_asks_about_the_new_head_instead() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.push_by_hand("kelpie/7", "first.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    let second = rig.push_by_hand("kelpie/7", "second.txt");
    let status = rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "71" })
    );
    assert_eq!(status["rulings"][0]["kind"]["known"]["head"], json!(second));
    let question = status["rulings"][0]["question"].as_str().unwrap();
    assert!(
        question.starts_with(&format!(
            "Pull request #71 changed outside kelpie: its head moved to {}",
            &second[..7]
        )),
        "{question}"
    );
    assert_eq!(
        crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD"]),
        head
    );
}

#[test]
fn a_yes_on_a_head_is_refused_while_the_worktree_holds_work_not_pushed() {
    let (rig, runner, head) = Rig::with_pull_request("chelone");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    std::fs::write(rig.worktree_7().join("work.txt"), "unsaved\n").unwrap();
    let reply = rig.ask(&runner, "rule", Some("71 yes"));
    let error = reply["error"].as_str().unwrap();
    assert!(
        error.starts_with(&format!("cannot bring the worktree to {}: ", &by_hand[..7])),
        "{error}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": "71" })
    );
    assert_eq!(
        crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD"]),
        head
    );

    // A commit kelpie never saw pushed is the worker's too.
    let worktree = rig.worktree_7();
    crate::test::git(&worktree, &["commit", "--quiet", "-am", "not pushed"]);
    let reply = rig.ask(&runner, "rule", Some("71 yes"));
    assert!(
        reply["error"].as_str().unwrap().contains("holds work"),
        "{reply}"
    );
}

// A turn that failed before the harness started never saw its prompt, so the
// yes sends it again behind a line saying the last turn failed.
#[test]
fn a_yes_on_a_fix_turn_that_failed_before_starting_sends_the_findings_again() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let finding = |line| {
        crate::ports::Finding::new(
            crate::ports::Severity::Medium,
            "src/lib.rs",
            line,
            "the flag is misnamed",
            "it reads as its opposite",
        )
    };
    rig.reviewer
        .script([ScriptedRound::Findings((1..=5).map(finding).collect())]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the five findings go to the worker

    rig.claude
        .script([Scripted::Fail(crate::ports::AgentError::Failed(
            crate::settings::Harness::ClaudeCode,
            "the sandbox refused the settings file".into(),
        ))]);
    let Some(StepReport::Failed { id, .. }) = step(&runner).unwrap() else {
        panic!("the fix turn did not fail");
    };
    let sent = rig.claude.calls().last().unwrap().prompt.clone();
    assert!(sent.contains("found 5 finding(s)"), "{sent}");

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap();
    let retried = rig.claude.calls().last().unwrap().prompt.clone();
    let (line, rest) = retried.split_once('\n').expect("a line, then the prompt");
    assert!(line.contains("last turn failed"), "{line}");
    assert_eq!(rest.trim_start(), sent);
}

// The retried turn's prompt is kept in its turn, so a second failure and a
// second yes must not stack another line on it.
#[test]
fn a_prompt_that_already_carries_the_failed_turn_line_gets_no_second_one() {
    for prompt in ["fix the five", "/plugin:review check the diff"] {
        let once = failed_again(prompt.to_owned());
        assert_eq!(failed_again(once.clone()), once);
        assert_eq!(once.matches("last turn failed").count(), 1, "{once}");
    }
}

// A command only runs from the very start of a prompt, so a retried prompt
// that begins with one keeps it first.
#[test]
fn the_failed_turn_line_goes_after_a_command_the_prompt_starts_with() {
    let again = failed_again("/plugin:review check the diff".to_owned());
    assert_eq!(
        again,
        "/plugin:review Your last turn failed before it finished. \
         Kelpie sends its prompt again.\n\ncheck the diff"
    );
}

const DOLLAR: u64 = 1_000_000_000;

// Issue 7 under a $5 cap, whose first turn cost `dollars` and so waits on ruling 7
fn over_its_cap_by(dollars: u64) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("acme");
    rig.edit_settings(|s| s.replace("per_item_usd = 10", "per_item_usd = 5"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(dollars * DOLLAR))]);
    step(&runner).unwrap();
    let Some(StepReport::Failed { id, .. }) = step(&runner).unwrap() else {
        panic!("the work item did not wait on a ruling");
    };
    assert_eq!(id, ticket(7));
    assert_eq!(rig.claude.calls().len(), 1);
    (rig, runner)
}

fn over_its_cap() -> (Rig, Mutex<Runner>) {
    over_its_cap_by(6)
}

fn cap_usd(rig: &Rig, runner: &Mutex<Runner>) -> f64 {
    rig.ask(runner, "status", None)["work_item"]["budget"]["cap_usd"]
        .as_f64()
        .unwrap()
}

#[test]
fn a_yes_on_an_over_budget_ruling_lets_the_work_item_run_to_twice_its_cap() {
    let (rig, runner) = over_its_cap();
    rig.ask(&runner, "rule", Some("7 yes"));
    // $9 in all is past $5 and under the doubled $10.
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(9 * DOLLAR))]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 2, "the worker carried on");
    assert_eq!(cap_usd(&rig, &runner), 10.0);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_no_on_an_over_budget_ruling_sends_the_note_and_raises_the_cap_once() {
    let (rig, runner) = over_its_cap();
    rig.ask(
        &runner,
        "rule",
        Some("7 no  stop polishing and open the pull request "),
    );
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(9 * DOLLAR))]);
    step(&runner).unwrap();
    let calls = rig.claude.calls();
    let (first, noted) = (&calls[0], calls.last().unwrap());
    assert_eq!(calls.len(), 2);
    assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
    let (note, owed) = noted
        .prompt
        .split_once("The turn that was due is still yours, and follows:\n\n")
        .unwrap();
    assert_eq!(
        note,
        "The maintainer answered no on your work item, with this note:\n\n\
         stop polishing and open the pull request\n\n"
    );
    assert!(
        owed.starts_with("Your last turn ended with no pull request"),
        "the turn that was due is kept: {owed}"
    );
    assert_eq!(cap_usd(&rig, &runner), 10.0, "raised once, not twice");
}

#[test]
fn a_yes_lifts_the_cap_past_what_one_long_turn_cost_so_the_worker_is_not_asked_again() {
    // $11 is past the doubled $10, so the cap goes to $20 and the next turn starts.
    let (rig, runner) = over_its_cap_by(11);
    rig.ask(&runner, "rule", Some("7 yes"));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(12 * DOLLAR))]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 2, "the worker carried on");
    assert_eq!(cap_usd(&rig, &runner), 20.0);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_no_on_a_first_turn_that_has_no_session_to_resume_starts_it_and_says_the_note_was_not_sent() {
    let (rig, runner) = over_its_cap();
    runner
        .lock()
        .unwrap()
        .update(|item| item.turn = Turn::Due)
        .unwrap();
    rig.ask(&runner, "rule", Some("7 no  polish less"));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(7 * DOLLAR))]);
    step(&runner).unwrap();
    let calls = rig.claude.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        matches!(calls[1].session, Session::New(_)),
        "{:?}",
        calls[1].session
    );
    assert_eq!(cap_usd(&rig, &runner), 10.0);
    let notes = runner.lock().unwrap().take_notes();
    assert_eq!(
        notes,
        ["ruling 7: the worker's session has not begun, so your note was not sent"]
    );
}

// The maintainer answers with the number they read, not a counter: the pull
// request's once there is one, the issue's before.
#[test]
fn the_question_names_the_pull_request_number_or_the_issue_number_before_one_opens() {
    let kind = RulingKind::from(Stuck::Rebase {
        why: "conflict in a.txt".into(),
        review: None,
    });
    let with_pull_request = question(ticket(7), Some(71), &kind);
    assert!(
        with_pull_request.contains("`shep kelpie rule 71 yes`"),
        "{with_pull_request}"
    );
    assert!(
        !with_pull_request.contains("rule 1 "),
        "{with_pull_request}"
    );
    let before_one = question(ticket(7), None, &kind);
    assert!(
        before_one.contains("`shep kelpie rule 7 no <note>`"),
        "{before_one}"
    );
}

const FLAG_TEST: &str = "tests/flag.rs";
const FLAG_TEXT: &str = "#[test]\nfn flag_is_read() { assert!(false); }\n";

fn failed(output: &str) -> crate::ports::TestOutcome {
    crate::ports::TestOutcome::Failed {
        output: output.to_owned(),
    }
}

// Round 2's reviewer attached a test that fails before the fix and after both
// of the worker's fixes: ruling 71 waits on it.
fn a_test_still_failing_after_two_fixes() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.test_command("cargo test --test {name}");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.tests
        .script([failed("before"), failed("after one"), failed("after two")]);
    rig.claude.script([Scripted::Attach(
        FLAG_TEST,
        FLAG_TEXT,
        "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs",
    )]);
    step(&runner).unwrap(); // round 2, claude
    step(&runner).unwrap(); // the findings sent
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap(); // the first fix turn
    step(&runner).unwrap(); // the fix ended: the worker is sent back
    rig.claude.script([Scripted::Push("again.txt", "again\n")]);
    step(&runner).unwrap(); // the second fix turn
    let raised = step(&runner).unwrap();
    assert!(
        matches!(raised, Some(StepReport::Ruling { id, .. }) if id == ticket(71)),
        "{raised:?}"
    );
    (rig, runner)
}

#[test]
fn a_yes_on_a_failing_test_sends_one_more_fix_turn() {
    let (rig, runner) = a_test_still_failing_after_two_fixes();

    rig.ask(&runner, "rule", Some("71 yes"));
    rig.tests.script([crate::ports::TestOutcome::Passed]);
    rig.claude.script([Scripted::Push("third.txt", "third\n")]);
    step(&runner).unwrap(); // the third fix turn
    let prompt = rig.claude.calls().last().unwrap().prompt.clone();
    assert!(prompt.contains(FLAG_TEST), "{prompt}");
    assert!(prompt.contains("after two"), "{prompt}");
    assert!(prompt.contains("leave the test alone"), "{prompt}");

    step(&runner).unwrap(); // the fix ended: the test runs again
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["phase"]["state"], "ci");
    assert_eq!(
        item["test_checks"][0]["after"],
        json!({ "result": "passed" })
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_no_on_it_lets_the_pass_go_on() {
    let (rig, runner) = a_test_still_failing_after_two_fixes();
    let reads = rig.reviewer.seen().len();

    rig.ask(&runner, "rule", Some("71 no leave that test be"));
    rig.claude.script([Scripted::Text("Understood.")]);
    step(&runner).unwrap(); // the worker's turn on the note
    let note = rig.claude.calls().last().unwrap().prompt.clone();
    assert!(note.contains("leave that test be"), "{note}");

    step(&runner).unwrap(); // the turn ended, and the pass goes on
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), reads, "no round ran again");
    assert_eq!(rig.tests.ran().len(), 3, "the test is not run again");
    assert_eq!(
        status["work_item"]["test_checks"][0]["after"],
        json!({ "result": "failed", "output": "after two" }),
        "status keeps the failed check"
    );
}

// A commit the note's turn pushes is a fix turn's push: kelpie vouches for it,
// and the merge gate does not name it unread.
#[test]
fn a_commit_pushed_on_a_no_is_vouched_for() {
    let (rig, runner) = a_test_still_failing_after_two_fixes();

    rig.ask(&runner, "rule", Some("71 no rename it instead"));
    rig.claude.script([Scripted::Push("note.txt", "noted\n")]);
    step(&runner).unwrap(); // the worker's turn on the note
    step(&runner).unwrap(); // the turn ended, and the pass goes on

    let runner = runner.lock().unwrap();
    let item = &runner.state.work_items[0];
    let head = rig.forge.head_of("kelpie/7").unwrap();
    assert!(item.vouches_for(&head));
    assert!(matches!(item.phase, Phase::Ci { .. }), "{:?}", item.phase);
    assert!(item.nit_fix_heads.is_empty(), "{:?}", item.nit_fix_heads);
}

// The pull request is public, and a test's output may hold anything: its
// comment says that a test fails, and the terminal question carries the tails.
#[test]
fn the_public_line_names_no_test_output() {
    let kind = RulingKind::from(Stuck::TestStillFails {
        fix: Fix::Review(Review::first()),
        head: "abcdef1234567".into(),
        tests: vec![crate::state::FailingTest {
            test: FLAG_TEST.into(),
            output: "panicked at the secret token".into(),
        }],
        prompt: "fix it".into(),
    });

    let said = comment(&kind).expect("a stuck ruling is said");

    assert_eq!(
        said,
        "A reviewer's test still fails after two fixes.\n\nWaiting on the maintainer."
    );
    let asked = question(ticket(7), Some(71), &kind);
    assert!(asked.contains("panicked at the secret token"), "{asked}");
    assert!(asked.contains(FLAG_TEST), "{asked}");
    assert!(asked.contains("abcdef1"), "{asked}");
    assert!(asked.contains("`shep kelpie rule 71 yes`"), "{asked}");
}
