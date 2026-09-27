//! CodeRabbit rounds through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::{ANSWER_WAIT, LABEL, REVIEW_WAIT};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Checks, Role};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told, git};

const HOLDS: &str = r#"{"holds": true, "severity": "medium", "reason": "real"}"#;
const REJECTED: &str = r#"{"holds": false, "severity": "low", "reason": "not so"}"#;

fn cr() -> LeaseKind {
    LeaseKind::coderabbit()
}

// A running project with CodeRabbit on, whose worker opened pull request
// 71 and whose qwen-review loop settled. CI has not reported.
fn reviewed_by_qwen(project: &str) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.coderabbit_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").unwrap();
    (rig, runner, head)
}

// Green CI on the head, and the summon that follows it.
fn summoned(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            issue: 7,
            pull_request: 71,
            head: head.clone(),
        })
    );
    (rig, runner, head)
}

fn now(rig: &Rig) -> u64 {
    use crate::ports::Clock;
    rig.clock.now().0
}

fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    rig.forge.coderabbit.label_log()
}

fn on() -> (u64, String, bool) {
    (71, LABEL.to_owned(), true)
}

fn off() -> (u64, String, bool) {
    (71, LABEL.to_owned(), false)
}

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

#[test]
fn with_the_gate_off_green_ci_goes_straight_to_the_merge_ruling_and_no_lease_is_asked() {
    let (rig, runner, head) = Rig::with_pull_request("hazels-lab");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(rig.leases.told(), []);
    assert_eq!(labels(&rig), []);
}

#[test]
fn nothing_summons_without_green_ci() {
    let (rig, runner, head) = reviewed_by_qwen("shep");
    for checks in [Checks::Pending, Checks::None] {
        rig.forge.set_checks(&head, checks);
        rig.clock.advance(3600);
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(rig.leases.told(), []);
    assert_eq!(labels(&rig), []);
}

#[test]
fn nothing_summons_without_the_lease() {
    let (rig, runner, head) = reviewed_by_qwen("shep");
    rig.leases.withhold(true);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(rig.verdict(&runner), None);
    assert_eq!(step(&runner).unwrap(), None, "asks again, still no grant");
    assert_eq!(rig.leases.told(), [Told::Want(cr()), Told::Want(cr())]);
    assert_eq!(labels(&rig), []);
    assert_eq!(
        phase(&rig, &runner),
        json!({ "state": "coderabbit", "stage": "lease", "head": head })
    );

    rig.leases.withhold(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [on()]);
    assert_eq!(
        rig.ask(&runner, "status", None)["leases"],
        json!([{ "resource": "coderabbit", "since": now(&rig) }])
    );
}

// A review of another commit, a limit block that quotes the head, and a
// clean walkthrough of the head: only the last is the round.
#[test]
fn a_round_counts_only_once_a_review_covers_the_head() {
    let (rig, runner, head) = summoned("shep");
    let summon = now(&rig);
    let cr_bot = &rig.forge.coderabbit;
    cr_bot.review(71, "0ldc0mm1t", summon + 60, &[]);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.leases.held(&cr()), "no answer yet, so the lease stays");

    cr_bot.start(71, summon + 120);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(
        !rig.leases.held(&cr()),
        "a review running is an accepted summon"
    );
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summon)),
        "the hour runs from the summon, not from when it was seen"
    );
    assert_eq!(
        labels(&rig),
        [on()],
        "the label stays until the review lands"
    );

    cr_bot.review(71, &head, summon + 1500, &[]);
    rig.clock.advance(1400);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied {
            issue: 7,
            pull_request: 71,
            rounds: 1
        })
    );
    assert_eq!(labels(&rig), [on(), off()]);
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Quota, 1)),
        "the footer's quota reaches the dog"
    );
}

#[test]
fn once_coderabbit_is_satisfied_the_work_item_goes_on_to_the_merge_ruling() {
    let (rig, runner, head) = summoned("golbat");
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    step(&runner).unwrap();
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": head })
    );
    assert_eq!(
        status["work_item"]["coderabbit"],
        json!({ "rounds": 1, "cap_cleared": false, "satisfied": true })
    );
    assert_eq!(labels(&rig), [on(), off()], "no second summon");
}

#[test]
fn a_head_already_reviewed_is_not_summoned_again() {
    let (rig, runner, head) = reviewed_by_qwen("koji");
    rig.forge.coderabbit.review(71, &head, now(&rig), &[]);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CodeRabbitSatisfied { rounds: 1, .. })
    ));
    assert_eq!(labels(&rig), []);
    assert_eq!(rig.leases.told(), [Told::Window(WindowFact::Quota, 1)]);
}

#[test]
fn a_refusal_gives_the_lease_back_with_its_quoted_wait_and_the_label_comes_off() {
    let (rig, runner, head) = summoned("reactmap");
    let refused_at = now(&rig) + 20;
    rig.forge.coderabbit.refuse(71, refused_at, 12);
    rig.clock.advance(30);
    rig.leases.withhold(true);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused {
            issue: 7,
            pull_request: 71,
            opens: crate::ports::Timestamp(refused_at + 12 * 60),
        })
    );
    let told = rig.leases.told();
    assert!(told.contains(&Told::Window(WindowFact::Opens, refused_at + 12 * 60)));
    assert_eq!(told.last(), Some(&Told::Return(cr())));
    assert_eq!(labels(&rig), [on(), off()]);
    assert_eq!(rig.ask(&runner, "status", None)["leases"], json!([]));

    // The label goes back on only to summon again, once the dog grants.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(labels(&rig), [on(), off()]);
    rig.leases.withhold(false);
    rig.clock.advance(720);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 7,
            pull_request: 71,
            head
        })
    );
    assert_eq!(labels(&rig), [on(), off(), on()]);
}

#[test]
fn a_summon_nobody_answers_is_counted_spent_and_then_asked_about() {
    let (rig, runner, head) = summoned("zeus");
    let summon = now(&rig);
    rig.clock.advance(ANSWER_WAIT - 1);
    step(&runner).unwrap();
    assert!(rig.leases.held(&cr()));
    rig.clock.advance(1);
    step(&runner).unwrap();
    assert!(!rig.leases.held(&cr()));
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summon))
    );

    rig.clock.advance(REVIEW_WAIT - ANSWER_WAIT);
    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("no ruling on a review that never ran");
    };
    assert!(
        question.starts_with(&format!(
            "CodeRabbit never reviewed pull request #71 at {} after kelpie summoned it.",
            &head[..7]
        )),
        "{question}"
    );
    assert_eq!(labels(&rig), [on(), off()]);

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
}

#[test]
fn the_judge_reads_every_open_thread_rejected_ones_are_resolved_and_held_ones_go_to_the_worker() {
    let (rig, runner, head) = summoned("shep");
    rig.forge.coderabbit.review(
        71,
        &head,
        now(&rig) + 60,
        &["Name the flag.", "Guard the index."],
    );
    rig.clock.advance(60);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            issue: 7,
            pull_request: 71,
            round: 1,
            open_threads: 2
        })
    );
    rig.claude
        .script([Scripted::Text(HOLDS), Scripted::Text(REJECTED)]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let judged: Vec<_> = rig
        .claude
        .all_calls()
        .into_iter()
        .filter(|c| c.role == Role::Judge)
        .collect();
    assert_eq!(judged.len(), 2);
    assert!(judged[0].prompt.contains("what: Name the flag."));
    assert!(judged[1].prompt.contains("what: Guard the index."));

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitJudged {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 1,
            resolved: 1
        })
    );
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_71_1"]);

    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("CodeRabbit round 1 on your pull request #71 left 1 finding(s) that hold"),
        "{}",
        fix.prompt
    );
    let file = std::fs::read_to_string(rig.paths().worker.join("review-findings.md")).unwrap();
    assert!(file.contains("Name the flag."), "{file}");
    assert!(
        !file.contains("Guard the index."),
        "the worker never sees it"
    );

    // The fix goes through CI, then round two, whose review CodeRabbit
    // wrote after seeing the fix and resolving its own thread.
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(step(&runner).unwrap(), None, "CI on the fix is pending");
    assert_eq!(labels(&rig), [on(), off()]);
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    rig.forge.coderabbit.settle("PRRT_71_0");
    rig.forge.coderabbit.review(71, &fixed, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied {
            issue: 7,
            pull_request: 71,
            rounds: 2
        })
    );
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
}

#[test]
fn a_thread_the_judge_holds_nothing_on_leaves_coderabbit_satisfied() {
    let (rig, runner, head) = summoned("rotom");
    rig.forge
        .coderabbit
        .review(71, &head, now(&rig) + 60, &["Nothing real."]);
    rig.clock.advance(60);
    step(&runner).unwrap();
    rig.claude.script([Scripted::Text(REJECTED)]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { rounds: 1, .. })
    ));
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_71_0"]);
    assert_eq!(rig.claude.calls().len(), 1, "the worker took no turn");
}

// Round one's fix, CI, and round two, each holding a finding.
fn hold_a_finding(
    rig: &Rig,
    runner: &Mutex<Runner>,
    head: &str,
    title: &str,
) -> Option<StepReport> {
    rig.forge
        .coderabbit
        .review(71, head, now(rig) + 60, &[title]);
    rig.clock.advance(60);
    step(runner).unwrap();
    rig.claude.script([Scripted::Text(HOLDS)]);
    step(runner).unwrap();
    step(runner).unwrap()
}

// The worker's fix: a new commit, CI green on it, and the next summon.
fn fixed(rig: &Rig, runner: &Mutex<Runner>, file: &'static str) -> String {
    rig.claude.script([Scripted::Push(file, "fixed\n")]);
    step(runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::Summoned { .. })
    ));
    head
}

#[test]
fn the_cap_leaves_generated_files_out_and_parks_the_worker_with_findings_open() {
    let (rig, runner, _) = reviewed_by_qwen("shep");
    rig.edit_settings(|s| s.replace("divisor = 1000", "divisor = 2"));
    drop(runner);
    let runner = rig.open().unwrap();
    // Five lockfile lines would lift the cap to five rounds if counted.
    let worktree = rig.worktree_7();
    std::fs::write(worktree.join("Cargo.lock"), "a\nb\nc\nd\ne\n").unwrap();
    git(&worktree, &["add", "Cargo.lock"]);
    git(&worktree, &["commit", "--quiet", "-m", "lock"]);
    git(&worktree, &["push", "--quiet", "origin", "HEAD"]);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));

    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "First."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    // Two changed lines by round two: ceil(2 / 2) + 1 = 2 rounds.
    let head = fixed(&rig, &runner, "one.txt");
    rig.forge.coderabbit.settle("PRRT_71_0");
    let Some(StepReport::Ruling { id, question, .. }) =
        hold_a_finding(&rig, &runner, &head, "Second.")
    else {
        panic!("the cap did not park the worker");
    };
    assert!(
        question.starts_with(
            "CodeRabbit has run 2 rounds on pull request #71, its cap, and the judge \
             still holds 1 of its findings."
        ),
        "{question}"
    );
    let turns = rig.claude.calls().len();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls().len(), turns, "a parked worker waits");

    // A yes sends the held findings, and the cap no longer parks.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    let head = fixed(&rig, &runner, "two.txt");
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("CodeRabbit round 2 on your pull request #71")
    );
    rig.forge.coderabbit.settle("PRRT_71_1");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Third."),
        Some(StepReport::CodeRabbitJudged { round: 3, .. })
    ));
}

#[test]
fn a_restart_mid_summon_reads_on_and_holds_no_stale_lease() {
    let (rig, runner, head) = summoned("xilriws");
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "status", None)["leases"], json!([]));
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
    assert_eq!(labels(&rig), [on(), off()]);
}

#[test]
fn dropping_a_work_item_mid_round_returns_the_lease_and_takes_the_label_off() {
    let (rig, runner, _) = summoned("chelone");
    rig.ask(&runner, "drop", None);
    assert!(!rig.leases.held(&cr()));
    assert_eq!(labels(&rig), [on(), off()]);
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_pull_request_merged_by_hand_mid_round_ends_the_work_item() {
    let (rig, runner, _) = summoned("golbat");
    rig.forge
        .set_state(71, crate::ports::PullRequestState::Merged);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { merged: true, .. })
    ));
    assert!(!rig.leases.held(&cr()));
}

#[test]
fn coderabbit_that_cannot_be_read_is_tried_again_later() {
    let (rig, runner, head) = summoned("koji");
    rig.forge.coderabbit.set_down(true);
    let Some(StepReport::GateFailed { reason, .. }) = step(&runner).unwrap() else {
        panic!("a failed read was not reported");
    };
    assert!(
        reason.starts_with("cannot read CodeRabbit on #71: "),
        "{reason}"
    );
    rig.forge.coderabbit.set_down(false);
    rig.forge.coderabbit.review(71, &head, now(&rig), &[]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
}
