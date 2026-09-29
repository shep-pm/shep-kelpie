//! CodeRabbit rounds through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::{ANSWER_WAIT, HEARD_WAIT, LABEL, REVIEW_WAIT};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Checks, ClaudeError, Cost, Role};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told, git};

mod again;
mod budget;
mod full;

const HOLDS: &str = r#"{"holds": true, "severity": "medium", "reason": "real"}"#;
const REJECTED: &str = r#"{"holds": false, "severity": "low", "reason": "not so"}"#;

fn cr() -> LeaseKind {
    LeaseKind::coderabbit()
}

// A running project with CodeRabbit on, whose worker opened pull request
// 71 and whose qwen-review loop settled. CI has not reported.
pub(in crate::runner) fn reviewed_by_qwen(project: &str) -> (Rig, Mutex<Runner>, String) {
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

// Green CI on the head, the pass that marks the draft ready, and the summon
// that follows it.
pub(in crate::runner) fn summoned(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady {
            issue: 7,
            pull_request: 71,
        })
    );
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

pub(in crate::runner) fn now(rig: &Rig) -> u64 {
    use crate::ports::Clock;
    rig.clock.now().0
}

// Kelpie's changes to the summon label, leaving out the triage labels
fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    let log = rig.forge.coderabbit.label_log();
    log.into_iter().filter(|(_, l, _)| l == LABEL).collect()
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
    assert!(
        rig.forge.readied().is_empty(),
        "ready waits for the merge ruling"
    );
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
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady {
            issue: 7,
            pull_request: 71,
        })
    );
    assert_eq!(step(&runner).unwrap(), None, "asks, no grant");
    assert_eq!(step(&runner).unwrap(), None, "asks again, still no grant");
    assert_eq!(rig.leases.told(), [Told::Want(cr()), Told::Want(cr())]);
    assert_eq!(labels(&rig), []);
    assert_eq!(
        phase(&rig, &runner),
        json!({ "state": "coderabbit", "stage": "lease", "head": head, "readied": now(&rig) })
    );

    rig.leases.withhold(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [on()]);
    assert_eq!(
        rig.ask(&runner, "status", None)["leases"],
        json!([{ "resource": "coderabbit", "issue": 7, "since": now(&rig) }])
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
            .contains(&Told::Window(WindowFact::Quota(1), summon + 1500)),
        "the footer's quota reaches the dog, with when it was posted"
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
fn a_draft_is_marked_ready_before_the_label_goes_on_and_the_summon_waits_a_pass() {
    let (rig, runner, head) = reviewed_by_qwen("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady {
            issue: 7,
            pull_request: 71,
        })
    );
    assert_eq!(rig.forge.readied(), [71]);
    assert_eq!(labels(&rig), [], "no label in the pass that marks ready");
    assert_eq!(rig.leases.told(), []);

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [on()]);
    assert_eq!(rig.forge.readied(), [71], "marked once");
    assert!(
        rig.forge.skipped_as_drafts().is_empty(),
        "no draft was summoned"
    );
}

// The forge can read a pull request as a draft for a few seconds after it is
// marked ready. The pass that marked it ends the step, and the runner's next
// step must neither mark it again nor summon a draft.
fn marked_while_the_forge_lags() -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = reviewed_by_qwen("shep");
    rig.forge.set_lagging_draft(71, true);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    (rig, runner)
}

#[test]
fn a_forge_still_reading_draft_is_neither_marked_again_nor_summoned() {
    let (rig, runner) = marked_while_the_forge_lags();
    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None);
    }
    rig.clock.advance(CHECKS_SETTLE - 1);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.readied(), [71], "marked once");
    assert!(labels(&rig).is_empty(), "nothing summoned yet");
    assert_eq!(rig.leases.told(), []);

    // Once the forge reads it ready, the summon needs no more waiting.
    rig.forge.set_lagging_draft(71, false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [on()]);
    assert_eq!(rig.forge.readied(), [71]);
    assert!(rig.forge.skipped_as_drafts().is_empty());
}

#[test]
fn a_forge_reading_draft_past_the_settle_is_marked_again_not_summoned() {
    let (rig, runner) = marked_while_the_forge_lags();
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(rig.forge.readied(), [71, 71]);
    assert!(labels(&rig).is_empty());
    assert_eq!(step(&runner).unwrap(), None, "a new settle starts");
    assert_eq!(rig.forge.readied(), [71, 71]);
}

#[test]
fn a_restart_during_the_settle_does_not_mark_again() {
    let (rig, runner) = marked_while_the_forge_lags();
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.readied(), [71]);
}

#[test]
fn a_rework_on_a_ready_pull_request_leaves_it_ready_and_summons_at_once() {
    let (rig, runner, head) = summoned("shep");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    fixed(&rig, &runner, "flag.txt");
    assert_eq!(rig.forge.readied(), [71], "marked once, before round one");
    assert!(rig.forge.skipped_as_drafts().is_empty());
    assert_eq!(labels(&rig), [on(), off(), on()]);
}

#[test]
fn a_head_already_reviewed_is_not_summoned_again() {
    let (rig, runner, head) = reviewed_by_qwen("koji");
    let reviewed_at = now(&rig);
    rig.forge.coderabbit.review(71, &head, reviewed_at, &[]);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CodeRabbitSatisfied { rounds: 1, .. })
    ));
    assert_eq!(labels(&rig), []);
    assert_eq!(
        rig.leases.told(),
        [Told::Window(WindowFact::Quota(1), reviewed_at)]
    );
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
    rig.clock.advance(ANSWER_WAIT);
    step(&runner).unwrap();
    assert!(rig.leases.held(&cr()), "held for the re-send");
    rig.clock.advance(HEARD_WAIT - ANSWER_WAIT);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SummonedAgain { .. })
    ));
    step(&runner).unwrap();
    assert!(!rig.leases.held(&cr()));
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summon))
    );

    rig.clock.advance(REVIEW_WAIT - HEARD_WAIT);
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
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);

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
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("Name the flag."), "{file}");
    assert!(
        !file.contains("Guard the index."),
        "the worker never sees it"
    );

    // The fix goes through CI, then round two, whose review CodeRabbit
    // wrote after seeing the fix and resolving its own thread.
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: 7,
            pull_request: 71,
            round: 1,
            head: Some(fixed.clone()),
        })
    );
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
pub(in crate::runner) fn hold_a_finding(
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
pub(in crate::runner) fn fixed(rig: &Rig, runner: &Mutex<Runner>, file: &'static str) -> String {
    rig.claude.script([Scripted::Push(file, "fixed\n")]);
    step(runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    assert!(matches!(
        step(runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::Summoned { .. })
    ));
    head
}

#[test]
fn a_fix_turn_that_pushes_nothing_parks_instead_of_opening_round_two() {
    let (rig, runner, head) = summoned("shep");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    rig.claude
        .script([Scripted::Say("I can't push from this sandbox.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("a fix with nothing pushed raised no ruling");
    };
    assert!(
        question.starts_with(
            "The worker on pull request #71 ended its fix for CodeRabbit round 1 \
             without pushing, so those findings still hold."
        ),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    let path = rig.build_7().join("review-findings.md");
    let again = format!(
        "Your last turn on pull request #71 pushed nothing, so round 1's \
         findings in {} still hold. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    );
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({
            "kind": "fix-not-pushed",
            "coderabbit": { "round": 1, "head": head },
            "prompt": again,
        })
    );
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    assert_eq!(step(&runner).unwrap(), None, "parked, not summoning");
    assert_eq!(labels(&rig), [on(), off()], "no second summon");

    // A yes sends the same findings, and a fix that pushes goes on to CI.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().pop().unwrap().prompt, again);
    let fixed = rig.forge.head_of("kelpie/7");
    assert_ne!(fixed.as_deref(), Some(head.as_str()));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: 7,
            pull_request: 71,
            round: 1,
            head: fixed,
        })
    );
    assert_eq!(phase(&rig, &runner)["state"], "ci");
}

#[test]
fn a_timed_out_fix_turn_resumes_that_round() {
    let (rig, runner, head) = summoned("mew");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    rig.claude.script([Scripted::Fail(ClaudeError::TimedOut)]);
    let Some(StepReport::TimedOut { id, .. }) = step(&runner).unwrap() else {
        panic!("the fix turn did not time out");
    };
    step(&runner).unwrap(); // the alert

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Say("Still thinking.")]);
    step(&runner).unwrap(); // the resumed fix turn ends with nothing pushed
    let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
        panic!("the resumed fix was not checked for a push");
    };
    assert!(
        question.contains("CodeRabbit round 1 without pushing"),
        "{question}"
    );
}

#[test]
fn a_question_during_a_fix_turn_resumes_that_round() {
    let (rig, runner, head) = summoned("koji");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    let rounds = rig.reviewer.seen().len();
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nShould it be `--dry-run`?\n</kelpie-question>\n",
    )]);
    let Some(StepReport::Asked { id, .. }) = step(&runner).unwrap() else {
        panic!("the fix turn's question raised no ruling");
    };
    step(&runner).unwrap(); // the alert

    rig.ask(
        &runner,
        "rule",
        Some(&format!("{id} answer yes, --dry-run")),
    );
    rig.claude.script([Scripted::Say("Done, I think.")]);
    step(&runner).unwrap(); // the answered fix turn ends with nothing pushed
    let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
        panic!("the answered fix was not checked for a push");
    };
    assert!(
        question.contains("CodeRabbit round 1 without pushing"),
        "{question}"
    );
    assert_eq!(
        rig.reviewer.seen().len(),
        rounds,
        "the qwen-review loop never restarted"
    );
}

#[test]
fn a_fix_past_the_cap_that_pushes_nothing_still_parks() {
    // A few changed lines under the default divisor: a cap of two rounds.
    let (rig, runner, head) = summoned("rotom");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "First."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    let head = fixed(&rig, &runner, "one.txt");
    rig.forge.coderabbit.settle("PRRT_71_0");
    let Some(StepReport::Ruling { id, .. }) = hold_a_finding(&rig, &runner, &head, "Second.")
    else {
        panic!("round two did not reach the cap");
    };
    step(&runner).unwrap(); // the alert
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Say("Nothing to change.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
        panic!("a fix past the cap with nothing pushed raised no ruling");
    };
    assert!(
        question.contains("CodeRabbit round 2 without pushing"),
        "{question}"
    );
}

#[test]
fn the_cap_leaves_generated_files_out_and_parks_the_worker_with_findings_open() {
    let (rig, runner, _) = reviewed_by_qwen("shep");
    // At the default divisor the lockfile would not move the cap either.
    rig.edit_settings(|s| {
        assert!(s.contains("divisor = 1000"), "the default divisor moved");
        s.replace("divisor = 1000", "divisor = 2")
    });
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
    // Not the worker's push, so it waits on a yes and a clean review loop.
    let Some(StepReport::Ruling { id, .. }) = step(&runner).unwrap() else {
        panic!("the lockfile commit did not park the worker");
    };
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
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

// The label went on, then the runner died before the summon was saved: on
// disk the round still waits for its lease.
#[test]
fn a_restart_between_the_label_and_its_save_does_not_summon_twice() {
    let (rig, runner, head) = summoned("rotom");
    drop(runner);
    let state = rig.paths().state;
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    saved["work_items"][0]["phase"] =
        json!({ "state": "coderabbit", "stage": "lease", "head": head });
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 7,
            pull_request: 71,
            head: head.clone()
        })
    );
    assert_eq!(labels(&rig), [on()], "the label was not toggled again");
    assert_eq!(phase(&rig, &runner)["stage"], "summoned");
}

#[test]
fn a_label_someone_else_left_on_is_toggled_to_summon() {
    let (rig, runner, head) = reviewed_by_qwen("zeus");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.leases.withhold(true);
    rig.verdict(&runner);
    rig.forge.label_pull_request(71, LABEL);
    rig.leases.withhold(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [off(), on()]);
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
fn a_round_with_a_held_finding_records_a_judge_call_that_the_finished_totals_carry() {
    let (rig, runner, head) = summoned("shep");
    rig.forge
        .coderabbit
        .review(71, &head, now(&rig) + 60, &["Name the flag."]);
    rig.clock.advance(60);
    step(&runner).unwrap(); // the review covers the head
    rig.claude
        .script([Scripted::Billed(HOLDS, Cost(12_000_000))]);
    step(&runner).unwrap(); // the judge holds the finding

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["by_role"]["judge"],
        json!({ "calls": 1, "cost_usd": 0.012 })
    );

    rig.forge
        .set_state(71, crate::ports::PullRequestState::Merged);
    let Some(StepReport::Finished { spend, .. }) = step(&runner).unwrap() else {
        panic!("the merged work item did not finish");
    };
    assert_eq!(spend.judge.calls, 1);
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

// The round a hand push lands after: CodeRabbit read `head` and left nothing.
fn satisfied(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = summoned(project);
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
    (rig, runner, head)
}

#[test]
fn a_commit_pushed_by_hand_after_the_round_parks_rather_than_reaching_the_merge_ruling() {
    let (rig, runner, _) = satisfied("koji");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    rig.forge.set_checks(&by_hand, Checks::Passed);
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("the commit pushed by hand parked nothing");
    };
    assert_eq!(
        question,
        format!(
            "Pull request #71 changed outside kelpie: its head moved to {}, a commit \
             the worker did not push. `shep trigger koji rule '{id} yes'` accepts it \
             and kelpie carries on, and `shep trigger koji rule '{id} no <note>'` \
             sends the worker your note.",
            &by_hand[..7]
        )
    );
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None, "a parked worker waits");
    assert_eq!(rig.forge.merges(), []);
}

#[test]
fn a_yes_on_a_commit_pushed_by_hand_runs_the_review_loop_and_coderabbit_on_it() {
    let (rig, runner, _) = satisfied("zeus");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    let Some(StepReport::Ruling { id, .. }) = rig.verdict(&runner) else {
        panic!("the commit pushed by hand parked nothing");
    };
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(git(&rig.worktree_7(), &["rev-parse", "HEAD"]), by_hand);

    let reviewed = rig.reviewer.seen().len();
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    assert_eq!(rig.reviewer.seen().len(), reviewed + 1);
    let claude = rig.claude.all_calls().pop().unwrap();
    assert!(claude.prompt.contains("by-hand.txt"), "{}", claude.prompt);

    rig.forge.set_checks(&by_hand, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            issue: 7,
            pull_request: 71,
            head: by_hand,
        })
    );
}
