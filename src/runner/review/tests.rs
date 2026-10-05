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

#[test]
fn each_reviewer_runs_once_in_order_and_the_last_goes_to_ci() {
    let (rig, runner) = at_round_1("shep");
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // round 1, qwen: clean by default
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "ran": ["qwen"],
        }),
        "the next reviewer in the list is due"
    );
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci",
        "no reviewer is left after claude"
    );
    assert_eq!(rig.reviewer.seen().len(), 1, "qwen ran once");
}

#[test]
fn a_review_saved_mid_loop_goes_on_from_the_reviewer_after_the_one_it_recorded() {
    let (rig, runner) = at_round_1("shep");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    saved["work_items"][0]["phase"] = json!({
        "state": "review",
        "round": 4,
        "consecutive_clean": 1,
        "guard_cleared": false,
        "stage": { "stage": "round" },
        "last": "qwen",
    });
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // claude, the reviewer after qwen
    assert!(rig.reviewer.seen().is_empty(), "qwen does not run again");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

#[test]
fn a_review_saved_after_its_last_reviewer_goes_on_to_ci() {
    let (rig, runner) = at_round_1("shep");
    drop(runner);
    let state = rig.paths().state;
    let text = std::fs::read_to_string(&state).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    saved["work_items"][0]["phase"] = json!({
        "state": "review",
        "round": 6,
        "consecutive_clean": 1,
        "guard_cleared": false,
        "stage": { "stage": "round" },
        "last": "claude",
    });
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
    assert!(rig.reviewer.seen().is_empty());
    assert_eq!(rig.claude.all_calls().len(), 1, "only the worker's turn");
}

#[test]
fn a_round_of_nits_goes_to_the_next_reviewer_with_no_fix_turn() {
    let (rig, runner) = at_round_1("shep");
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Low,
        file: "src/lib.rs".into(),
        line: 3,
        what: "unused variable".into(),
        why: "dead code".into(),
    }])]);
    step(&runner).unwrap(); // round 1's qwen call: one nit
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 0,
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["turn"]["state"], "ended");
    assert_eq!(status["work_item"]["phase"]["ran"], json!(["qwen"]));
    assert_eq!(rig.claude.calls().len(), 1, "no fix turn");
}

#[test]
fn a_round_above_a_nit_sends_every_finding_for_one_fix_and_the_next_reviewer_reads_it() {
    let (rig, runner) = at_round_1("shep");

    rig.reviewer.script([ScriptedRound::Findings(vec![
        Finding {
            severity: Severity::Medium,
            file: "src/lib.rs".into(),
            line: 3,
            what: "the flag is misnamed".into(),
            why: "it reads as its opposite".into(),
        },
        Finding {
            severity: Severity::Low,
            file: "src/lib.rs".into(),
            line: 9,
            what: "unused variable".into(),
            why: "dead code".into(),
        },
    ])]);
    let report = step(&runner).unwrap(); // round 1's qwen call
    assert!(
        matches!(
            report,
            Some(StepReport::ReviewRound {
                round: 1,
                findings: 2,
                ..
            })
        ),
        "{report:?}"
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 2,
        }),
        "the nit goes too, at the reviewer's own severity"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["turn"]["state"], "next");
    let held = runner.lock().unwrap().state.work_items[0].held.clone();
    assert_eq!(held.len(), 2, "{held:?}");
    assert_eq!(held[1].severity, Severity::Low);
    let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        text.contains("MEDIUM|src/lib.rs:3|the flag is misnamed"),
        "{text}"
    );
    assert!(text.contains("LOW|src/lib.rs:9|unused variable"), "{text}");

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
        "claude was the last reviewer"
    );
    assert_eq!(
        rig.reviewer.seen().len(),
        1,
        "qwen did not read the fix again"
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
            "stage": { "stage": "round" },
            "reviewer": "qwen",
            "failures": 1,
            "unread": true,
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
        })
    );
}

#[test]
fn a_reviewer_whose_call_fails_three_times_in_a_row_is_passed_over_for_the_pass() {
    let (rig, runner) = at_round_1("shep");
    rig.reviewer
        .script((0..3).map(|_| ScriptedRound::Fail(crate::ports::ReviewerError::Incomplete)));
    for _ in 0..2 {
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::GateFailed { .. })
        ));
    }
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["failures"],
        2
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped {
            issue: 7,
            pull_request: 71,
            round: 1,
            reviewer: AgentName::try_from("qwen".to_owned()).unwrap(),
            reason: "the local round left no completion marker".into(),
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["reviewers_skipped"], json!(["qwen"]));
    assert_eq!(
        status["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "ran": ["qwen"],
            "unread": true,
        }),
        "qwen was passed over, so nobody has read it yet"
    );
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // round 2, claude
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

#[test]
fn reviewer_sessions_never_match_the_workers() {
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
    assert!(question.contains("Round 1 of the review"), "{question}");
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
            "stage": { "stage": "round" },
            "reviewer": "qwen",
            "unread": true,
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
fn a_claude_round_with_a_finding_records_a_reviewer_call() {
    let (rig, runner) = at_round_1("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([Scripted::Billed(
        "MEDIUM|src/lib.rs:3|unused variable|dead code",
        Cost(50_000_000),
    )]);
    step(&runner).unwrap(); // round 2, claude: one finding

    let status = rig.ask(&runner, "status", None);
    let item = &status["work_item"];
    assert_eq!(item["calls"], 2, "the worker's turn and the reviewer's");
    assert_eq!(
        item["by_role"]["reviewer"],
        json!({ "calls": 1, "tokens": { "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 }, "cost_usd": 0.05 })
    );
    assert_eq!(
        item["by_role"]["worker"],
        json!({ "calls": 1, "tokens": { "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 }, "cost_usd": 0.0 }),
        "the worker's turn cost nothing here, and is not the reviewer's"
    );
    assert_eq!(item["by_role"].get("judge"), None);
    assert_eq!(item["qwen"]["rounds"], 1);
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
    rig.claude.script([Scripted::Fail(AgentError::Stopped)]);
    assert_eq!(step(&runner).unwrap(), None, "no failed gate is reported");
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "reviewer": "claude",
            "ran": ["qwen"],
        }),
        "round 2 is still due, with the same reviewer"
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
