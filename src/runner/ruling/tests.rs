//! Rulings through the runner's stand-ins

use serde_json::json;

use super::*;
use crate::ports::{Checks, Session};
use crate::runner::gate::CHECKS_SETTLE;
use crate::runner::step;
use crate::test::{Rig, Scripted};

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
        }),
        RulingKind::from(Stuck::StillRed {
            head: "abcdef123".into(),
            checks: vec!["test".into(), "lint".into()],
        }),
        RulingKind::from(Stuck::Closed),
        RulingKind::from(Stuck::FixNotPushed {
            fix: Fix::Review(review),
            prompt: "fix it".into(),
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
        unread_head: false,
    };
    assert_eq!(comment(&merge), None);
}

#[test]
fn nothing_merges_without_a_yes() {
    let (rig, runner, _) = Rig::parked("shep");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    for _ in 0..5 {
        rig.clock.advance(3600);
        assert_eq!(step(&runner).unwrap(), None);
    }
    let refused = [
        ("2 yes", "no ruling 2 is pending"),
        (
            "1 no",
            "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 no\"",
        ),
        (
            "1 yes please",
            "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 yes please\"",
        ),
        (
            "one yes",
            "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"one yes\"",
        ),
        (
            "1 maybe",
            "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 maybe\"",
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

// A no's fix is new code the review has not seen: it goes back through
// a pass of the review, not straight to CI, before the next ruling.
#[test]
fn a_no_sends_the_note_to_the_worker_and_it_goes_through_review_before_the_next_ruling() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(&runner, "rule", Some("1 no  rename the flag to --dry-run "));
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
         rename the flag to --dry-run\n"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review",
        "a no's fix starts a pass of the review, not CI directly"
    );

    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI on the new head is pending"
    );
    rig.forge.set_checks(&pushed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 2, .. })
    ));
    let rulings = &rig.ask(&runner, "status", None)["rulings"];
    assert_eq!(
        rulings[0]["kind"],
        json!({ "kind": "merge", "head": pushed })
    );
    assert_eq!(rulings.as_array().unwrap().len(), 1);
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn ruling_ids_are_never_given_twice() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(&runner, "rule", Some("1 yes"));
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { merged: true, .. })
    ));
    drop(runner);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(72, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("again.txt", "again\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 2, .. })
    ));
    assert_eq!(
        rig.ask(&runner, "rule", Some("1 yes")),
        json!({ "error": "no ruling 1 is pending" })
    );
}

#[test]
fn a_no_on_a_commit_pushed_by_hand_has_the_worker_build_on_it_without_force() {
    let (rig, runner, _) = Rig::with_pull_request("rotom");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.ask(&runner, "rule", Some("1 no revert it"));
    // A plain push from the worktree: the stand-in panics if it is refused.
    rig.claude.script([
        Scripted::Push("revert.txt", "reverted\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap();
    let noted = rig.claude.calls().pop().unwrap();
    assert_eq!(
        noted.prompt,
        format!(
            "Someone other than you pushed commit {} to pull request #71, and the \
             maintainer declined it, with this note:\n\nrevert it\n\nYour worktree is \
             now at that commit. Revert or change it with a new commit on top, and \
             push with `git push origin HEAD`. Do not force-push.\n",
            &by_hand[..7]
        )
    );
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    let parent = crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD^"]);
    assert_eq!(parent, by_hand);

    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    rig.forge.set_checks(&pushed, Checks::Passed);
    let Some(StepReport::Ruling {
        id: 2, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no second ruling");
    };
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_yes_on_a_head_the_branch_moved_past_asks_about_the_new_head_instead() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.push_by_hand("kelpie/7", "first.txt");
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let second = rig.push_by_hand("kelpie/7", "second.txt");
    let status = rig.ask(&runner, "rule", Some("1 yes"));
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": 2 })
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
        Some(StepReport::Ruling { id: 1, .. })
    ));
    std::fs::write(rig.worktree_7().join("work.txt"), "unsaved\n").unwrap();
    let reply = rig.ask(&runner, "rule", Some("1 yes"));
    let error = reply["error"].as_str().unwrap();
    assert!(
        error.starts_with(&format!("cannot bring the worktree to {}: ", &by_hand[..7])),
        "{error}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({ "state": "ruling", "id": 1 })
    );
    assert_eq!(
        crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD"]),
        head
    );

    // A commit kelpie never saw pushed is the worker's too.
    let worktree = rig.worktree_7();
    crate::test::git(&worktree, &["commit", "--quiet", "-am", "not pushed"]);
    let reply = rig.ask(&runner, "rule", Some("1 yes"));
    assert!(
        reply["error"].as_str().unwrap().contains("holds work"),
        "{reply}"
    );
}
