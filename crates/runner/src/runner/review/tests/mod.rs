use serde_json::json;

use super::*;
use crate::ports::{AgentError, CLEAN, Cost, Role, Session, SessionId, Severity, Timestamp, Usage};
use crate::runner::report::Spent;
use crate::runner::{Pass, advance, step};
use crate::test::ticket;
use crate::test::{Rig, Scripted, ScriptedRound, git};
use crate::worktree::Remote;

// `drop` refuses while a call is running (see merge/mod.rs's own test for
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
            ended: None,
        })
        .unwrap();
    assert_eq!(report, None);
}

#[test]
fn each_reviewer_runs_once_in_order_and_the_last_goes_to_ci() {
    let (rig, runner) = at_round_1("shep");
    rig.claude.script([Scripted::Text(CLEAN)]);
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
fn a_round_that_found_nothing_is_journaled_as_a_round_that_read() {
    let (rig, runner) = at_round_1("shep");
    rig.claude.script([Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: clean

    let rounds: Vec<serde_json::Value> = crate::runner::retro::journal::read(&rig.paths().journal)
        .iter()
        .filter_map(|s| serde_json::to_value(&s.entry).ok())
        .filter(|e| e["entry"] == "round")
        .collect();
    assert_eq!(
        json!(rounds),
        json!([
            {"entry": "round", "issue": "7", "round": 1, "reviewer": "qwen", "findings": 0},
            {"entry": "round", "issue": "7", "round": 2, "reviewer": "claude", "findings": 0},
        ]),
        "{rounds:#?}"
    );
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
        "ran": ["qwen"],
    });
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Text(CLEAN)]);
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
        "ran": ["qwen", "claude"],
    });
    std::fs::write(&state, saved.to_string()).unwrap();

    rig.ci_on();
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
fn a_round_above_a_nit_sends_every_finding_for_one_fix_and_the_next_reviewer_reads_it() {
    let (rig, runner) = at_round_1("shep");

    rig.reviewer.script([ScriptedRound::Findings(vec![
        Finding::new(
            Severity::Medium,
            "src/lib.rs",
            3,
            "the flag is misnamed",
            "it reads as its opposite",
        ),
        Finding::new(
            Severity::Low,
            "src/lib.rs",
            9,
            "unused variable",
            "dead code",
        ),
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
            issue: ticket(7),
            pull_request: 71,
            round: 1,
            held: 2,
        }),
        "the nit goes too, at the reviewer's own severity"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["turn"]["state"], "next");
    let held = runner.lock().unwrap().state.work_items[0].held.clone();
    assert_eq!(
        held.len(),
        1,
        "a nit is never held for a follow-up: {held:?}"
    );
    assert_eq!(held[0].severity, Severity::Medium);
    let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        text.contains("MEDIUM|src/lib.rs:3|the flag is misnamed"),
        "{text}"
    );
    assert!(text.contains("LOW|src/lib.rs:9|unused variable"), "{text}");

    rig.claude.script([
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap(); // the worker's fix turn
    let fixed = rig.forge.head_of("kelpie/7");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: ticket(7),
            pull_request: 71,
            round: 1,
            head: fixed,
            checks: Vec::new(),
            unchecked: 0,
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
            issue: ticket(7),
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
            issue: ticket(7),
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
            issue: ticket(7),
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
    rig.claude.script([Scripted::Text(CLEAN)]);
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
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
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

// Where round 1 stands before its qwen call runs: the worker's first
// turn opened the pull request.
fn at_round_1(project: &str) -> (Rig, std::sync::Mutex<crate::runner::Runner>) {
    let rig = Rig::new(project);
    rig.allow(&["sonnet-high", "qwen", "claude"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    (rig, runner)
}

// A round waits for CI on the head it would read, which it asks the forge for
// with the runner's lock let go
#[test]
fn a_trigger_is_answered_while_the_review_head_is_read() {
    let rig = Rig::new("shep");
    rig.ci_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let read = rig.forge.hold_next_pull_request_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the head's read held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    pass.unwrap();
}

// A round with findings, which the next step sends to the worker against the head git reports
fn after_a_round_with_findings(project: &str) -> (Rig, std::sync::Mutex<crate::runner::Runner>) {
    let (rig, runner) = at_round_1(project);
    rig.reviewer
        .script([ScriptedRound::Findings(vec![Finding::new(
            Severity::Medium,
            "src/lib.rs",
            3,
            "the flag is misnamed",
            "it reads as its opposite",
        )])]);
    step(&runner).unwrap(); // round 1's qwen call
    (rig, runner)
}

// The head the findings are sent against is asked of git, a fetch the step makes with the
// runner's lock let go, after the fetch that checks the branch for agents' own files
#[test]
fn a_trigger_is_answered_while_origin_head_is_read() {
    let (rig, runner) = after_a_round_with_findings("shep");
    let read = rig.hold_git_fetch_after(1);

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the fetch held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    assert!(
        matches!(
            pass,
            Ok(Pass::Report(StepReport::ReviewFindingsSent { .. }))
        ),
        "{pass:?}"
    );
}

fn gate_failed(report: Option<StepReport>) -> String {
    match report {
        Some(StepReport::GateFailed { reason, .. }) => reason,
        other => panic!("the gate did not fail: {other:?}"),
    }
}

// The step fails as it did when it made the read in place, with git's own words
#[test]
fn a_git_read_that_fails_reaches_the_step_as_the_error_the_inline_read_returned() {
    let (rig, runner) = after_a_round_with_findings("shep");
    git(
        &rig.repo(),
        &["remote", "set-url", "origin", "/nowhere/at/all"],
    );
    let inline = worktree::origin_head(&rig.repo(), "kelpie/7", &Remote::Origin).unwrap_err();

    let reason = gate_failed(step(&runner).unwrap());

    assert_eq!(reason, inline.to_string());
}

// A rebase that holds the lock can have a ref of the repo locked as a fetch in flight updates it.
// The fetch fails with git's words, the step fails, and the next step reads again.
#[test]
fn a_read_in_flight_while_the_pass_rebases_ends_in_the_error_path() {
    let (rig, runner) = after_a_round_with_findings("shep");
    let read = rig.hold_git_fetch_after(1);
    let lock = rig.repo().join(".git/refs/remotes/origin/kelpie/7.lock");

    let (_, pass) = rig.during_read(
        read,
        || advance(&runner),
        || {
            rig.push_by_hand("kelpie/7", "other.txt");
            std::fs::write(&lock, "").unwrap();
        },
    );

    let Ok(Pass::Report(report)) = pass else {
        panic!("the pass made no report: {pass:?}");
    };
    assert!(gate_failed(Some(report)).contains("kelpie/7.lock"));
    std::fs::remove_file(&lock).unwrap();
    rig.clock.advance(60); // past the failed step's wait
    let again = step(&runner).unwrap();
    assert!(
        matches!(again, Some(StepReport::ReviewFindingsSent { .. })),
        "{again:?}"
    );
}

// A work item dropped while its head is read is gone when the read comes
// back, so no round is made for it.
#[test]
fn an_item_that_left_review_during_the_read_is_not_stepped() {
    let rig = Rig::new("shep");
    rig.ci_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let read = rig.forge.hold_next_pull_request_read();

    let (_, pass) = rig.during_read(read, || advance(&runner), || rig.ask(&runner, "drop", None));

    assert!(matches!(pass.unwrap(), Pass::Idle));
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert_eq!(rig.reviewer.seen().len(), 0, "no round was made");
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
        .script([Scripted::Billed(CLEAN, Cost(20_000_000))]);
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
            issue: ticket(7),
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
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "reviewer": "claude",
            "ran": ["qwen"],
            "reading": pushed,
        }),
        "round 2 is still due, with the same reviewer and the head it reads"
    );
    rig.claude.script([Scripted::Text(CLEAN)]);
    step(&runner).unwrap();
    let rounds = rig.claude.all_calls();
    let rounds = rounds.iter().filter(|c| c.role == Role::Reviewer).count();
    assert_eq!(rounds, 2, "round 2 ran again");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

const DOLLAR: u64 = 1_000_000_000;

// Issue 7 in review of pull request 71 under `budget.per_item_usd = 5`, the
// worker having pushed
fn in_review_under_a_five_dollar_cap(project: &str) -> (Rig, std::sync::Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| s.replace("per_item_usd = 10", "per_item_usd = 5"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    (rig, runner)
}

// A priced worker call that cost `usd` dollars, as a turn kelpie ran would leave
fn spend(runner: &std::sync::Mutex<Runner>, usd: u64) {
    let session = SessionId("spent".into());
    let cost = Some(Cost(usd * DOLLAR));
    let call = |item: &mut WorkItem| {
        item.record_call(Role::Worker, Timestamp(1), session, Usage::default(), cost);
    };
    runner.lock().unwrap().update(call).unwrap();
}

fn reviewer_calls(rig: &Rig) -> usize {
    let calls = rig.claude.all_calls();
    calls.iter().filter(|c| c.role == Role::Reviewer).count()
}

#[test]
fn a_local_round_does_not_start_past_the_cap() {
    let (rig, runner) = in_review_under_a_five_dollar_cap("shep");
    spend(&runner, 6);

    let Some(StepReport::Failed {
        issue,
        pull_request: Some(71),
        id,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the round was not parked");
    };
    assert_eq!(id, ticket(71));
    assert_eq!(issue, ticket(7));
    assert!(
        rig.reviewer.seen().is_empty(),
        "the local round did not run"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ruling");
    let kind = &status["rulings"][0]["kind"];
    assert_eq!(kind["reason"], "over-budget");
    let phase = &kind["phase"];
    assert_eq!(
        (&phase["state"], &phase["round"], &phase["stage"]),
        (&json!("review"), &json!(1), &json!({ "stage": "round" })),
        "the ruling names the review stage the round was in"
    );
}

#[test]
fn a_session_round_does_not_start_past_the_cap_and_a_yes_resumes_that_round() {
    let (rig, runner) = in_review_under_a_five_dollar_cap("shep");
    step(&runner).unwrap(); // round 1, qwen: clean by default
    spend(&runner, 6);

    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Failed { issue, .. }) if issue == ticket(7)
    ));
    assert_eq!(reviewer_calls(&rig), 0, "round 2 did not start");

    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.ask(&runner, "rule", Some("71 yes"));
    let phase = &rig.ask(&runner, "status", None)["work_item"]["phase"];
    assert_eq!(phase["state"], "review", "{phase}");
    assert_eq!(phase["round"], 2, "the same round is due again: {phase}");

    step(&runner).unwrap();
    assert_eq!(reviewer_calls(&rig), 1, "round 2 ran at the doubled cap");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

#[test]
fn the_question_for_a_round_past_the_cap_names_the_round_and_not_a_worker_turn() {
    let (rig, runner) = in_review_under_a_five_dollar_cap("shep");
    spend(&runner, 6);
    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("the round was not parked");
    };
    assert_eq!(
        question,
        "The work on pull request #71 has cost $6.00 against its cap of $5.00: the worker $6.00 and \
         the reviewers $0.00, so the review round has not started. \
         `shep kelpie rule 71 yes` raises the cap to $10.00 and the round starts, and \
         `shep kelpie rule 71 no <note>` sends the worker your note, raises the cap the same \
         and has the pull request reviewed again."
    );
    drop(rig);
}

#[test]
fn a_no_on_a_round_past_the_cap_sends_the_worker_the_note_and_reviews_again() {
    let (rig, runner) = in_review_under_a_five_dollar_cap("shep");
    spend(&runner, 6);
    step(&runner).unwrap(); // parked before round 1
    rig.claude.script([Scripted::Push("more.txt", "more\n")]);
    rig.ask(&runner, "rule", Some("71 no rename the flag"));

    step(&runner).unwrap(); // the worker's fix turn
    let sent = rig.claude.calls();
    let note = sent.last().unwrap();
    assert!(note.prompt.contains("rename the flag"), "{}", note.prompt);
    assert!(
        rig.reviewer.seen().is_empty(),
        "no round ran before the fix"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "review",
        "the pull request goes through the review again"
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_round_under_the_cap_starts_as_before() {
    let (rig, runner) = in_review_under_a_five_dollar_cap("shep");
    spend(&runner, 5);
    rig.claude.script([Scripted::Text(CLEAN)]);

    step(&runner).unwrap(); // round 1, qwen
    step(&runner).unwrap(); // round 2, claude
    assert_eq!(
        rig.reviewer.seen().len(),
        1,
        "exactly the cap is not over it"
    );
    assert_eq!(reviewer_calls(&rig), 1);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
        "ci"
    );
}

// Round 2, the claude reviewer's, is next, and the project asks for tests.
fn at_round_2_asking_tests() -> (Rig, std::sync::Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.test_command("cargo test --test {name}");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    (rig, runner)
}

fn kept_tests(rig: &Rig) -> std::path::PathBuf {
    rig.paths().worker.join("review-tests-7")
}

// The one finding the claude round holds after its reply, and the findings
// file the worker is sent.
fn held_after(rig: &Rig, runner: &std::sync::Mutex<Runner>) -> (Finding, String) {
    step(runner).unwrap(); // round 2, claude
    step(runner).unwrap(); // the findings sent
    let held = runner.lock().unwrap().state.work_items[0].held.clone();
    assert_eq!(held.len(), 1, "{held:?}");
    let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    (held[0].clone(), text)
}

const FLAG_FINDING: &str = "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing";

#[test]
fn an_attached_file_is_kept_for_its_finding() {
    let (rig, runner) = at_round_2_asking_tests();
    rig.claude.script([Scripted::Attach(
        "tests/flag.rs",
        "#[test]\nfn flag_is_read() { assert!(false); }\n",
        "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs",
    )]);

    let (found, text) = held_after(&rig, &runner);

    assert_eq!(found.test.as_deref(), Some("tests/flag.rs"));
    assert_eq!(found.why, "a caller sees nothing");
    assert!(
        text.contains(&format!("{FLAG_FINDING}|tests/flag.rs")),
        "{text}"
    );
    let kept = std::fs::read_to_string(kept_tests(&rig).join("tests/flag.rs")).unwrap();
    assert!(kept.contains("flag_is_read"), "{kept}");
    let original = rig.build_7().join("review-tests/tests/flag.rs");
    assert_eq!(std::fs::read_to_string(original).unwrap(), kept);
}

#[test]
fn a_later_reviewers_first_look_keeps_the_tests_kept_before() {
    let rig = Rig::new("shep");
    rig.test_command("cargo test --test {name}");
    rig.write_agent("second", crate::test::CLAUDE_REVIEWER);
    rig.reviewers(&["claude", "second"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Attach(
            "tests/flag.rs",
            "#[test]\nfn flag_is_read() { assert!(false); }\n",
            "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs",
        ),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, the first claude reviewer
    step(&runner).unwrap(); // its findings sent
    let kept = kept_tests(&rig).join("tests/flag.rs");
    assert!(kept.is_file(), "the first reviewer's test is kept");

    rig.claude.script([
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the fix pushed
    step(&runner).unwrap(); // round 2, the second claude reviewer

    assert_eq!(
        rig.claude.all_seen().len(),
        4,
        "worker, reviewer, fix, reviewer"
    );
    assert!(kept.is_file(), "a later reviewer's first look keeps it");
    let held = runner.lock().unwrap().state.work_items[0].held.clone();
    assert_eq!(held[0].test.as_deref(), Some("tests/flag.rs"));
}

#[test]
fn a_missing_file_costs_its_finding_only_the_test() {
    let (rig, runner) = at_round_2_asking_tests();
    rig.claude.script([Scripted::Text(
        "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs",
    )]);

    let (found, text) = held_after(&rig, &runner);

    assert_eq!(found.test, None);
    assert_eq!(found.what, "the flag is read first");
    // The reviewer may have meant a pipe in its why, so no word of it is lost.
    assert_eq!(found.why, "a caller sees nothing|tests/flag.rs");
    assert!(
        text.contains(&format!("{FLAG_FINDING}|tests/flag.rs")),
        "{text}"
    );
    assert!(!kept_tests(&rig).join("tests/flag.rs").exists());
    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(notes.contains("tests/flag.rs"), "{notes}");
    assert!(notes.contains("no such file"), "{notes}");
}

#[test]
fn a_path_the_worktree_already_holds_costs_its_finding_only_the_test() {
    let (rig, runner) = at_round_2_asking_tests();
    // `work.txt` is on the branch the reviewer reads.
    rig.claude.script([Scripted::Attach(
        "work.txt",
        "not the work\n",
        "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|work.txt",
    )]);

    let (found, text) = held_after(&rig, &runner);

    assert_eq!(found.test, None);
    assert_eq!(found.why, "a caller sees nothing");
    assert!(!text.contains("work.txt"), "{text}");
    assert!(!kept_tests(&rig).join("work.txt").exists());
    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(notes.contains("already holds"), "{notes}");
    let work = std::fs::read_to_string(rig.worktree_7().join("work.txt")).unwrap();
    assert_eq!(work, "work\n", "the branch's file is as it was");
}

#[cfg(unix)]
#[test]
fn a_linked_test_file_is_refused() {
    let (rig, runner) = at_round_2_asking_tests();
    rig.claude.script([Scripted::AttachLinked(
        "tests/flag.rs",
        "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs",
    )]);

    let (found, text) = held_after(&rig, &runner);

    assert_eq!(found.test, None);
    assert!(!text.contains("tests/flag.rs"), "{text}");
    assert!(!kept_tests(&rig).join("tests/flag.rs").exists());
    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(notes.contains("tests/flag.rs"), "{notes}");
}

mod kept;
