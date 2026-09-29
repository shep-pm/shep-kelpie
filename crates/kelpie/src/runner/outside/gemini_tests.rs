//! Gemini rounds through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::LABEL;
use crate::gemini::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::window::DAY;
use crate::lease::wire::WindowFact;
use crate::outside::Outside;
use crate::ports::{Checks, Role};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told};

const HOLDS: &str = r#"{"holds": true, "severity": "medium", "reason": "real"}"#;
const REJECTED: &str = r#"{"holds": false, "severity": "low", "reason": "not so"}"#;

const GEMINI: Outside = Outside::Gemini;

// A running project with Gemini's rounds on, and CodeRabbit's too when
// `coderabbit` says so, whose worker opened pull request 71 and whose
// qwen-review loop settled. CI has not reported.
fn reviewed_by_qwen(project: &str, coderabbit: bool) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.gemini_on();
    if coderabbit {
        rig.coderabbit_on();
    }
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

// Green CI, the pass that marks the draft ready, and the summon after it.
fn summoned(project: &str, coderabbit: bool) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project, coderabbit);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            reviewer: GEMINI,
            issue: 7,
            pull_request: 71,
            head: head.clone(),
        })
    );
    (rig, runner, head)
}

fn summons(rig: &Rig) -> Vec<(u64, String)> {
    let comments = rig.forge.comments();
    comments.into_iter().filter(|(_, c)| c == SUMMON).collect()
}

fn summon() -> (u64, String) {
    (71, SUMMON.to_owned())
}

fn now(rig: &Rig) -> u64 {
    use crate::ports::Clock;
    rig.clock.now().0
}

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

#[test]
fn with_gemini_rounds_off_no_summon_is_ever_posted() {
    let (rig, runner, head) = Rig::with_pull_request("hazels-lab");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    for _ in 0..3 {
        rig.clock.advance(3600);
        step(&runner).unwrap();
    }
    assert_eq!(summons(&rig), []);
    assert_eq!(rig.forge.gemini.summons(), Vec::<u64>::new());
    assert!(!rig.leases.told().contains(&Told::Want(LeaseKind::gemini())));
}

#[test]
fn a_summon_posts_the_comment_once_and_is_recorded_as_kelpies() {
    let (rig, runner, head) = summoned("shep", false);
    let at = rig.ask(&runner, "status", None)["leases"][0]["since"].clone();
    assert_eq!(summons(&rig), [summon()]);
    assert_eq!(
        phase(&rig, &runner),
        json!({ "state": "gemini", "stage": "summoned", "head": head, "at": at })
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["leases"],
        json!([{ "resource": "gemini", "since": at }])
    );
    for _ in 0..3 {
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), None, "waiting on the review");
    }
    drop(runner);
    let runner = rig.open().unwrap();
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None, "a restart posts nothing more");
    assert_eq!(summons(&rig), [summon()]);
    assert!(rig.forge.pull_request_labels(71).iter().all(|l| l != LABEL));
}

// The round saves the summon before posting it, so a post that failed is
// tried again on the next pass.
#[test]
fn a_summon_whose_post_failed_is_posted_on_the_next_pass() {
    let (rig, runner, head) = reviewed_by_qwen("shep", false);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // marks ready
    rig.forge.set_comments_down(true);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { .. })
    ));
    assert_eq!(phase(&rig, &runner)["posted"], false);
    rig.forge.set_comments_down(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            reviewer: GEMINI,
            ..
        })
    ));
    assert_eq!(summons(&rig), [summon()]);
    assert_eq!(phase(&rig, &runner)["posted"], serde_json::Value::Null);
}

// As if the forge took the comment and its reply was lost to a restart.
#[test]
fn a_summon_seen_on_the_forge_after_a_restart_is_not_posted_again() {
    let (rig, runner, head) = reviewed_by_qwen("shep", false);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // marks ready
    rig.forge.set_comments_down(true);
    step(&runner).unwrap(); // saved unposted, then the post fails
    rig.forge.set_comments_down(false);
    rig.forge.gemini.summoned(71); // the post landed after all
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(phase(&rig, &runner)["posted"], serde_json::Value::Null);
    assert_eq!(summons(&rig), [], "kelpie posted nothing itself");
    assert_eq!(rig.forge.gemini.summons(), [71]);
}

#[test]
fn a_review_gemini_posted_before_the_summon_is_ignored() {
    let (rig, runner, head) = reviewed_by_qwen("shep", false);
    rig.forge.gemini.review(71, &head, &["Posted on its own."]);
    rig.clock.advance(120);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // marks ready
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            reviewer: GEMINI,
            ..
        })
    ));
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None, "no answer to the summon yet");
}

#[test]
fn a_review_with_only_refuted_findings_satisfies_the_round() {
    let (rig, runner, head) = summoned("shep", false);
    let summoned_at = now(&rig);
    rig.clock.advance(180);
    rig.forge
        .gemini
        .review(71, &head, &["Rename it.", "Guard it."]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideReviewed {
            reviewer: GEMINI,
            issue: 7,
            pull_request: 71,
            round: 1,
            open_threads: 2,
        })
    );
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summoned_at)),
        "the day's window counts the summon"
    );
    rig.claude
        .script([Scripted::Text(REJECTED), Scripted::Text(REJECTED)]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideSatisfied {
            reviewer: GEMINI,
            issue: 7,
            pull_request: 71,
            rounds: 1,
        })
    );
    assert_eq!(
        rig.forge.coderabbit.resolved(),
        ["PRRT_gemini_71_0", "PRRT_gemini_71_1"]
    );
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(summons(&rig), [summon()]);
}

#[test]
fn a_confirmed_finding_goes_to_the_worker_and_its_fix_is_summoned_again() {
    let (rig, runner, head) = summoned("shep", false);
    rig.clock.advance(180);
    rig.forge
        .gemini
        .review(71, &head, &["Name the flag.", "Nothing real."]);
    step(&runner).unwrap();
    rig.claude
        .script([Scripted::Text(HOLDS), Scripted::Text(REJECTED)]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideJudged {
            reviewer: GEMINI,
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 1,
            resolved: 1,
        })
    );
    let judged: Vec<_> = rig
        .claude
        .all_calls()
        .into_iter()
        .filter(|c| c.role == Role::Judge)
        .collect();
    assert!(judged[0].prompt.contains("what: Name the flag."));

    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("Gemini round 1 on your pull request #71 left 1 finding(s) that hold"),
        "{}",
        fix.prompt
    );
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("Name the flag.") && !file.contains("Nothing real."));

    // The fix passes CI and is summoned again. Round one's held thread
    // stays open, but only the new review's threads are read.
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            reviewer: GEMINI,
            ..
        })
    ));
    rig.clock.advance(180);
    rig.forge.gemini.review(71, &fixed, &[]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideSatisfied {
            reviewer: GEMINI,
            rounds: 2,
            ..
        })
    ));
    assert_eq!(summons(&rig), [summon(), summon()]);
}

#[test]
fn a_refusal_gives_the_lease_back_and_the_window_opens_a_day_later() {
    let (rig, runner, head) = summoned("shep", false);
    rig.clock.advance(10);
    rig.forge.gemini.refuse(71);
    let refused = now(&rig);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused {
            reviewer: GEMINI,
            issue: 7,
            pull_request: 71,
            opens: crate::ports::Timestamp(refused + DAY),
        })
    );
    let told = rig.leases.told();
    assert!(told.contains(&Told::Window(WindowFact::Opens, refused + DAY)));
    assert!(told.contains(&Told::Return(LeaseKind::gemini())));
    assert_eq!(
        phase(&rig, &runner),
        json!({ "state": "gemini", "stage": "lease", "head": head })
    );
}

// Two reviews Gemini posted on its own count toward the cap of three, so
// one summon is left before the work item moves on without asking.
#[test]
fn reviews_already_on_the_pull_request_count_toward_the_cap() {
    let (rig, runner, head) = reviewed_by_qwen("shep", false);
    rig.forge.gemini.review(71, "0ldc0mm1t", &[]);
    rig.forge.gemini.review(71, &head, &["Posted on its own."]);
    rig.clock.advance(120);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // marks ready
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            reviewer: GEMINI,
            ..
        })
    ));
    rig.clock.advance(180);
    rig.forge.gemini.review(71, &head, &["Name the flag."]);
    step(&runner).unwrap();
    rig.claude.script([Scripted::Text(HOLDS)]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideJudged { held: 1, .. })
    ));

    // The held finding still goes to the worker, and its fix to CI.
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap();
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    step(&runner).unwrap(); // the fix pushed
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert_eq!(
        rig.verdict(&runner),
        Some(StepReport::GeminiCapped {
            issue: 7,
            pull_request: 71,
            reviews: 3,
        })
    );
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "merge");
    assert_eq!(summons(&rig), [summon()]);
}

#[test]
fn at_one_round_a_held_finding_is_fixed_and_the_item_moves_on_to_coderabbit() {
    let (rig, runner, head) = reviewed_by_qwen("shep", true);
    rig.edit_settings(|s| s.replace("max_rounds = 3", "max_rounds = 1"));
    drop(runner);
    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(&runner); // marks ready
    step(&runner).unwrap(); // summons Gemini
    rig.clock.advance(180);
    rig.forge.gemini.review(71, &head, &["Name the flag."]);
    step(&runner).unwrap();
    rig.claude
        .script([Scripted::Text(HOLDS), Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideJudged { held: 1, .. })
    ));
    step(&runner).unwrap(); // the fix
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    step(&runner).unwrap(); // the fix pushed
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::GeminiCapped { reviews: 1, .. })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
            | Some(StepReport::Summoned {
                reviewer: Outside::CodeRabbit,
                ..
            })
    ));
    let status = rig.ask(&runner, "status", None);
    assert!(
        status["rulings"].as_array().unwrap().is_empty(),
        "never asks"
    );
    assert_eq!(summons(&rig), [summon()]);
}

#[test]
fn gemini_runs_before_coderabbit_and_both_are_owed_before_the_merge_ruling() {
    let (rig, runner, head) = summoned("shep", true);
    let labelled = |rig: &Rig| {
        rig.forge
            .pull_request_labels(71)
            .contains(&LABEL.to_owned())
    };
    assert!(!labelled(&rig), "CodeRabbit waits for Gemini");
    rig.clock.advance(180);
    rig.forge.gemini.review(71, &head, &[]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::OutsideSatisfied {
            reviewer: GEMINI,
            ..
        })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            reviewer: Outside::CodeRabbit,
            ..
        })
    ));
    assert!(labelled(&rig));
    assert_eq!(summons(&rig), [summon()]);
}
