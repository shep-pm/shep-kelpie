use serde_json::json;

use super::*;
use crate::ports::{Cost, Role, Session};
use crate::runner::step;
use crate::test::{Rig, Scripted, ScriptedRound};

// `drop` refuses while a call is running (see merge.rs's own test for
// that), but `end_review` tolerates a work item that is gone anyway,
// the same defensive shape as `end_turn`'s equivalent guard, rather
// than panicking the runner on the `.expect` this replaced.
#[test]
fn end_review_tolerates_a_work_item_that_is_gone() {
    let (_rig, runner, _) = Rig::with_pull_request("shep");
    runner.lock().unwrap().state.work_items.clear();
    let report = runner
        .lock()
        .unwrap()
        .end_review(Reviewed {
            result: ReviewResult::Findings(Ok(vec![])),
            spent: None,
        })
        .unwrap();
    assert_eq!(report, None);
}

fn named(name: &str) -> Option<ReviewerName> {
    Some(ReviewerName::try_from(name.to_owned()).unwrap())
}

#[test]
fn two_clean_rounds_from_different_reviewers_end_the_loop() {
    let now = crate::ports::Timestamp(100);
    let first = Review {
        reviewer: named("qwen"),
        ..Review::first()
    };
    let Phase::Review(review) = advance(first, true, now, true, &mut 0) else {
        panic!("stays reviewing after one clean round");
    };
    assert_eq!((review.round, review.consecutive_clean), (2, 1));
    assert_eq!(
        (review.reviewer, review.last.clone()),
        (None, named("qwen"))
    );
    assert_eq!(review.stage, ReviewStage::Round);

    let second = Review {
        reviewer: named("claude"),
        ..review
    };
    let phase = advance(second, true, now, false, &mut 0);
    assert_eq!(
        phase,
        Phase::Ci {
            head: None,
            since: now
        }
    );
}

#[test]
fn two_clean_rounds_from_the_same_reviewer_do_not() {
    let now = crate::ports::Timestamp(1);
    let again = Review {
        round: 2,
        consecutive_clean: 1,
        reviewer: named("qwen"),
        last: named("qwen"),
        ..Review::first()
    };
    let Phase::Review(next) = advance(again, true, now, true, &mut 0) else {
        panic!("a second clean round from qwen does not end the loop");
    };
    assert_eq!((next.round, next.consecutive_clean), (3, 2));
}

#[test]
fn a_clean_round_from_the_only_reviewer_that_could_run_ends_the_loop() {
    let now = crate::ports::Timestamp(1);
    let alone = Review {
        reviewer: named("claude"),
        alone: true,
        ..Review::first()
    };
    assert!(matches!(
        advance(alone, true, now, false, &mut 0),
        Phase::Ci { .. }
    ));
}

#[test]
fn an_older_state_file_s_clean_pair_still_ends_the_loop() {
    let now = crate::ports::Timestamp(1);
    let older = Review {
        round: 2,
        consecutive_clean: 1,
        ..Review::first()
    };
    assert!(matches!(
        advance(older, true, now, false, &mut 0),
        Phase::Ci { .. }
    ));
}

#[test]
fn a_local_round_counts_toward_the_work_item_s_local_rounds() {
    let now = crate::ports::Timestamp(1);
    let mut ran = 2;
    advance(Review::first(), false, now, true, &mut ran);
    assert_eq!(ran, 3);
    advance(Review::first(), false, now, false, &mut ran);
    assert_eq!(ran, 3);
}

#[test]
fn a_dirty_round_resets_the_streak_but_keeps_the_guard_cleared_flag() {
    let now = crate::ports::Timestamp(1);
    let review = Review {
        round: 4,
        consecutive_clean: 1,
        guard_cleared: true,
        reviewer: named("claude"),
        ..Review::first()
    };
    let Phase::Review(next) = advance(review, false, now, false, &mut 0) else {
        panic!("stays reviewing");
    };
    assert_eq!(
        next,
        Review {
            round: 5,
            consecutive_clean: 0,
            guard_cleared: true,
            last: named("claude"),
            ..Review::first()
        }
    );
}

#[test]
fn a_round_with_only_nits_is_clean_once_the_worker_fixes_them() {
    let (rig, runner) = at_round_1("shep");

    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Medium,
        file: "src/lib.rs".into(),
        line: 3,
        what: "unused variable".into(),
        why: "dead code".into(),
    }])]);
    let report = step(&runner).unwrap(); // round 1's qwen call
    assert!(
        matches!(
            report,
            Some(StepReport::ReviewRound {
                round: 1,
                findings: 1,
                ..
            })
        ),
        "{report:?}"
    );

    rig.claude.script([Scripted::Text(
        r#"{"holds": true, "severity": "low", "reason": "it is a nit, not a bug"}"#,
    )]);
    step(&runner).unwrap(); // the judge regrades it to LOW and holds it

    let Some(StepReport::ReviewFindingsSent {
        round, held, clean, ..
    }) = step(&runner).unwrap()
    // the round finalizes: one nit held
    else {
        panic!("the held nit was not sent to the worker");
    };
    assert_eq!((round, held, clean), (1, 1, true));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
        "next"
    );

    rig.claude.script([
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's fix turn
    let fixed = rig.forge.head_of("kelpie/7");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: 7,
            pull_request: 71,
            round: 1,
            head: fixed,
        })
    );
    step(&runner).unwrap(); // round 2, claude: scripted clean above

    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci",
        "two clean rounds in a row end the loop"
    );
}

#[test]
fn a_round_the_script_could_not_finish_is_reported_and_retried() {
    let (rig, runner) = at_round_1("shep");

    rig.reviewer
        .script([ScriptedRound::Fail(crate::ports::ReviewerError::Incomplete)]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed {
            issue: 7,
            reason: "the local round left no completion marker".into(),
        })
    );
    assert_eq!(rig.reviewer.seen().len(), 1);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 1,
            "consecutive_clean": 0,
            "guard_cleared": false,
            "stage": { "stage": "round" },
            "reviewer": "qwen",
        }),
        "the round stays due, and the next step tries it again"
    );
    assert_eq!(
        step(&runner).unwrap(), // clean by default this time
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
            clean: true,
        })
    );
}

#[test]
fn a_rejected_finding_never_reaches_the_worker() {
    let (rig, runner) = at_round_1("shep");

    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::High,
        file: "src/lib.rs".into(),
        line: 9,
        what: "looks racy".into(),
        why: "two threads write the same field".into(),
    }])]);
    step(&runner).unwrap(); // round 1's qwen call

    rig.claude.script([Scripted::Text(
        r#"{"holds": false, "severity": "high", "reason": "the field is behind a mutex"}"#,
    )]);
    step(&runner).unwrap(); // the judge rejects it

    assert_eq!(
        step(&runner).unwrap(), // the round finalizes: nothing held
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
            clean: true,
        })
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
        "ended",
        "no fix turn was queued for a finding the judge rejected"
    );
    assert_eq!(
        rig.claude.calls().len(),
        1,
        "only the worker's own turn is a `Role::Worker` call"
    );
    assert_eq!(
        rig.claude.all_calls().len(),
        2,
        "the worker's turn, and the judge's one-shot"
    );
}

#[test]
fn reviewer_and_judge_sessions_never_match_the_workers() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: scripted clean above

    let worker_session = rig.ask(&runner, "status", None)["work_item"]["session"].clone();
    let all = rig.claude.all_calls();
    let reviewer_call = all
        .iter()
        .find(|c| c.role == Role::Reviewer)
        .expect("a reviewer round ran");
    assert_ne!(json!(reviewer_call.session.id()), worker_session);
    assert!(
        matches!(reviewer_call.session, Session::New(_)),
        "a fresh session every round, never the worker's resumed"
    );
}

#[test]
fn the_round_guard_parks_for_a_ruling_and_a_yes_clears_it_for_the_rest_of_the_item() {
    let (rig, runner, _) = Rig::with_pull_request("shep");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    saved["work_items"][0]["phase"] = json!({
        "state": "review",
        "round": 9,
        "consecutive_clean": 0,
        "guard_cleared": false,
        "stage": { "stage": "round" },
    });
    std::fs::write(&state, saved.to_string()).unwrap();
    let runner = rig.open().unwrap();

    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("the round guard did not raise a ruling");
    };
    assert!(question.contains("8 rounds"), "{question}");

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(
        step(&runner).unwrap(), // round 9, qwen: clean by default, past the guard
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 9,
            held: 0,
            clean: true,
        })
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 10,
            "consecutive_clean": 1,
            "guard_cleared": true,
            "stage": { "stage": "round" },
            "last": "qwen",
        }),
    );
}

#[test]
fn a_spilled_model_parks_the_round_on_a_ruling_that_alerts_and_a_yes_runs_it_again() {
    let (rig, runner) = at_round_1("shep");
    let reason = "the local model coder is 25% on the GPU, so its rounds would run at CPU speed";
    rig.reviewer
        .script([ScriptedRound::Fail(crate::ports::ReviewerError::Spilled(
            reason.into(),
        ))]);

    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("a spilled model did not raise a ruling");
    };
    assert!(
        question.contains("Round 1 of the qwen-review loop"),
        "{question}"
    );
    assert!(question.contains(reason), "{question}");
    assert!(question.contains("runs the round again"), "{question}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": id })
    );
    assert_eq!(status["work_item"]["qwen"]["rounds"], 0, "no round was run");
    assert_eq!(rig.reviewer.seen().len(), 1);

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    assert_eq!(rig.alerts.posts()[0].1.text, question);

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 1,
            "consecutive_clean": 0,
            "guard_cleared": false,
            "stage": { "stage": "round" },
            "reviewer": "qwen",
        }),
        "the same round is due again"
    );
    assert_eq!(
        step(&runner).unwrap(), // the model is back: clean by default
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
            clean: true,
        })
    );
    assert_eq!(rig.reviewer.seen().len(), 2);
}

#[test]
fn status_shows_where_the_local_model_sits() {
    let (rig, runner) = at_round_1("shep");
    assert!(
        rig.ask(&runner, "status", None)
            .get("local_model")
            .is_none(),
        "nothing to show until a round has looked"
    );
    rig.reviewer.set_seat(Some(crate::ports::ModelSeat {
        name: "coder:latest".into(),
        size: 1000,
        size_vram: 900,
        context_length: Some(32768),
        expires_at: Some("2026-09-29T21:14:03+01:00".into()),
    }));
    assert_eq!(
        rig.ask(&runner, "status", None)["local_model"],
        json!({
            "name": "coder:latest",
            "gpu_percent": 90,
            "context_length": 32768,
            "expires_at": "2026-09-29T21:14:03+01:00",
        })
    );
}

// Where round 1 stands before its qwen call runs: the worker's first
// turn opened the pull request.
fn at_round_1(project: &str) -> (Rig, std::sync::Mutex<crate::runner::Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    (rig, runner)
}

#[test]
fn a_claude_round_with_a_finding_records_a_reviewer_call_and_a_judge_call() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([
        Scripted::Billed(
            "MEDIUM|src/lib.rs:3|unused variable|dead code",
            Cost(50_000_000),
        ),
        Scripted::Billed(
            r#"{"holds": false, "severity": "low", "reason": "it is used"}"#,
            Cost(7_000_000),
        ),
    ]);
    step(&runner).unwrap(); // round 2, claude: one finding
    step(&runner).unwrap(); // the judge rejects it

    let status = rig.ask(&runner, "status", None);
    let item = &status["work_item"];
    assert_eq!(
        item["calls"], 3,
        "the worker's turn, the reviewer's and the judge's"
    );
    assert_eq!(
        item["by_role"]["reviewer"],
        json!({ "calls": 1, "cost_usd": 0.05 })
    );
    assert_eq!(
        item["by_role"]["judge"],
        json!({ "calls": 1, "cost_usd": 0.007 })
    );
    assert_eq!(
        item["by_role"]["worker"],
        json!({ "calls": 1, "cost_usd": 0.0 }),
        "the worker's turn cost nothing here, and is not the reviewer's"
    );
    assert_eq!(item["qwen"]["rounds"], 1);
}

#[test]
fn a_judge_reply_that_cannot_be_read_is_still_recorded() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([
        Scripted::Billed("HIGH|src/lib.rs:9|racy|two writers", Cost(1)),
        Scripted::Billed("not json", Cost(9)),
    ]);
    step(&runner).unwrap(); // round 2, claude: one finding
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { .. })
    ));

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["by_role"]["judge"]["calls"], 1);
}

#[test]
fn a_qwen_rounds_time_runs_from_when_it_was_marked_running() {
    let mut item = crate::test::a_work_item();
    item.review_call = ReviewCallState::Running {
        since: Timestamp(100),
    };
    record_spent(&mut item, Some(Spent::Local), Timestamp(190));
    record_spent(&mut item, Some(Spent::Local), Timestamp(200));
    assert_eq!(item.qwen.rounds, 2);
    assert_eq!(
        item.qwen.seconds, 90,
        "the second had no start to measure from"
    );
    assert_eq!(item.review_call, ReviewCallState::Idle);
}

#[test]
fn a_finished_work_items_report_carries_its_totals() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude
        .script([Scripted::Billed("CLEAN", Cost(20_000_000))]);
    step(&runner).unwrap(); // round 2, claude: clean, so CI is next
    rig.forge
        .set_state(71, crate::ports::PullRequestState::Merged);
    let Some(StepReport::Finished { spend, qwen, .. }) = step(&runner).unwrap() else {
        panic!("the merged work item did not finish");
    };
    assert_eq!(spend.reviewer.calls, 1);
    assert_eq!(qwen.rounds, 1);
}

#[test]
fn a_qwen_round_stopped_with_the_runner_runs_again_on_restart() {
    let (rig, runner) = at_round_1("shep");
    rig.reviewer
        .script([ScriptedRound::Fail(crate::ports::ReviewerError::Stopped)]);
    assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(
        step(&runner).unwrap(), // clean by default this time
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
            clean: true,
        })
    );
    assert_eq!(rig.reviewer.seen().len(), 2, "round 1 ran again");
}

#[test]
fn a_claude_round_that_summarises_and_ends_on_clean_is_clean() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude
        .script([Scripted::Text("The refactor is correct.\n\nCLEAN\n")]);
    step(&runner).unwrap(); // round 2, claude
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

#[test]
fn a_claude_round_that_only_mentions_clean_fails_the_gate() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude
        .script([Scripted::Text("CLEAN would be premature.\nnot CLEAN")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { .. })
    ));
}

#[test]
fn a_claude_round_stopped_with_the_runner_runs_again_on_restart() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([Scripted::Fail(ClaudeError::Stopped)]);
    assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "consecutive_clean": 1,
            "guard_cleared": false,
            "stage": { "stage": "round" },
            "reviewer": "claude",
            "last": "qwen",
        }),
        "round 2 is still due, and round 1's clean still counts"
    );
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap();
    let rounds = rig.claude.all_calls();
    let rounds = rounds.iter().filter(|c| c.role == Role::Reviewer).count();
    assert_eq!(rounds, 2, "round 2 ran again");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

#[test]
fn a_judge_call_stopped_with_the_runner_runs_again_on_restart() {
    let (rig, runner) = at_round_1("shep");
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::High,
        file: "src/lib.rs".into(),
        line: 9,
        what: "looks racy".into(),
        why: "two threads write the same field".into(),
    }])]);
    step(&runner).unwrap(); // round 1's qwen call
    rig.claude.script([Scripted::Fail(ClaudeError::Stopped)]);
    assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["stage"]["verdicts"],
        json!([]),
    );
    rig.claude.script([Scripted::Text(
        r#"{"holds": false, "severity": "high", "reason": "the field is behind a mutex"}"#,
    )]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FindingJudged {
            issue: 7,
            round: 1,
            holds: false,
            severity: Severity::High,
        })
    );
}
