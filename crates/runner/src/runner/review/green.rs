//! No reviewer reads a head until CI has passed on it (ADR 0008)

use std::sync::Mutex;

use crate::lease::LeaseKind;
use crate::ports::{CLEAN, Checks, Cost, Finding, Severity, Usage};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

fn finding(severity: Severity) -> Finding {
    Finding::new(severity, "src/lib.rs", 3, "unused variable", "dead code")
}

// The worker's first turn has pushed and opened pull request 71, whose head
// is returned, with CI on it not yet reported. The review is due.
fn pushed() -> (Rig, Mutex<Runner>, String) {
    pushed_set(|_| {})
}

// [`pushed`], after `setup` changed the rig's settings or repo.
fn pushed_set(setup: impl FnOnce(&Rig)) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new("shep");
    rig.ci_on();
    setup(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    (rig, runner, head)
}

// How many times a reviewer has read: qwen's local rounds and Claude's
// reviewer sessions, which the worker's turns are not.
fn reads(rig: &Rig) -> usize {
    let sessions = rig.claude.all_seen().len() - rig.claude.seen().len();
    rig.reviewer.seen().len() + sessions
}

fn red(rig: &Rig, head: &str) {
    rig.forge
        .set_checks(head, Checks::Failed(vec!["lint".into()]));
}

fn state(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let status = rig.ask(runner, "status", None);
    status["work_item"]["phase"]["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn ci_fix_turns(runner: &Mutex<Runner>) -> u32 {
    runner.lock().unwrap().state.work_items[0]
        .counts
        .ci_fix_turns
}

// The first reviewer found `severity`, and the worker's fix turn pushed a
// head past the green one. Its report is read, so the next step is the next
// round's. Returns the fix's head.
fn fixed(rig: &Rig, runner: &Mutex<Runner>, head: &str, severity: Severity) -> String {
    rig.forge.set_checks(head, Checks::Passed);
    rig.reviewer
        .script([ScriptedRound::Findings(vec![finding(severity)])]);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    rig.verdict(runner); // round 1, qwen: one finding
    step(runner).unwrap(); // the finding goes to a fix turn
    step(runner).unwrap(); // the worker's fix turn pushes
    let report = step(runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::FixPushed { .. })),
        "{report:?}"
    );
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head);
    fixed
}

#[test]
fn the_review_starts_only_once_the_pushed_head_is_green() {
    let (rig, runner, head) = pushed();

    assert_eq!(step(&runner).unwrap(), None, "no run is reported yet");
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None, "a pending run waits");
    assert_eq!(reads(&rig), 0, "nobody read the head");

    rig.forge.set_checks(&head, Checks::Passed);
    let report = step(&runner).unwrap();
    assert!(
        matches!(
            report,
            Some(StepReport::ReviewFindingsSent { round: 1, .. })
        ),
        "{report:?}"
    );
    assert_eq!(reads(&rig), 1, "the first reviewer read the green head");
}

#[test]
fn a_green_run_not_yet_settled_does_not_start_the_review() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::Passed);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(reads(&rig), 0, "a check set registers a check at a time");

    rig.clock.advance(CHECKS_SETTLE);
    assert!(step(&runner).unwrap().is_some());
    assert_eq!(reads(&rig), 1);
}

#[test]
fn a_red_head_goes_to_the_ci_fix_turn_before_any_reviewer_reads() {
    let (rig, runner, head) = pushed();
    red(&rig, &head);

    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::CiFailed { head: ref h, .. }) if *h == head),
        "{report:?}"
    );
    assert_eq!(reads(&rig), 0, "the red head was read by no reviewer");

    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap(); // the worker's CI fix turn
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head);
    assert_eq!(
        state(&rig, &runner),
        "review",
        "the fix goes back to the review, not on to CI and the merge"
    );
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(reads(&rig), 0, "the fix's head waits for green too");

    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(rig.verdict(&runner).is_some());
    assert_eq!(reads(&rig), 1, "the first reviewer reads the green fix");
    assert_eq!(ci_fix_turns(&runner), 1);
}

#[test]
fn a_head_that_is_red_again_after_the_fix_never_reaches_a_reviewer() {
    let (rig, runner, head) = pushed();
    red(&rig, &head);
    rig.verdict(&runner);
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    let fixed = rig.forge.head_of("kelpie/7").unwrap();

    red(&rig, &fixed);
    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::CiFailed { head: ref h, .. }) if *h == fixed),
        "a second fix turn, not a reviewer: {report:?}"
    );
    assert_eq!(reads(&rig), 0);
    assert_eq!(ci_fix_turns(&runner), 2);
}

#[test]
fn the_fix_attempts_cap_counts_the_red_runs_before_the_review() {
    let (rig, runner, head) = pushed();
    rig.fix_attempts(0);
    drop(runner);
    let runner = rig.open().unwrap();
    red(&rig, &head);

    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "the cap parks it on a ruling: {report:?}"
    );
    assert_eq!(rig.claude.calls().len(), 1, "no fix turn went out");
    assert_eq!(reads(&rig), 0);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "still-red");
}

#[test]
fn a_fix_turns_head_waits_for_green_before_the_next_reviewer() {
    let (rig, runner, head) = pushed();
    let fixed = fixed(&rig, &runner, &head, Severity::High);
    assert_eq!(reads(&rig), 1);

    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None, "the fix's run is pending");
    assert_eq!(reads(&rig), 1, "the next reviewer waits");

    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.forge.set_checks(&fixed, Checks::Passed);
    step(&runner).unwrap();
    assert_eq!(reads(&rig), 2, "and then reads it");
}

#[test]
fn a_red_fix_head_goes_to_the_ci_fix_turn_before_the_next_reviewer() {
    let (rig, runner, head) = pushed();
    let fixed = fixed(&rig, &runner, &head, Severity::High);

    red(&rig, &fixed);
    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::CiFailed { head: ref h, .. }) if *h == fixed),
        "{report:?}"
    );
    assert_eq!(reads(&rig), 1, "no reviewer read it");
    assert_eq!(ci_fix_turns(&runner), 1);
}

#[test]
fn a_nit_only_fix_waits_for_green_before_the_pass_goes_on() {
    let (rig, runner, head) = pushed();
    let fixed = fixed(&rig, &runner, &head, Severity::Low);

    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(reads(&rig), 1, "claude waits for the run");

    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.forge.set_checks(&fixed, Checks::Passed);
    step(&runner).unwrap();
    assert_eq!(reads(&rig), 2);
}

#[test]
fn a_head_green_for_one_reviewer_is_not_waited_on_again_for_the_next() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // round 1, qwen: clean
    rig.claude.script([Scripted::Text(CLEAN)]);

    step(&runner).unwrap(); // round 2, claude: the same head, already green
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(reads(&rig), 2);
}

#[test]
fn with_ci_block_off_the_review_starts_at_once() {
    let (rig, runner, _) = pushed();
    drop(runner);
    rig.edit_settings(|s| s.replace("block = true", "block = false"));
    let runner = rig.open().unwrap();

    assert!(step(&runner).unwrap().is_some());
    assert_eq!(reads(&rig), 1, "no run was waited for");
}

#[test]
fn a_draft_whose_every_check_was_skipped_is_not_green() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::Skipped);

    rig.verdict(&runner);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None, "nothing ran on the draft");
    assert_eq!(reads(&rig), 0);

    // Once it is ready, a skipped rollup is the repo's own choice not to run.
    rig.forge.ready_pull_request(71);
    assert!(step(&runner).unwrap().is_some());
    assert_eq!(reads(&rig), 1);
}

#[test]
fn a_question_in_the_ci_fix_turn_goes_back_to_the_round_that_waited() {
    let (rig, runner, head) = pushed();
    let fixed = fixed(&rig, &runner, &head, Severity::High);
    red(&rig, &fixed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich flag?\n</kelpie-question>\n",
    )]);
    let asked = step(&runner).unwrap();
    assert!(matches!(asked, Some(StepReport::Asked { .. })), "{asked:?}");

    rig.ask(&runner, "rule", Some("71 answer use --dry-run"));
    rig.claude
        .script([Scripted::Push("answer.txt", "answered\n")]);
    step(&runner).unwrap(); // the answered turn pushes
    let status = rig.ask(&runner, "status", None);
    let phase = &status["work_item"]["phase"];
    assert_eq!(phase["state"], "review");
    assert_eq!(phase["ran"], serde_json::json!(["qwen"]), "{phase}");
    assert_eq!(phase["round"], 2, "the pass goes on, not from the top");
}

#[test]
fn a_pull_request_merged_by_hand_while_the_review_waits_is_read_by_no_one() {
    let (rig, runner, _) = pushed();
    rig.forge
        .set_state(71, crate::ports::PullRequestState::Merged);

    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::Finished { merged: true, .. })),
        "{report:?}"
    );
    assert_eq!(reads(&rig), 0);
}

#[test]
fn a_pull_request_closed_by_hand_while_the_review_waits_is_read_by_no_one() {
    let (rig, runner, _) = pushed();
    rig.forge
        .set_state(71, crate::ports::PullRequestState::Closed);

    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    assert_eq!(reads(&rig), 0);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "closed");
}

// CodeRabbit alone is listed, and its summon spends a window that is scarce.
fn bot_pushed() -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    rig.edit_settings(|s| {
        let both = crate::test::RIG_REVIEWERS_AND_CODERABBIT;
        s.replace(both, "reviewers = [\"coderabbit\"]\n")
    });
    rig.ci_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    (rig, runner, head)
}

#[test]
fn a_red_head_is_never_summoned_to_a_review_bot() {
    let (rig, runner, head) = bot_pushed();
    red(&rig, &head);

    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::CiFailed { .. })),
        "{report:?}"
    );
    assert_eq!(rig.forge.readied(), Vec::<u64>::new(), "not marked ready");
    assert_eq!(rig.forge.comments(), [], "no summon was posted");
    assert!(!rig.leases.held(&LeaseKind::coderabbit()));
}

#[test]
fn a_green_head_is_summoned_to_a_review_bot() {
    let (rig, runner, head) = bot_pushed();
    rig.forge.set_checks(&head, Checks::Passed);

    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::MarkedReady { .. })),
        "{report:?}"
    );
}

#[test]
fn the_findings_sent_to_the_worker_are_journaled_with_their_round_for_the_retro() {
    let (rig, runner, head) = pushed();
    fixed(&rig, &runner, &head, Severity::High);

    let journal = crate::runner::retro::journal::read(&rig.paths().journal);

    let sent: Vec<_> = (journal.iter())
        .filter_map(|s| serde_json::to_value(s).ok())
        .map(|s| s["entry"].clone())
        .filter(|e| e["entry"] == "findings")
        .collect();
    assert_eq!(
        sent,
        [serde_json::json!({
            "entry": "findings", "issue": "7", "round": 1,
            "findings": [{"severity": "high", "file": "src/lib.rs", "line": 3, "what": "unused variable"}],
        })]
    );
}

// A red run on the pushed head, a fix turn that pushes nothing, and the
// still-red ruling that follows, under `auto`. Returns the head.
fn parked_still_red(rig: &Rig, runner: &Mutex<Runner>, head: &str) {
    red(rig, head);
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::CiFailed { .. })
    ));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(runner).unwrap(); // the fix turn pushes nothing
    let report = rig.verdict(runner);
    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "still-red");
}

#[test]
fn a_still_red_yes_raised_mid_review_goes_back_to_that_review() {
    let (rig, runner, head) = pushed();
    drop(runner);
    rig.merge_auto();
    let runner = rig.open().unwrap();
    parked_still_red(&rig, &runner, &head);

    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(state(&rig, &runner), "review", "the review it left");

    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script([Scripted::Text(CLEAN)]);
    let mut reports = Vec::new();
    for _ in 0..6 {
        let report = rig.verdict(&runner);
        assert!(
            !matches!(report, Some(StepReport::Finished { .. })) || reads(&rig) == 2,
            "the merge waited for both reviewers: {report:?}"
        );
        reports.push(report);
    }
    assert_eq!(reads(&rig), 2, "{reports:?}");
    assert!(
        reports
            .iter()
            .any(|r| matches!(r, Some(StepReport::Finished { merged: true, .. }))),
        "{reports:?}"
    );
}

#[test]
fn a_still_red_yes_after_review_goes_to_ci() {
    let (rig, runner, head) = pushed();
    drop(runner);
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.verdict(&runner); // round 1, qwen: clean
    step(&runner).unwrap(); // round 2, claude: clean
    assert_eq!(state(&rig, &runner), "ci");
    let rounds = reads(&rig);

    parked_still_red(&rig, &runner, &head);
    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(state(&rig, &runner), "ci");

    rig.forge.set_checks(&head, Checks::Passed);
    let reports: Vec<_> = (0..4).map(|_| rig.verdict(&runner)).collect();
    assert!(
        reports
            .iter()
            .any(|r| matches!(r, Some(StepReport::Finished { merged: true, .. }))),
        "{reports:?}"
    );
    assert_eq!(reads(&rig), rounds, "no new round");
}

#[test]
fn a_still_red_yes_under_ask_adopts_the_fix_pushed_by_hand_before_the_review() {
    let (rig, runner, head) = pushed();
    parked_still_red(&rig, &runner, &head);

    let fixed = rig.push_by_hand("kelpie/7", "lint.txt");
    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(state(&rig, &runner), "review");

    rig.forge.set_checks(&fixed, Checks::Passed);
    let report = rig.verdict(&runner);
    assert!(
        !matches!(report, Some(StepReport::Ruling { .. })),
        "the worktree was behind the pushed head: {report:?}"
    );
    assert_eq!(reads(&rig), 1, "the first reviewer reads the fix");
}

// A conflict with `main` runs no `pull_request` workflow, so the head's checks
// are none for good. The review's wait catches the branch up once they settle.
fn settled_with_no_checks(rig: &Rig, runner: &Mutex<Runner>) -> Option<StepReport> {
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::None);
    assert_eq!(step(runner).unwrap(), None, "no run is reported yet");
    rig.clock.advance(CHECKS_SETTLE);
    step(runner).unwrap()
}

#[test]
fn a_review_head_behind_main_with_no_checks_is_caught_up_and_read_after_green() {
    let (rig, runner, head) = pushed();
    rig.land_on_origin("landed.txt");

    let reported = settled_with_no_checks(&rig, &runner);
    let Some(StepReport::Rebased { head: rebased, .. }) = reported else {
        panic!("the branch was not caught up: {reported:?}");
    };
    assert_ne!(rebased, head);
    assert_eq!(rig.forge.head_of("kelpie/7"), Some(rebased.clone()));
    assert_eq!(state(&rig, &runner), "review");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["reading"], rebased);
    assert_eq!(status["work_item"]["phase"]["ci"], serde_json::Value::Null);
    assert_eq!(reads(&rig), 0, "nobody read the old head");

    rig.forge.set_checks(&rebased, Checks::Passed);
    assert!(rig.verdict(&runner).is_some());
    assert_eq!(reads(&rig), 1, "the first reviewer reads the rebased head");
}

// CodeRabbit's own check run, skipped, is on a head whose workflows never ran.
#[test]
fn a_draft_that_conflicts_with_main_and_shows_only_a_skipped_check_gets_the_conflict_turn() {
    let (rig, runner, head) = pushed();
    rig.land_on_origin("work.txt");
    rig.forge.set_checks(&head, Checks::Skipped);

    assert_eq!(step(&runner).unwrap(), None, "no run is reported yet");
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Conflicted {
            issue: crate::test::ticket(7),
            pull_request: 71,
            head,
            files: vec!["work.txt".into()],
        })
    );
    assert_eq!(reads(&rig), 0);
}

#[test]
fn a_review_head_that_conflicts_with_main_gets_the_conflict_turn_and_comes_back_to_review() {
    let (rig, runner, head) = pushed();
    rig.land_on_origin("work.txt");

    let reported = settled_with_no_checks(&rig, &runner);
    assert_eq!(
        reported,
        Some(StepReport::Conflicted {
            issue: crate::test::ticket(7),
            pull_request: 71,
            head,
            files: vec!["work.txt".into()],
        })
    );
    rig.claude.script([Scripted::FinishMerge]);
    step(&runner).unwrap(); // the worker resolves and pushes the merge
    let [_, conflict] = rig.claude.calls().try_into().unwrap();
    assert!(
        conflict
            .prompt
            .starts_with("Your pull request #71 conflicts with main in work.txt. "),
        "{}",
        conflict.prompt
    );
    let merged = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(state(&rig, &runner), "review", "back to the waiting review");
    assert_eq!(reads(&rig), 0);

    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the merge's run is not reported"
    );
    rig.forge.set_checks(&merged, Checks::Passed);
    assert!(rig.verdict(&runner).is_some());
    assert_eq!(reads(&rig), 1, "the first reviewer reads the merge");
}

#[test]
fn a_conflict_left_unresolved_from_review_raises_a_ruling_naming_the_files() {
    let (rig, runner, _) = pushed();
    rig.land_on_origin("work.txt");
    assert!(matches!(
        settled_with_no_checks(&rig, &runner),
        Some(StepReport::Conflicted { .. })
    ));
    // The worker gives the merge up and pushes something else.
    rig.claude
        .script([Scripted::AbortAndPush("extra.txt", "extra\n")]);
    step(&runner).unwrap();
    assert_eq!(state(&rig, &runner), "review");

    let report = settled_with_no_checks(&rig, &runner);
    let Some(StepReport::Ruling { question, .. }) = report else {
        panic!("no ruling was raised: {report:?}");
    };
    assert!(
        question.contains("it conflicts with main in work.txt"),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "rebase");
    assert_eq!(rig.claude.calls().len(), 2, "no third turn, no loop");
    assert_eq!(reads(&rig), 0);
}

// A conflict turn that gives the merge up and pushes something else, and the
// rebase ruling the review's wait raises for it.
fn parked_on_a_rebase_from_review(rig: &Rig, runner: &Mutex<Runner>) {
    rig.land_on_origin("work.txt");
    assert!(matches!(
        settled_with_no_checks(rig, runner),
        Some(StepReport::Conflicted { .. })
    ));
    rig.claude
        .script([Scripted::AbortAndPush("extra.txt", "extra\n")]);
    step(runner).unwrap();
    let report = settled_with_no_checks(rig, runner);
    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
}

#[test]
fn a_rebase_yes_raised_mid_review_goes_back_to_that_review() {
    let (rig, runner, _) = pushed();
    parked_on_a_rebase_from_review(&rig, &runner);

    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(state(&rig, &runner), "review", "the review it left");
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(rig.verdict(&runner).is_some());
    assert_eq!(reads(&rig), 1, "the first reviewer reads the fixed head");
}

#[test]
fn a_rebase_yes_raised_mid_review_under_auto_is_read_before_any_merge() {
    let (rig, runner, _) = pushed();
    drop(runner);
    rig.merge_auto();
    let runner = rig.open().unwrap();
    parked_on_a_rebase_from_review(&rig, &runner);

    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(state(&rig, &runner), "review", "the review it left");
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    rig.claude.script([Scripted::Text(CLEAN)]);
    let mut reports = Vec::new();
    for _ in 0..6 {
        let report = rig.verdict(&runner);
        assert!(
            !matches!(report, Some(StepReport::Finished { .. })) || reads(&rig) == 2,
            "the merge waited for both reviewers: {report:?}"
        );
        reports.push(report);
    }
    assert_eq!(reads(&rig), 2, "{reports:?}");
}

#[test]
fn a_review_head_behind_main_whose_checks_report_is_not_rebased() {
    let (rig, runner, head) = pushed();
    rig.land_on_origin("landed.txt");
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));

    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None, "a pending run waits");
    assert_eq!(rig.forge.head_of("kelpie/7"), Some(head));
    assert_eq!(state(&rig, &runner), "review");
    assert_eq!(reads(&rig), 0);
}

#[test]
fn status_shows_a_ci_wait_with_no_checks_reported() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::None);
    let checks = |rig: &Rig| {
        let status = rig.ask(&runner, "status", None);
        status["work_item"]["checks"].clone()
    };
    assert_eq!(checks(&rig), serde_json::Value::Null, "not yet looked at");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(checks(&rig), "none");
    let status = rig.ask(&runner, "status", None);
    assert!(status["work_item"]["phase"]["ci"]["since"].is_number());

    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(checks(&rig), serde_json::Value::Null, "a check reports");
}

const CAP: u64 = 60 * 60;

#[test]
fn a_review_head_with_no_checks_and_no_pull_request_workflow_is_read_at_once() {
    let (rig, runner, head) = pushed_set(Rig::without_pull_request_ci);
    rig.forge.set_checks(&head, Checks::None);

    assert_eq!(step(&runner).unwrap(), None, "the checks settle first");
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    assert_eq!(reads(&rig), 1, "the first reviewer read the head");
    assert_ne!(state(&rig, &runner), "ruling");
}

#[test]
fn a_review_head_with_no_checks_past_the_cap_raises_a_no_checks_ruling() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::None);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CAP - 1);
    assert_eq!(step(&runner).unwrap(), None, "within the cap");
    assert_eq!(reads(&rig), 0);

    rig.clock.advance(1);
    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    let kind = &status["rulings"][0]["kind"];
    assert_eq!(kind["reason"], "no-checks");
    assert_eq!(kind["head"], head);
    assert_eq!(kind["review"]["round"], 1, "the round it interrupted");
    assert_eq!(state(&rig, &runner), "ruling");
    assert_eq!(reads(&rig), 0);
}

#[test]
fn a_yes_on_a_review_no_checks_ruling_reads_the_head_as_skipped() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::None);
    step(&runner).unwrap();
    rig.clock.advance(CAP);
    step(&runner).unwrap();

    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(state(&rig, &runner), "review", "back to the round it left");
    rig.verdict(&runner);

    assert_eq!(reads(&rig), 1, "the first reviewer read the head");
}

#[test]
fn a_no_on_a_review_no_checks_ruling_waits_another_cap() {
    let (rig, runner, head) = pushed();
    rig.forge.set_checks(&head, Checks::None);
    step(&runner).unwrap();
    rig.clock.advance(CAP);
    step(&runner).unwrap();

    rig.ask(&runner, "rule", Some("71 no still starting"));
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CAP - 1);
    assert_eq!(step(&runner).unwrap(), None, "the cap starts from the no");
    assert_eq!(reads(&rig), 0);

    rig.clock.advance(1);
    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
}
