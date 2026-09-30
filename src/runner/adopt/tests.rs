use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::coderabbit::LABEL;
use crate::ports::{Checks, PullRequestState, Session};
use crate::runner::{StepReport, step};
use crate::settings::Effort;
use crate::test::{Hold, Rig, Scripted, git};

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

fn waiting(numbers: &[u64], by_label: bool) -> serde_json::Value {
    let waiting: Vec<_> = numbers
        .iter()
        .map(|n| json!({ "pull_request": n, "by_label": by_label }))
        .collect();
    json!(waiting)
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
        (&waiting(&[80], false), &json!(null))
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
        "/mattpocock:diagnosing-bugs Your work item adopts pull request #80 for issue #5: \
         Title of #5\n\n\
         You did not write its code. {} holds the pull request's title and body, \
         its issue, and the review to fix, if it has one. Your branch is the pull \
         request's as `origin` holds it. The pull request is already open, so do \
         not open another.\n\n",
        file_5(&rig).display(),
    );
    let prompt = &seen.call.prompt;
    assert!(prompt.starts_with(&named), "{prompt}");
    let red = format!(
        "\nCI failed on your pull request #80 at {}: test. ",
        &head[..7]
    );
    assert!(prompt.contains(&red), "{prompt}");
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
    let cases: [Refusal; 6] = [
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
            "base",
            |rig| rig.forge.set_base(80, "feat/stacked"),
            "pull request #80 merges into `feat/stacked`, not `main`",
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
    assert_eq!(status["adopted"], waiting(&[81, 80], false));
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
    assert_eq!(
        rig.ask(&runner, "status", None)["adopted"],
        waiting(&[80], false)
    );
}

// Pull request 80 adopted and at CI, whose red run went to the worker as its
// next turn. Returns the red head.
fn red_80(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let head = opened_80(rig);
    rig.ask(runner, "adopt", Some("80"));
    step(runner).unwrap();
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::CiFailed { .. })
    ));
    head
}

fn foreign_ruling(rig: &Rig, runner: &Mutex<Runner>, pushed: &str) {
    rig.forge.set_checks(pushed, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(runner) else {
        panic!("the push was not ruled on");
    };
    let moved = format!("its head moved to {}", &pushed[..7]);
    assert!(question.contains(&moved), "{question}");
}

#[test]
fn a_push_by_the_branchs_owner_during_the_workers_turn_is_not_the_workers() {
    let rig = Rig::new("koji");
    let runner = running(&rig);
    red_80(&rig, &runner);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let pushed = std::thread::scope(|s| {
        let turn = s.spawn(|| step(&runner).unwrap());
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the turn never began"
        );
        let pushed = rig.push_by_hand("fix/timeline", "owner.txt");
        hold.release();
        turn.join().unwrap();
        pushed
    });
    foreign_ruling(&rig, &runner, &pushed);
}

#[test]
fn a_push_by_the_branchs_owner_before_the_workers_turn_parks_it_with_no_turn() {
    let rig = Rig::new("rotom");
    let runner = running(&rig);
    red_80(&rig, &runner);
    let pushed = rig.push_by_hand("fix/timeline", "owner.txt");
    foreign_ruling(&rig, &runner, &pushed);
    assert_eq!(rig.claude.all_calls(), []);
}

#[test]
fn one_coderabbit_review_of_the_head_it_arrived_with_is_round_one_not_the_cap() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    let head = opened_80(&rig);
    rig.forge.ready_pull_request(80);
    rig.forge
        .coderabbit
        .review(80, &head, Rig::EPOCH - 60, &["Check the bounds"]);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["coderabbit"]["rounds"],
        0
    );

    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CodeRabbitReviewed { round: 1, .. })
    ));
    rig.claude.script([Scripted::Text(
        r#"{"holds": true, "severity": "medium", "reason": "real"}"#,
    )]);
    step(&runner).unwrap(); // the judge
    step(&runner).unwrap(); // the finding goes to the worker
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]), "no cap ruling");
    assert_eq!(status["work_item"]["coderabbit"]["rounds"], 1);
}

#[test]
fn a_start_retried_after_a_failure_begins_at_the_head_origin_holds_now() {
    let rig = Rig::new("golbat");
    rig.coderabbit_on();
    opened_80(&rig);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    rig.forge.coderabbit.set_down(true);
    step(&runner).unwrap();
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));

    let moved = rig.push_by_hand("fix/timeline", "later.txt");
    rig.forge.coderabbit.set_down(false);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            issue: 5,
            pull_request: 80,
            worker: WorkerModel {
                model: "claude-sonnet-5".into(),
                effort: Effort::Medium,
            },
        })
    );
    let worktree = rig.paths().worktree(5);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), moved);
    rig.forge.set_checks(&moved, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
}

#[test]
fn a_ready_pull_request_reaches_the_merge_ruling_without_parking() {
    let rig = Rig::new("webapp");
    let head = opened_80(&rig);
    rig.forge.ready_pull_request(80);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no ruling was raised");
    };
    assert!(question.starts_with("Merge pull request #80"), "{question}");
}

// Issue 7 in flight, with pull requests 80 and 81 waiting behind it
fn two_waiting(rig: &Rig) -> Mutex<Runner> {
    opened_80(rig);
    rig.push_by_hand("fix/other", "other.txt");
    rig.forge.open_pull_request(81, "fix/other", &[6]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    runner
}

#[test]
fn a_waiting_pull_request_merged_or_closed_by_hand_leaves_without_a_comment() {
    let rig = Rig::new("chelone");
    let runner = two_waiting(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    rig.ask(&runner, "adopt", Some("81"));
    rig.forge.set_state(80, PullRequestState::Merged);
    rig.forge.set_state(81, PullRequestState::Closed);
    rig.ask(&runner, "drop", None);
    rig.ask(&runner, "start", None);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.comments(), []);
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));
}

#[test]
fn taking_the_label_off_a_waiting_pull_request_takes_it_back() {
    let rig = Rig::new("xilriws");
    opened_80(&rig);
    rig.push_by_hand("fix/other", "other.txt");
    rig.forge.open_pull_request(81, "fix/other", &[6]);
    rig.forge.label_pull_request(80, READY);
    rig.forge.label_pull_request(81, READY);
    let runner = running(&rig);
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["pull_request"], 80);
    assert_eq!(status["adopted"], waiting(&[81], true));

    rig.forge.unlabel_pull_request(81, READY);
    rig.ask(&runner, "drop", None);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["adopted"], json!([]));
    assert_eq!(rig.forge.comments(), []);
}

// Pull request 80 adopted under `auto` with CodeRabbit on, ready, its head
// already reviewed by CodeRabbit before the adoption. Returns its head.
fn adopted_reviewed_under_auto(rig: &Rig, titles: &[&str]) -> (Mutex<Runner>, String) {
    rig.coderabbit_on();
    rig.merge_auto();
    let head = opened_80(rig);
    rig.forge.ready_pull_request(80);
    rig.forge
        .coderabbit
        .review(80, &head, Rig::EPOCH - 60, titles);
    let runner = running(rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    (runner, head)
}

#[test]
fn a_clean_review_from_before_the_adoption_never_satisfies_the_round() {
    let rig = Rig::new("shep");
    let (runner, head) = adopted_reviewed_under_auto(&rig, &[]);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            issue: 5,
            pull_request: 80,
            head: head.clone(),
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["coderabbit"]["satisfied"], false);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None, "the old review is no answer");
    assert_eq!(rig.forge.merges(), []);

    let at = crate::ports::Clock::now(&rig.clock).0;
    rig.forge.coderabbit.review(80, &head, at + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Finished { merged: true, .. })
    ));
    assert_eq!(rig.forge.merges(), [(80, head)]);
}

#[test]
fn findings_from_before_the_adoption_the_judge_rejects_still_leave_a_summon_owed() {
    let rig = Rig::new("rotom");
    let (runner, head) = adopted_reviewed_under_auto(&rig, &["Not real."]);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CodeRabbitReviewed { round: 1, .. })
    ));
    rig.claude.script([Scripted::Text(
        r#"{"holds": false, "severity": "low", "reason": "not so"}"#,
    )]);
    step(&runner).unwrap(); // the judge
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 5,
            pull_request: 80,
            head,
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["coderabbit"]["satisfied"], false);
    assert_eq!(status["work_item"]["coderabbit"]["rounds"], 1);
    assert_eq!(rig.forge.merges(), []);
}
