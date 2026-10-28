use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::coderabbit::LABEL;
use crate::ports::{CLEAN, Checks, PullRequestState, Session};
use crate::runner::{StepReport, step};
use crate::test::{Hold, Rig, Scripted, git, ticket};

// Pull request 80 on `fix/timeline`, opened by kelpie's account from another
// session: two commits pushed by hand, closing issue 5. Returns its head.
fn opened_80(rig: &Rig) -> String {
    rig.push_by_hand("fix/timeline", "work.txt");
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    head
}

fn running(rig: &Rig) -> Mutex<Runner> {
    rig.open().unwrap()
}

// Pull request 80 adopted on `sonnet-high`, the rig's board's first pick
fn adopted_80() -> StepReport {
    adopted_80_on("sonnet-high")
}

fn adopted_80_on(agent: &str) -> StepReport {
    StepReport::Adopted {
        issue: ticket(5),
        pull_request: 80,
        agent: agent.to_owned().try_into().unwrap(),
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
    rig.paths().build(ticket(5)).join("adopted-pull-request.md")
}

#[test]
fn adopt_starts_at_ci_and_the_first_turn_names_the_file_ahead_of_the_red_run() {
    let rig = Rig::new("shep");
    rig.ci_on();
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
        (&json!("5"), &json!("fix/timeline"), &json!(80))
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
    assert_eq!(seen.call.cwd, rig.paths().worktree(ticket(5)));
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
    let worktree = rig.paths().worktree(ticket(5));
    assert_eq!(
        git(&worktree, &["rev-parse", &format!("{fixed}~1")]),
        head,
        "the fix lands on the adopted head, pushed without force"
    );
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(80)));
    assert_eq!(rig.reviewer.seen(), [], "no qwen round, before or after");
}

#[test]
fn an_adopted_pull_request_opens_on_the_agent_boards_pick() {
    let rig = Rig::new("shep");
    opened_80(&rig);
    rig.agent_board.ranks(&["opus-high", "sonnet-high"]);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));

    assert_eq!(step(&runner).unwrap(), Some(adopted_80_on("opus-high")));
    let [ask] = rig.agent_board.asks().try_into().unwrap();
    assert_eq!(ask.pin, None);
}

#[test]
fn a_waiting_pull_request_pinned_to_an_implementer_is_held_by_its_account() {
    let rig = Rig::new("shep");
    rig.ci_on();
    opened_80(&rig);
    rig.write_codex_implementer();
    rig.forge.label(5, "agent:codex!");
    rig.codex_day_began(0);
    // 15% of Codex's week since the day began, past its allowance of 100 / 7.
    rig.codex_meter.set(Rig::utilization(15, 0));
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));

    let Some(StepReport::Held { kind, reason, .. }) = step(&runner).unwrap() else {
        panic!("expected a hold");
    };
    assert_eq!(kind, crate::pacer::HoldKind::Allowance);
    assert!(reason.starts_with("today's Codex allowance"), "{reason}");
    assert_eq!(rig.codex_meter.reads(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], waiting(&[80], false), "still queued");
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(status["rulings"], json!([]), "a hold is no `adopt-failed`");
    assert_eq!(status["skipped"], json!([]));
}

#[test]
fn a_pull_request_held_by_its_labelled_account_leaves_the_ones_behind_it_to_start() {
    let rig = Rig::new("shep");
    rig.ci_on();
    opened_80(&rig);
    rig.push_by_hand("fix/other", "other.txt");
    rig.forge.open_pull_request(81, "fix/other", &[6]);
    rig.write_codex_implementer();
    rig.forge.label(5, "agent:codex!");
    rig.agent_board.ranks(&["sonnet-high", "codex"]);
    rig.codex_day_began(0);
    rig.codex_meter.set(Rig::utilization(15, 0));
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    rig.ask(&runner, "adopt", Some("81"));

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            pull_request: 81,
            ..
        })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], waiting(&[80], false), "80 stays queued");
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

// Where an App covers the repo the refusal is the App's, not the maintainer's.
#[test]
fn a_refusal_is_posted_through_the_app_where_one_covers_the_repo() {
    let rig = Rig::new("shep");
    rig.with_app();
    opened_80(&rig);
    rig.forge.set_from_fork(80);
    let runner = running(&rig);

    rig.ask(&runner, "adopt", Some("80"));

    assert_eq!(rig.forge.comments(), []);
    let posted = (rig.github.writes().into_iter())
        .find(|(_, call)| call.path == "/repos/shep-pm/shep/issues/80/comments")
        .expect("the refusal went through the App");
    let body = posted.1.body.unwrap_or_default();
    assert!(
        body.contains("Kelpie cannot adopt this pull request"),
        "{body}"
    );
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
fn an_adopted_pull_request_skips_the_other_reviewers_and_reaches_coderabbit() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.push_by_hand("fix/timeline", "work.txt");
    let reviewed = rig.forge.head_of("fix/timeline").unwrap();
    rig.forge
        .coderabbit
        .review(80, &reviewed, Rig::EPOCH - 3600, &[]);
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.label_pull_request(80, LABEL);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["bot_reads"],
        json!({ "coderabbit": 1 }),
        "its reads count the review already there"
    );
    assert_eq!(
        rig.forge.pull_request_labels(80),
        Vec::<String>::new(),
        "no push summons outside the lease"
    );

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady {
            issue: ticket(5),
            pull_request: 80,
        })
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: ticket(5),
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

    rig.claude
        .script([Scripted::Push("fix.txt", "fixed\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn
    let [first] = rig.claude.calls().try_into().unwrap();
    assert!(matches!(first.session, Session::New(_)));
    assert!(
        first
            .prompt
            .ends_with("Make the changes the review asks for, then commit and push with `git push origin HEAD`. Update the pull request's body so it matches the head you push: what changed, the files touched, the test evidence, and the decisions you made.\n"),
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
fn an_adopted_turn_that_pushes_nothing_is_sent_back_once_then_parks_on_a_ruling() {
    let rig = Rig::new("reactmap");
    opened_80(&rig);
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
    rig.claude.script([
        Scripted::Reply(Default::default(), Default::default()),
        Scripted::Reply(Default::default(), Default::default()),
    ]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [first, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.session, Session::Resume(first.session.id().clone()));
    assert!(
        again
            .prompt
            .starts_with("Your last turn ended without a new push"),
        "{}",
        again.prompt
    );
    let Some(StepReport::Failed {
        issue,
        pull_request: Some(80),
        question,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the adopted work item was not parked");
    };
    assert_eq!(issue, ticket(5));
    assert!(
        question.contains("it ended twice with no new push and no question"),
        "{question}"
    );
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
    let worktree = rig.paths().worktree(ticket(5));
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
    assert_eq!(status["work_item"]["issue"], "7");
    rig.ask(&runner, "drop", None);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            issue: ticket(6),
            pull_request: 81,
            agent: "sonnet-high".to_owned().try_into().unwrap(),
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
    rig.ci_on();
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
    rig.ci_on();
    let runner = running(&rig);
    red_80(&rig, &runner);
    let pushed = rig.push_by_hand("fix/timeline", "owner.txt");
    foreign_ruling(&rig, &runner, &pushed);
    assert_eq!(rig.claude.all_calls(), []);
}

#[test]
fn a_coderabbit_review_of_the_head_it_arrived_with_counts_once_its_own_read_lands() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    let head = opened_80(&rig);
    rig.forge.ready_pull_request(80);
    rig.forge
        .coderabbit
        .review(80, &head, Rig::EPOCH - 3600, &["Check the bounds"]);
    let runner = running(&rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["bot_reads"],
        json!(null)
    );

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    let at = crate::ports::Clock::now(&rig.clock).0;
    rig.forge.coderabbit.review(80, &head, at + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed {
            round: 1,
            open_threads: 1,
            ..
        })
    ));
    step(&runner).unwrap(); // the finding goes to the worker
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_eq!(status["work_item"]["bot_reads"], json!({ "coderabbit": 1 }));
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
    rig.next_look();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Adopted {
            issue: ticket(5),
            pull_request: 80,
            agent: "sonnet-high".to_owned().try_into().unwrap(),
        })
    );
    let worktree = rig.paths().worktree(ticket(5));
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), moved);
    assert!(matches!(
        step(&runner).unwrap(),
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

// Pull request 80 adopted under `auto` by a project listing CodeRabbit,
// ready and green, its head already reviewed by CodeRabbit before the
// adoption. Returns its head.
fn adopted_reviewed_under_auto(rig: &Rig, titles: &[&str]) -> (Mutex<Runner>, String) {
    rig.coderabbit_on();
    rig.merge_auto();
    let head = opened_80(rig);
    rig.forge.ready_pull_request(80);
    rig.forge
        .coderabbit
        .review(80, &head, Rig::EPOCH - 3600, titles);
    let runner = running(rig);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    (runner, head)
}

#[test]
fn a_clean_review_from_before_the_adoption_never_stands_for_the_bots_read() {
    let rig = Rig::new("shep");
    let (runner, head) = adopted_reviewed_under_auto(&rig, &[]);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            issue: ticket(5),
            pull_request: 80,
            head: head.clone(),
        })
    );
    assert_eq!(rig.forge.comments().len(), 1, "a full review, by comment");
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None, "the old review is no answer");
    assert_eq!(rig.forge.merges(), []);

    let at = crate::ports::Clock::now(&rig.clock).0;
    rig.forge.coderabbit.review(80, &head, at + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed {
            open_threads: 0,
            ..
        })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Finished { merged: true, .. })
    ));
    assert_eq!(rig.forge.merges(), [(80, head)]);
}

#[test]
fn threads_from_before_the_adoption_go_to_the_worker_once_kelpies_own_read_lands() {
    let rig = Rig::new("rotom");
    let (runner, head) = adopted_reviewed_under_auto(&rig, &["Check the bounds"]);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    let at = crate::ports::Clock::now(&rig.clock).0;
    rig.forge.coderabbit.review(80, &head, at + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed {
            round: 1,
            open_threads: 1,
            ..
        })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    rig.claude.script([Scripted::PushSay(
        "bounds.txt",
        "checked\n",
        "thread 1: fixed",
    )]);
    step(&runner).unwrap(); // the fix turn
    let fixed = rig.forge.head_of("fix/timeline").unwrap();
    assert_ne!(fixed, head);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_80_0"]);
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Finished { merged: true, .. })
    ));
    assert_eq!(rig.forge.merges(), [(80, fixed)]);
}

// Checks `fix/timeline` out in a worktree of the maintainer's own, named
// `name` beside the rig's other folders, and returns where it is.
fn hold_timeline(rig: &Rig, name: &str) -> PathBuf {
    let mine = rig.home.path().join(name);
    git(&rig.repo(), &["fetch", "--quiet", "origin", "fix/timeline"]);
    let args = ["worktree", "add", "--quiet", "-B", "fix/timeline"];
    let args = args.iter().map(|a| (*a).to_owned());
    let place = [mine.display().to_string(), "origin/fix/timeline".to_owned()];
    let args: Vec<String> = args.chain(place).collect();
    git(
        &rig.repo(),
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    mine
}

#[test]
fn adopting_a_branch_checked_out_in_another_worktree_is_refused_and_names_it() {
    let rig = Rig::new("shep");
    opened_80(&rig);
    hold_timeline(&rig, "mine");
    let runner = running(&rig);

    let reply = rig.ask(&runner, "adopt", Some("80"));

    let error = reply["error"].as_str().expect("a refusal").to_owned();
    assert!(
        error.contains("`fix/timeline` is checked out in "),
        "{error}"
    );
    assert!(error.contains("/mine"), "{error}");
    assert!(
        error.contains("remove it or switch it off the branch"),
        "{error}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], json!([]), "nothing is queued to fail");
    assert_eq!(rig.forge.comments(), [], "the pull request is not told");
}

#[test]
fn an_adopt_failing_on_every_pass_raises_one_ruling_until_its_error_changes() {
    let rig = Rig::new("shep");
    opened_80(&rig);
    rig.forge.label_pull_request(80, READY);
    let first = hold_timeline(&rig, "mine");
    let runner = running(&rig);

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::BoardFailed { .. })
    ));
    let status = rig.ask(&runner, "status", None);
    let [ruling] = status["rulings"].as_array().unwrap().as_slice() else {
        panic!("one ruling: {status}");
    };
    assert_eq!(ruling["id"], "80");
    assert_eq!(ruling["pull_request"], 80);
    assert_eq!(ruling["kind"]["kind"], "stuck");
    assert_eq!(ruling["kind"]["reason"], "adopt-failed");
    let question = ruling["question"].as_str().unwrap();
    assert!(question.contains("/mine"), "{question}");
    assert!(
        question.contains("remove it or switch it off the branch"),
        "{question}"
    );
    assert_eq!(status["work_items"], json!([]));

    rig.clock.advance(60);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let again = rig.ask(&runner, "status", None);
    let [kept] = again["rulings"].as_array().unwrap().as_slice() else {
        panic!("one ruling: {again}");
    };
    let asked = |ruling: &serde_json::Value| (ruling["question"].clone(), ruling["raised"].clone());
    assert_eq!(
        asked(kept),
        asked(&status["rulings"][0]),
        "the same error asks nothing again"
    );

    rig.ask(&runner, "rule", Some("80 yes"));
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"],
        json!([]),
        "an answered ruling is not raised again for the same error"
    );

    git(
        &rig.repo(),
        &[
            "worktree",
            "remove",
            "--force",
            &first.display().to_string(),
        ],
    );
    hold_timeline(&rig, "theirs");
    rig.clock.advance(60);
    step(&runner).unwrap();
    let changed = rig.ask(&runner, "status", None);
    let [ruling] = changed["rulings"].as_array().unwrap().as_slice() else {
        panic!("one ruling: {changed}");
    };
    let question = ruling["question"].as_str().unwrap();
    assert!(question.contains("/theirs"), "{question}");
    assert_ne!(ruling["raised"], status["rulings"][0]["raised"]);
}

#[test]
fn a_pass_that_adopts_after_failures_clears_the_ruling_and_the_status_line() {
    let rig = Rig::new("shep");
    opened_80(&rig);
    rig.forge.label_pull_request(80, READY);
    let mine = hold_timeline(&rig, "mine");
    let runner = running(&rig);

    step(&runner).unwrap();
    rig.clock.advance(3_600);
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    let [line] = status["skipped"].as_array().unwrap().as_slice() else {
        panic!("one status line: {status}");
    };
    assert_eq!(line["reason"], "adopt");
    assert_eq!(line["pull_request"], 80);
    assert!(line["error"].as_str().unwrap().contains("/mine"), "{line}");
    assert_eq!(
        line["since"],
        Rig::EPOCH,
        "since the first pass that failed"
    );
    assert_eq!(status["rulings"].as_array().unwrap().len(), 1);

    git(
        &rig.repo(),
        &["worktree", "remove", "--force", &mine.display().to_string()],
    );
    assert_eq!(step(&runner).unwrap(), Some(adopted_80()));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_eq!(status["skipped"], json!([]));
    assert_eq!(status["work_item"]["pull_request"], 80);
}
