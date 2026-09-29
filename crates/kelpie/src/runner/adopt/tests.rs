use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::{Checks, PullRequestState, Session};
use crate::runner::{StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, git};

// Pull request 80 on `fix/timeline`, opened by kelpie's account from another
// session: two commits pushed by hand, closing issue 5. Returns its head.
fn opened_80(rig: &Rig) -> String {
    rig.push_by_hand("fix/timeline", "work.txt");
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    head
}

fn running(rig: &Rig) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    runner
}

fn adopted_80() -> StepReport {
    StepReport::Adopted {
        issue: 5,
        pull_request: 80,
        worker: WorkerModel {
            model: "claude-sonnet-5".into(),
            effort: Effort::Medium,
        },
    }
}

fn file_5(rig: &Rig) -> PathBuf {
    rig.paths().build(5).join("adopted-pull-request.md")
}

#[test]
fn adopt_starts_at_ci_and_the_first_turn_names_the_file_ahead_of_the_red_run() {
    let rig = Rig::new("shep");
    let head = opened_80(&rig);
    let runner = running(&rig);
    let status = rig.ask(&runner, "adopt", Some("80"));
    assert_eq!(
        (&status["adopted"], &status["work_item"]),
        (&json!([80]), &json!(null))
    );

    assert_eq!(step(&runner).unwrap(), Some(adopted_80()));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], json!([]));
    let item = &status["work_item"];
    assert_eq!(
        (&item["issue"], &item["branch"], &item["pull_request"]),
        (&json!(5), &json!("fix/timeline"), &json!(80))
    );
    assert_eq!(item["adopted"], true);
    assert_eq!(item["phase"]["state"], "ci");
    assert_eq!(rig.claude.all_calls(), [], "nothing for the worker yet");
    assert_eq!(
        std::fs::read_to_string(file_5(&rig)).unwrap(),
        "# Pull request #80: Title of pull request #80\n\n\
         Body of pull request #80.\n\
         \n# Issue #5: Title of #5\n\nBody of #5.\n"
    );

    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let [seen] = rig.claude.seen().try_into().unwrap();
    assert!(matches!(seen.call.session, Session::New(_)));
    assert_eq!(seen.call.cwd, rig.paths().worktree(5));
    let named = format!(
        "Your work item adopts pull request #80 for issue #5: Title of #5\n\n\
         You did not write its code. {} holds the pull request's title and body, \
         its issue, and the review to fix, if it has one. Your branch is the pull \
         request's as `origin` holds it. The pull request is already open, so do \
         not open another.\n\n\
         CI failed on your pull request #80 at {}: test. ",
        file_5(&rig).display(),
        &head[..7]
    );
    assert!(seen.call.prompt.starts_with(&named), "{}", seen.call.prompt);
    rig.assert_worker_reads(&seen, &file_5(&rig));

    let fixed = rig.forge.head_of("fix/timeline").unwrap();
    let worktree = rig.paths().worktree(5);
    assert_eq!(
        git(&worktree, &["rev-parse", &format!("{fixed}~1")]),
        head,
        "the fix lands on the adopted head, pushed without force"
    );
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(rig.reviewer.seen(), [], "no qwen round, before or after");
}

#[test]
fn ready_for_agent_on_a_branch_not_kelpies_adopts_it_on_the_next_poll() {
    let rig = Rig::new("koji");
    opened_80(&rig);
    rig.push_by_hand("kelpie/7", "seven.txt");
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.forge.label_pull_request(71, READY);
    rig.forge.label_pull_request(80, READY);
    rig.forge.label_pull_request(80, HUMAN);
    rig.forge.label_pull_request(80, "bug");
    let runner = running(&rig);

    assert_eq!(step(&runner).unwrap(), Some(adopted_80()));
    assert_eq!(rig.forge.pull_request_labels(80), ["bug"]);
    assert_eq!(
        rig.forge.pull_request_labels(71),
        [READY],
        "kelpie's own branch asks for a rework, not an adoption"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["pull_request"], 80,
        "an adopted pull request goes before a rework"
    );
}

// A project, what makes pull request 80 refused there, and the refusal
type Refusal = (&'static str, fn(&Rig), &'static str);

#[test]
fn each_refusal_comments_and_adopts_nothing() {
    let cases: [Refusal; 5] = [
        (
            "closed",
            |rig| rig.forge.set_state(80, PullRequestState::Closed),
            "pull request #80 is closed",
        ),
        (
            "merged",
            |rig| rig.forge.set_state(80, PullRequestState::Merged),
            "pull request #80 is merged",
        ),
        (
            "fork",
            |rig| rig.forge.set_from_fork(80),
            "pull request #80 comes from a fork",
        ),
        (
            "author",
            |rig| rig.forge.set_author(80, "someone-else"),
            "pull request #80 was opened by someone-else, not by kelpie's account",
        ),
        (
            "issue",
            |rig| {
                rig.forge.open_pull_request(80, "fix/timeline", &[]);
            },
            "pull request #80 names no issue it closes",
        ),
    ];
    for (project, refuse, reason) in cases {
        let rig = Rig::new(project);
        opened_80(&rig);
        refuse(&rig);
        let runner = running(&rig);
        assert_eq!(
            rig.ask(&runner, "adopt", Some("80")),
            json!({ "error": reason }),
            "{project}"
        );
        assert_eq!(
            rig.forge.comments(),
            [(
                80,
                format!("Kelpie cannot adopt this pull request: {reason}.")
            )],
            "{project}"
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            (&status["adopted"], &status["work_item"]),
            (&json!([]), &json!(null)),
            "{project}"
        );
        assert_eq!(step(&runner).unwrap(), None, "{project}");
    }
}

#[test]
fn a_labelled_pull_request_refused_loses_the_label_and_gets_the_comment_once() {
    let rig = Rig::new("rotom");
    opened_80(&rig);
    rig.forge.set_author(80, "someone-else");
    rig.forge.label_pull_request(80, READY);
    let runner = running(&rig);
    let reason = "pull request #80 was opened by someone-else, not by kelpie's account";
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::AdoptRefused {
            pull_request: 80,
            reason: reason.into(),
            comment_failed: None,
        })
    );
    assert_eq!(rig.forge.pull_request_labels(80), Vec::<String>::new());
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.comments().len(), 1);
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));
}

#[test]
fn an_adopted_pull_request_skips_the_qwen_loop_and_reaches_coderabbit() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.push_by_hand("fix/timeline", "work.txt");
    let reviewed = rig.forge.head_of("fix/timeline").unwrap();
    rig.forge
        .coderabbit
        .review(80, &reviewed, Rig::EPOCH - 60, &[]);
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.label_pull_request(80, LABEL);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["coderabbit"]["rounds"], 1,
        "the cap counts the review already there"
    );
    assert_eq!(
        rig.forge.pull_request_labels(80),
        Vec::<String>::new(),
        "no push summons outside the lease"
    );

    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady {
            issue: 5,
            pull_request: 80,
        })
    );
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            issue: 5,
            pull_request: 80,
            head,
        })
    );
    assert_eq!(rig.reviewer.seen(), []);
    assert_eq!(rig.claude.all_calls(), []);
}

#[test]
fn a_review_asking_for_changes_is_the_first_turn_and_the_loop_reviews_only_the_fix() {
    let rig = Rig::new("reactmap");
    let head = opened_80(&rig);
    rig.forge.review(
        80,
        MaintainerReview {
            id: "PRR_80".into(),
            changes_requested: true,
            body: "Keep the dates.".into(),
            comments: vec![],
        },
    );
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    assert!(
        std::fs::read_to_string(file_5(&rig))
            .unwrap()
            .ends_with("# The latest review of pull request #80\n\nKeep the dates.\n")
    );

    rig.claude.script([
        Scripted::Push("fix.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    let [first] = rig.claude.calls().try_into().unwrap();
    assert!(matches!(first.session, Session::New(_)));
    assert!(
        first
            .prompt
            .ends_with("Make the changes the review asks for, then commit and push with `git push origin HEAD`.\n"),
        "{}",
        first.prompt
    );
    step(&runner).unwrap(); // review round 1, qwen
    step(&runner).unwrap(); // review round 2, claude
    let [round] = rig.reviewer.seen().try_into().unwrap();
    assert_eq!(
        round.base, head,
        "the commits it arrived with are not reviewed"
    );
    let [_, claude] = rig.claude.all_calls().try_into().unwrap();
    assert!(
        claude
            .prompt
            .contains(&format!("--- diff against {head} ---"))
    );
    assert!(claude.prompt.contains("fix.txt") && !claude.prompt.contains("more.txt"));
}

#[test]
fn a_push_by_anyone_else_after_adoption_parks_it() {
    let rig = Rig::new("golbat");
    opened_80(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();

    let pushed = rig.push_by_hand("fix/timeline", "late.txt");
    rig.forge.set_checks(&pushed, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("the push was not ruled on");
    };
    assert!(
        question.contains(&format!(
            "its head moved to {}, a commit the worker did not push",
            &pushed[..7]
        )),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "foreign-change");
}

#[test]
fn a_branch_behind_main_is_caught_up_by_a_merge_never_a_rewrite() {
    let rig = Rig::new("chelone");
    let head = opened_80(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    let landed = rig.land_on_origin("landed.txt");
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Rebased { head: merged, .. }) = step(&runner).unwrap() else {
        panic!("the branch was not caught up");
    };
    let worktree = rig.paths().worktree(5);
    assert_eq!(
        git(
            &worktree,
            &["rev-parse", &format!("{merged}^1"), &format!("{merged}^2")]
        ),
        format!("{head}\n{landed}")
    );
}

#[test]
fn adopted_pull_requests_wait_for_the_work_item_in_flight_across_a_restart() {
    let rig = Rig::new("xilriws");
    opened_80(&rig);
    rig.push_by_hand("fix/other", "other.txt");
    rig.forge.open_pull_request(81, "fix/other", &[6]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "adopt", Some("81"));
    rig.ask(&runner, "adopt", Some("80"));
    rig.ask(&runner, "adopt", Some("81"));
    drop(runner);

    let runner = rig.open().unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], json!([81, 80]));
    assert_eq!(status["work_item"]["issue"], 7);
    rig.ask(&runner, "drop", None);
    rig.ask(&runner, "start", None);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            issue: 6,
            pull_request: 81,
            worker: WorkerModel {
                model: "claude-sonnet-5".into(),
                effort: Effort::Medium,
            },
        })
    );
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([80]));
}
