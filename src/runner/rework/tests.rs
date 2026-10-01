use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::{Checks, ReviewComment, Session};
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted, git};

const BODY: &str = "The timeline needs a redesign.\n\nKeep the dates, lose the cards.";

fn review() -> MaintainerReview {
    MaintainerReview {
        id: "PRR_71".into(),
        changes_requested: false,
        body: BODY.into(),
        comments: vec![
            ReviewComment {
                file: "src/Timeline.tsx".into(),
                line: Some(12),
                body: "Use the grid here, not flex.".into(),
            },
            ReviewComment {
                file: "src/old.css".into(),
                line: None,
                body: "Delete this file.".into(),
            },
        ],
    }
}

// Issue 7's dropped pull request 71 on `kelpie/7`, with a commit from its
// worker and one the maintainer pushed by hand, reviewed. Returns the
// maintainer's commit.
fn reviewed_71(rig: &Rig) -> String {
    rig.push_by_hand("kelpie/7", "work.txt");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.forge.ready_pull_request(71);
    rig.forge.review(71, review());
    by_hand
}

fn running(rig: &Rig) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    runner
}

#[test]
fn a_rework_starts_on_the_pull_requests_branch_with_the_review_as_its_first_turn() {
    let rig = Rig::new("webapp");
    let by_hand = reviewed_71(&rig);
    let runner = running(&rig);
    let item = &rig.ask(&runner, "rework", Some("71"))["work_item"];
    assert_eq!(
        (&item["issue"], &item["branch"], &item["pull_request"]),
        (&json!(7), &json!("kelpie/7"), &json!(71))
    );

    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    assert!(matches!(seen.call.session, Session::New(_)));
    assert_eq!(seen.call.cwd, rig.worktree_7());
    let path = rig.build_7().join("maintainer-review.md");
    assert_eq!(
        seen.call.prompt,
        format!(
            "Your work item reworks your pull request #71 for issue #7: Title of #7\n\n\
             Its latest review asks for changes, and is in {}. \
             Make the changes it asks for, then commit and push with `git push origin HEAD`. \
             Your branch is the pull request's as `origin` holds it, with any commits the \
             maintainer pushed. The pull request is already open, so do not open another.\n",
            path.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!(
            "# The latest review of pull request #71\n\n{BODY}\n\
             \n## On `src/Timeline.tsx` line 12\n\nUse the grid here, not flex.\n\
             \n## On `src/old.css`\n\nDelete this file.\n"
        )
    );
    rig.assert_worker_reads(&seen, &path);

    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        git(&rig.worktree_7(), &["rev-parse", &format!("{fixed}~1")]),
        by_hand,
        "the fix lands on the maintainer's commit, pushed without force"
    );
}

#[test]
fn a_reworked_pull_request_goes_through_every_gate_to_the_merge_ruling() {
    let rig = Rig::new("reactmap");
    reviewed_71(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.claude.script([
        Scripted::Push("fix.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the rework's turn
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review",
        "the fix is new code, so the qwen-review loop runs first"
    );
    step(&runner).unwrap(); // review round 1, qwen: clean
    step(&runner).unwrap(); // review round 2, claude: clean
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling {
        id: 1, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("the rework never reached its merge ruling");
    };
    assert!(question.contains("#71"), "{question}");
    assert_eq!(rig.forge.merges(), []);
    assert_eq!(rig.reviewer.seen().len(), 1);
}

#[test]
fn a_rework_turn_past_its_ceiling_still_owes_the_review_loop_after_a_yes() {
    let rig = Rig::new("zeus");
    reviewed_71(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.claude
        .script([Scripted::Fail(crate::ports::AgentError::TimedOut(
            crate::settings::Harness::ClaudeCode,
        ))]);
    let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
        panic!("the timed-out turn raised no ruling");
    };
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review"
    );
}

#[test]
fn a_branch_the_maintainer_pushed_to_since_is_where_the_worker_starts() {
    let rig = Rig::new("koji");
    reviewed_71(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    let later = rig.push_by_hand("kelpie/7", "later.txt");

    rig.claude.script([Scripted::Kill]);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| step(&runner)));
    assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD"]), later);
    assert!(rig.worktree_7().join("by-hand.txt").exists());
}

#[test]
fn a_local_branch_left_behind_that_matches_origin_is_reused() {
    let rig = Rig::new("webapp");
    let by_hand = reviewed_71(&rig);
    git(&rig.repo(), &["fetch", "--quiet", "origin", "kelpie/7"]);
    git(&rig.repo(), &["branch", "kelpie/7", &by_hand]);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));

    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 1);
    assert!(rig.worktree_7().join("by-hand.txt").exists());
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        git(&rig.worktree_7(), &["rev-parse", &format!("{fixed}~1")]),
        by_hand
    );
}

#[test]
fn a_local_branch_with_commits_origin_lacks_stays_a_refusal() {
    let rig = Rig::new("webapp");
    let by_hand = reviewed_71(&rig);
    git(&rig.repo(), &["fetch", "--quiet", "origin", "kelpie/7"]);
    let tree = format!("{by_hand}^{{tree}}");
    let ahead = git(
        &rig.repo(),
        &[
            "-c",
            "user.name=kelpie",
            "-c",
            "user.email=kelpie@example.invalid",
            "commit-tree",
            &tree,
            "-p",
            &by_hand,
            "-m",
            "not pushed",
        ],
    );
    git(&rig.repo(), &["branch", "kelpie/7", &ahead]);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));

    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("the rework took a branch holding work origin lacks");
    };
    assert!(
        question.contains("branch kelpie/7 already exists without its worktree"),
        "{question}"
    );
    assert_eq!(rig.claude.calls(), []);
    assert_eq!(git(&rig.repo(), &["rev-parse", "kelpie/7"]), ahead);
}

#[test]
fn a_pull_request_that_is_not_kelpies_open_one_is_refused_and_nothing_starts() {
    let rig = Rig::new("golbat");
    for (number, branch) in [(80, "feat/timeline"), (81, "kelpie/8"), (82, "kelpie/9")] {
        rig.push_by_hand(branch, "work.txt");
        rig.forge.open_pull_request(number, branch, &[]);
        rig.forge.review(number, review());
    }
    rig.forge.open_pull_request(83, "kelpie/x", &[]);
    rig.push_by_hand("kelpie/10", "theirs.txt");
    rig.forge.open_pull_request(84, "kelpie/10", &[10]);
    rig.forge.review(84, review());
    rig.forge.set_author(84, "a-collaborator");
    rig.forge.set_from_fork(81);
    rig.forge.set_state(82, PullRequestState::Merged);
    let runner = running(&rig);
    for (number, error) in [
        ("80", "pull request #80 is not one kelpie opened"),
        ("81", "pull request #81 is not one kelpie opened"),
        ("83", "pull request #83 is not one kelpie opened"),
        ("84", "pull request #84 is not one kelpie opened"),
        ("82", "pull request #82 is merged"),
        (
            "90",
            "cannot read pull request #90: gh failed: no pull request #90",
        ),
    ] {
        assert_eq!(
            rig.ask(&runner, "rework", Some(number)),
            json!({ "error": error })
        );
    }
    rig.forge.set_state(80, PullRequestState::Closed);
    assert_eq!(
        rig.ask(&runner, "rework", Some("80")),
        json!({ "error": "pull request #80 is closed" })
    );
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert_eq!(rig.claude.calls(), []);
}

#[test]
fn a_review_with_no_body_and_no_unresolved_comment_is_nothing_to_rework() {
    let rig = Rig::new("chelone");
    reviewed_71(&rig);
    rig.forge.review(
        71,
        MaintainerReview {
            body: " \n".into(),
            comments: vec![],
            ..review()
        },
    );
    rig.push_by_hand("kelpie/8", "work.txt");
    rig.forge.open_pull_request(72, "kelpie/8", &[8]);
    let runner = running(&rig);
    for number in [71, 72] {
        assert_eq!(
            rig.ask(&runner, "rework", Some(&number.to_string())),
            json!({ "error": format!(
                "nothing to rework: the latest review of #{number} has no body \
                 and no unresolved comment"
            ) })
        );
    }
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert!(!rig.build_7().exists());
}

#[test]
fn a_rework_while_a_work_item_is_in_flight_is_refused() {
    let rig = Rig::new("rotom");
    reviewed_71(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "add", Some("5"));
    assert_eq!(
        rig.ask(&runner, "rework", Some("71")),
        json!({ "error": "the work item for #5 is in flight" })
    );
    rig.ask(&runner, "drop", None);
    rig.ask(&runner, "rework", Some("71"));
    assert_eq!(
        rig.ask(&runner, "rework", Some("71")),
        json!({ "error": "the work item for #7 is in flight" })
    );
}

#[test]
fn the_worker_reads_only_its_own_pull_requests_review() {
    let rig = Rig::new("zeus");
    reviewed_71(&rig);
    rig.push_by_hand("kelpie/8", "work.txt");
    rig.forge.open_pull_request(72, "kelpie/8", &[8]);
    rig.forge.review(
        72,
        MaintainerReview {
            body: "Another pull request's review.".into(),
            comments: vec![],
            ..review()
        },
    );
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.claude
        .script([Scripted::Reply(Default::default(), Default::default())]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    assert!(!seen.call.prompt.contains("Another"));
    let build = std::fs::read_dir(rig.build_7()).unwrap();
    for file in build.map(|entry| entry.unwrap().path()) {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(!text.contains("Another"), "{file:?}");
    }
}

#[test]
fn a_dropped_rework_leaves_its_issue_off_the_board() {
    let rig = Rig::new("xilriws");
    reviewed_71(&rig);
    rig.forge.list_ready(7, false);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.claude
        .script([Scripted::Reply(Default::default(), Default::default())]);
    step(&runner).unwrap();
    rig.ask(&runner, "drop", None);
    rig.forge.set_state(71, PullRequestState::Closed);
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(
        status["skipped"],
        json!([{ "reason": "finished", "issue": 7 }])
    );
    assert!(
        rig.forge.head_of("kelpie/7").is_some(),
        "its branch on the forge stays"
    );
}

#[test]
fn rework_takes_one_plain_pull_request_number() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "rework", None),
        json!({ "error": "`rework` takes a pull request number" })
    );
    for bad in ["#71", "0", "71 72", "pr71"] {
        assert_eq!(
            rig.ask(&runner, "rework", Some(bad)),
            json!({ "error": format!("{bad:?} is not a pull request number") })
        );
    }
}

#[test]
fn a_rework_of_a_pull_request_coderabbit_reviewed_spends_no_second_round() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.edit_settings(|s| s.replace("divisor = 1000\n", "divisor = 1000\nrounds = 1\n"));
    let reviewed = reviewed_71(&rig);
    rig.push_by_hand("kelpie/7", "later.txt");
    rig.forge.coderabbit.review(
        71,
        &reviewed,
        crate::runner::coderabbit::tests::now(&rig),
        &[],
    );
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    rig.claude.script([
        Scripted::Push("fix.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the rework's turn
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);

    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], json!("merge"));
    assert_eq!(status["work_item"]["coderabbit"]["rounds"], json!(1));
    let summons = rig.forge.coderabbit.label_log().into_iter();
    let summon = crate::runner::coderabbit::LABEL;
    assert_eq!(summons.filter(|(_, l, _)| l == summon).count(), 0);
}

#[test]
fn a_review_of_the_current_head_is_left_out_of_a_reworks_count_as_an_adoptions_is() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.edit_settings(|s| s.replace("divisor = 1000\n", "divisor = 1000\nrounds = 2\n"));
    let head = reviewed_71(&rig);
    rig.forge
        .coderabbit
        .review(71, &head, crate::runner::coderabbit::tests::now(&rig), &[]);
    let runner = running(&rig);
    rig.ask(&runner, "rework", Some("71"));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["coderabbit"]["rounds"], json!(0));
}

#[test]
fn a_rework_is_refused_while_the_forge_cannot_show_coderabbits_reviews() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.edit_settings(|s| s.replace("divisor = 1000\n", "divisor = 1000\nrounds = 2\n"));
    reviewed_71(&rig);
    let runner = running(&rig);
    rig.forge.coderabbit.set_down(true);
    let answer = rig.ask(&runner, "rework", Some("71"));
    let error = answer["error"].as_str().unwrap();
    assert!(
        error.starts_with("cannot read CodeRabbit's reviews of #71"),
        "{error}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    rig.forge.coderabbit.set_down(false);
    assert!(
        rig.ask(&runner, "rework", Some("71"))
            .get("error")
            .is_none()
    );
}

mod asked;
