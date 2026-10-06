//! CodeRabbit's round, in its place in the review pass, through the
//! runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::{ANSWER_WAIT, FAR, HEARD_WAIT, LABEL, REVIEW_WAIT};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{AgentError, Checks, Timestamp};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::settings::AgentName;
use crate::test::{Rig, Scripted, Told, git};

mod again;
mod budget;
mod full;
mod skips;
mod threads;

fn cr() -> LeaseKind {
    LeaseKind::coderabbit()
}

fn coderabbit() -> AgentName {
    AgentName::kelpies("coderabbit")
}

// A running project that lists CodeRabbit after its qwen and Claude rounds,
// whose worker opened pull request 71, and whose qwen and Claude rounds read
// it clean. CodeRabbit's round, the pass's third, is next.
pub(in crate::runner) fn reviewed_by_qwen(project: &str) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.coderabbit_on();
    let runner = rig.open().unwrap();
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

// CodeRabbit's round: the pass that marks the draft ready, and the summon
// that follows it.
pub(in crate::runner) fn summoned(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady {
            issue: 7,
            pull_request: 71,
        })
    );
    assert_eq!(
        step(&runner).unwrap(),
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

fn reviewed(open_threads: usize) -> Option<StepReport> {
    Some(StepReport::BotReviewed {
        issue: 7,
        pull_request: 71,
        round: 3,
        reviewer: coderabbit(),
        open_threads,
    })
}

#[test]
fn a_pass_with_no_bot_listed_never_touches_the_forge_for_a_review() {
    let (rig, runner, head) = Rig::with_pull_request("webapp");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(rig.leases.told(), []);
    assert_eq!(labels(&rig), []);
    assert_eq!(rig.forge.coderabbit.logins(), Vec::<String>::new());
    assert!(
        rig.forge.readied().is_empty(),
        "ready waits for the merge ruling"
    );
}

#[test]
fn nothing_summons_without_the_lease() {
    let (rig, runner, head) = reviewed_by_qwen("shep");
    rig.leases.withhold(true);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    let readied = now(&rig);
    assert_eq!(step(&runner).unwrap(), None, "asks, no grant");
    assert_eq!(step(&runner).unwrap(), None, "asks again, still no grant");
    assert_eq!(rig.leases.told(), [Told::Want(cr()), Told::Want(cr())]);
    assert_eq!(labels(&rig), []);
    assert_eq!(
        phase(&rig, &runner),
        json!({
            "state": "review",
            "round": 3,
            "stage": {
                "stage": "summon", "bot": "coderabbit", "started": readied, "head": head,
                "readied": readied,
            },
            "reviewer": "coderabbit",
            "ran": ["qwen", "claude"],
        })
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
// clean walkthrough of the head: only the last is the read.
#[test]
fn a_read_counts_only_once_a_review_covers_the_head() {
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
    assert_eq!(step(&runner).unwrap(), reviewed(0));
    assert_eq!(labels(&rig), [on(), off()]);
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Quota(1), summon + 1500)),
        "the footer's quota reaches the dog, with when it was posted"
    );
}

#[test]
fn a_clean_read_ends_the_pass_and_ci_goes_on_to_the_merge_ruling() {
    let (rig, runner, head) = summoned("golbat");
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), reviewed(0));
    assert_eq!(phase(&rig, &runner)["state"], "ci");
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        state["work_items"][0]["counts"]["review_rounds"], 3,
        "qwen's, claude's and the bot's"
    );
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": head })
    );
    assert_eq!(status["work_item"]["bot_reads"], json!({ "coderabbit": 1 }));
    assert_eq!(labels(&rig), [on(), off()], "no second summon");
}

#[test]
fn a_draft_is_marked_ready_before_the_label_goes_on_and_the_summon_waits_a_pass() {
    let (rig, runner, _) = reviewed_by_qwen("shep");
    assert_eq!(
        step(&runner).unwrap(),
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
    let (rig, runner, _) = reviewed_by_qwen("shep");
    rig.forge.set_lagging_draft(71, true);
    assert!(matches!(
        step(&runner).unwrap(),
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

// A merge ruling's no sends the worker back, and its fix gets a pass of its own.
#[test]
fn a_later_pass_on_a_ready_pull_request_summons_at_once() {
    let (rig, runner, head) = read_clean("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.ask(&runner, "rule", Some("1 no name the flag"));
    rig.claude.script([
        Scripted::Push("flag.txt", "named\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the noted turn: pushes, and a pass begins
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 7,
            pull_request: 71,
            head: fixed,
        })
    );
    assert_eq!(rig.forge.readied(), [71], "marked once, in the first pass");
    assert!(rig.forge.skipped_as_drafts().is_empty());
    assert_eq!(labels(&rig), [on(), off(), on()]);
}

#[test]
fn a_head_already_reviewed_is_not_summoned_again() {
    let (rig, runner, head) = reviewed_by_qwen("koji");
    let reviewed_at = now(&rig);
    rig.forge.coderabbit.review(71, &head, reviewed_at, &[]);
    assert_eq!(step(&runner).unwrap(), reviewed(0));
    assert_eq!(labels(&rig), []);
    assert!(
        rig.forge.readied().is_empty(),
        "no summon, so no mark either"
    );
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
            opens: Timestamp(refused_at + 12 * 60),
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

fn skipped(reason: &str) -> Option<StepReport> {
    Some(StepReport::ReviewerSkipped {
        issue: 7,
        pull_request: 71,
        round: 3,
        reviewer: coderabbit(),
        reason: reason.to_owned(),
    })
}

const FAR_OFF: &str = "its window opens more than an hour on";

#[test]
fn a_refusal_that_opens_more_than_an_hour_on_passes_the_bot_over_for_the_pass() {
    let (rig, runner, head) = summoned("reactmap");
    let refused_at = now(&rig) + 20;
    rig.forge.coderabbit.refuse(71, refused_at, 61);
    rig.clock.advance(30);
    assert_eq!(step(&runner).unwrap(), skipped(FAR_OFF));
    let opens = refused_at + 61 * 60;
    let told = rig.leases.told();
    assert!(told.contains(&Told::Window(WindowFact::Opens, opens)));
    assert_eq!(told.last(), Some(&Told::Return(cr())));
    assert_eq!(labels(&rig), [on(), off()]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["bots_skipped"],
        json!([{ "why": "window", "reviewer": "coderabbit", "opens": opens }])
    );
    assert_eq!(status["work_item"]["phase"]["state"], "ci");

    // qwen and claude read it, so the merge ruling marks nothing unread.
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": head })
    );
}

#[test]
fn a_window_the_dogs_book_opens_more_than_an_hour_on_is_skipped_unasked() {
    let (rig, runner, _) = reviewed_by_qwen("koji");
    rig.leases.opens_at(&cr(), Timestamp(now(&rig) + FAR + 1));
    assert_eq!(step(&runner).unwrap(), skipped(FAR_OFF));
    assert_eq!(rig.leases.told(), [], "no lease asked for");
    assert_eq!(labels(&rig), []);
    assert!(rig.forge.readied().is_empty(), "nothing to mark ready for");
    assert_eq!(phase(&rig, &runner)["state"], "ci");
}

#[test]
fn a_window_that_opens_within_the_hour_is_waited_for() {
    let (rig, runner, _) = reviewed_by_qwen("koji");
    rig.leases.opens_at(&cr(), Timestamp(now(&rig) + FAR));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
}

#[test]
fn a_summon_nobody_answers_is_counted_spent_and_then_passed_over() {
    let (rig, runner, head) = summoned("acme");
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
    let silent = format!(
        "it never reviewed {} in the two hours after its summon",
        &head[..7]
    );
    assert_eq!(step(&runner).unwrap(), skipped(&silent));
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["bots_skipped"],
        json!([{ "why": "silent", "reviewer": "coderabbit", "head": head }])
    );
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(
        status["rulings"],
        json!([]),
        "nothing waits on the maintainer"
    );
}

// With nobody else to read it, a bot that never answers leaves the pass
// unread, which the merge ruling names.
#[test]
fn a_bot_alone_that_never_answers_leaves_the_pass_unreviewed() {
    let rig = Rig::new("acme");
    rig.reviewers(&["coderabbit"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.coderabbit.start(71, now(&rig) + 20);
    rig.clock.advance(REVIEW_WAIT);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 1, .. })
    ));
    let why = format!(
        "coderabbit was passed over: it never reviewed {} in the two hours after its summon",
        &head[..7]
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Unreviewed {
            issue: 7,
            pull_request: 71,
            reason: why.clone(),
        })
    );
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        question.contains(&format!("No reviewer read it in its last review: {why}.")),
        "{question}"
    );
}

#[test]
fn every_open_thread_goes_to_the_worker_and_is_resolved_once_its_fix_moves_the_head() {
    let (rig, runner, head) = summoned("shep");
    rig.forge.coderabbit.review(
        71,
        &head,
        now(&rig) + 60,
        &["Name the flag.", "Guard the index."],
    );
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), reviewed(2));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 3,
            held: 2,
        })
    );
    assert!(
        rig.forge.coderabbit.resolved().is_empty(),
        "nothing is resolved before the fix"
    );

    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("Round 3 of the review on your pull request #71 found 2 finding(s)"),
        "{}",
        fix.prompt
    );
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("Name the flag."), "{file}");
    assert!(file.contains("Guard the index."), "{file}");

    // The fix resolves both threads, and the pass, run, goes on to CI.
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: 7,
            pull_request: 71,
            round: 3,
            head: Some(fixed.clone()),
        })
    );
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_71_0", "PRRT_71_1"]);
    assert_eq!(phase(&rig, &runner)["state"], "ci");
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(
        labels(&rig),
        [on(), off()],
        "once a pass, so no second summon"
    );
}

// A review of `head` with one open thread, which goes to the worker.
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
    step(runner).unwrap()
}

// The worker's fix: a new commit, which ends the pass. Returns its head.
pub(in crate::runner) fn fixed(rig: &Rig, runner: &Mutex<Runner>, file: &'static str) -> String {
    rig.claude.script([Scripted::Push(file, "fixed\n")]);
    step(runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    assert!(matches!(
        step(runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    head
}

// A review of the head that leaves nothing open: the pass ends on it.
fn read_clean(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = summoned(project);
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), reviewed(0));
    (rig, runner, head)
}

#[test]
fn a_fix_turn_that_pushes_nothing_parks_and_a_yes_sends_the_threads_again() {
    let (rig, runner, head) = summoned("shep");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::ReviewFindingsSent { round: 3, .. })
    ));
    rig.claude
        .script([Scripted::Say("I can't push from this sandbox.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("a fix with nothing pushed raised no ruling");
    };
    assert!(
        question.starts_with(
            "The worker on pull request #71 ended its fix for round 3 of the review \
             without pushing, so those findings still hold."
        ),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    let path = rig.build_7().join("review-findings.md");
    let again = format!(
        "Your last turn on pull request #71 pushed nothing, so round 3's \
         findings in {} still hold. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    );
    let kind = &status["rulings"][0]["kind"];
    assert_eq!(kind["kind"], "stuck");
    assert_eq!(kind["reason"], "fix-not-pushed");
    assert_eq!(kind["prompt"], json!(again));
    assert_eq!(kind["review"]["reviewer"], "coderabbit");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    assert_eq!(step(&runner).unwrap(), None, "parked, not summoning");
    assert_eq!(labels(&rig), [on(), off()], "no second summon");

    // A yes sends the same findings, and a fix that pushes resolves the thread.
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
            round: 3,
            head: fixed,
        })
    );
    assert_eq!(rig.forge.coderabbit.resolved(), ["PRRT_71_0"]);
    assert_eq!(phase(&rig, &runner)["state"], "ci");
}

#[test]
fn a_timed_out_fix_turn_resumes_that_round() {
    let (rig, runner, head) = summoned("mew");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    rig.claude.script([Scripted::Fail(AgentError::TimedOut(
        crate::settings::Harness::ClaudeCode,
    ))]);
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
        question.contains("round 3 of the review without pushing"),
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
        question.contains("round 3 of the review without pushing"),
        "{question}"
    );
    assert_eq!(
        rig.reviewer.seen().len(),
        rounds,
        "the review never restarted"
    );
}

#[test]
fn a_restart_mid_summon_reads_on_and_holds_no_stale_lease() {
    let (rig, runner, head) = summoned("xilriws");
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "status", None)["leases"], json!([]));
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), reviewed(0));
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
    saved["work_items"][0]["phase"]["stage"] =
        json!({ "stage": "summon", "bot": "coderabbit", "started": now(&rig), "head": head });
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
    assert_eq!(phase(&rig, &runner)["stage"]["stage"], "summoned");
}

#[test]
fn a_label_someone_else_left_on_is_toggled_to_summon() {
    let (rig, runner, _) = reviewed_by_qwen("acme");
    rig.leases.withhold(true);
    step(&runner).unwrap(); // marks the draft ready
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
    assert_eq!(step(&runner).unwrap(), reviewed(0));
}

#[test]
fn a_commit_pushed_by_hand_after_the_pass_parks_rather_than_reaching_the_merge_ruling() {
    let (rig, runner, _) = read_clean("koji");
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    rig.forge.set_checks(&by_hand, Checks::Passed);
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("the commit pushed by hand parked nothing");
    };
    assert_eq!(
        question,
        format!(
            "Pull request #71 changed outside kelpie: its head moved to {}, a commit \
             the worker did not push. `shep kelpie rule {id} yes` accepts it \
             and kelpie carries on, and `shep kelpie rule {id} no <note>` \
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
fn a_yes_on_a_commit_pushed_by_hand_runs_the_review_and_coderabbit_on_it() {
    let (rig, runner, _) = read_clean("acme");
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
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 7,
            pull_request: 71,
            head: by_hand,
        })
    );
}
