//! A fixed number of CodeRabbit rounds in place of the divisor's cap

use std::sync::Mutex;

use serde_json::json;

use super::super::{FULL_REVIEW, HEARD_WAIT};
use super::full::adopted_set;
use super::{HOLDS, LABEL, fixed, hold_a_finding, labels, now, off, on, reviewed_by_qwen};
use crate::ports::Checks;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted};

// Allows the project `rounds` CodeRabbit rounds, read when a runner next opens.
fn with_rounds(rig: &Rig, rounds: u32) {
    rig.edit_settings(|s| {
        assert!(s.contains("divisor = 1000\n"), "the default divisor moved");
        s.replace(
            "divisor = 1000\n",
            &format!("divisor = 1000\nrounds = {rounds}\n"),
        )
    });
}

// A summoned round on a project that allows `rounds` of them. The runner
// restarts before the summon, since a restart clears the lease rows.
fn summoned_with(project: &str, rounds: u32) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project);
    with_rounds(&rig, rounds);
    drop(runner);
    let runner = rig.open().unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    (rig, runner, head)
}

// The worker's fix for the last round on `branch`: a push and green CI on it.
fn last_fix(rig: &Rig, runner: &Mutex<Runner>, branch: &str) -> Option<StepReport> {
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(runner).unwrap(); // the fix turn
    let head = rig.forge.head_of(branch).unwrap();
    assert!(matches!(
        step(runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    rig.forge.set_checks(&head, Checks::Passed);
    rig.verdict(runner)
}

#[test]
fn one_round_sends_its_held_findings_and_the_fix_goes_to_the_merge_without_a_summon() {
    let (rig, runner, head) = summoned_with("shep", 1);
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::CodeRabbitJudged {
            round: 1,
            held: 1,
            ..
        })
    ));
    assert!(
        matches!(
            last_fix(&rig, &runner, "kelpie/7"),
            Some(StepReport::Ruling { id: 1, .. })
        ),
        "the fix summoned another round"
    );
    assert_eq!(labels(&rig), [on(), off()], "no second summon");
    assert!(
        !rig.forge.comments().iter().any(|(_, c)| c == FULL_REVIEW),
        "no summon by comment either"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], json!("merge"));

    assert_eq!(merged_on_yes(&rig, &runner), Some(true));
    assert_eq!(rig.forge.merges().len(), 1);
}

#[test]
fn a_label_left_on_after_the_last_round_comes_off_before_the_fix() {
    let (rig, runner, head) = summoned_with("rotom", 1);
    rig.forge
        .coderabbit
        .review(71, &head, now(&rig) + 60, &["Name the flag."]);
    rig.clock.advance(60);
    step(&runner).unwrap(); // the review lands and the label comes off
    rig.forge.label_pull_request(71, LABEL);
    rig.claude.script([Scripted::Text(HOLDS)]);
    step(&runner).unwrap(); // the judge holds it
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitJudged { .. })
    ));
    assert_eq!(labels(&rig), [on(), off(), off()]);
}

#[test]
fn two_rounds_summon_once_more_after_the_first_fix_and_not_after_the_second() {
    let (rig, runner, head) = summoned_with("golbat", 2);
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "First."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    let head = fixed(&rig, &runner, "one.txt");
    rig.forge.coderabbit.settle("PRRT_71_0");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Second."),
        Some(StepReport::CodeRabbitJudged { round: 2, .. })
    ));
    assert!(matches!(
        last_fix(&rig, &runner, "kelpie/7"),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
}

#[test]
fn a_re_send_in_the_one_round_is_that_round_and_the_fix_still_reaches_the_merge() {
    let (rig, runner, head) = summoned_with("xilriws", 1);
    rig.clock.advance(HEARD_WAIT);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SummonedAgain { .. })
    ));
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::CodeRabbitJudged { round: 1, .. })
    ));
    assert!(matches!(
        last_fix(&rig, &runner, "kelpie/7"),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["coderabbit"]["rounds"], json!(1));
}

// Merges on a yes to the merge ruling, and says whether it did.
fn merged_on_yes(rig: &Rig, runner: &Mutex<Runner>) -> Option<bool> {
    rig.ask(runner, "rule", Some("1 yes"));
    rig.clock.advance(CHECKS_SETTLE);
    (0..3).find_map(|_| match step(runner).unwrap() {
        Some(StepReport::Finished { merged, .. }) => Some(merged),
        _ => None,
    })
}

#[test]
fn an_adopted_pull_request_whose_earlier_review_spent_the_rounds_still_gets_one_full_review() {
    let (rig, runner, head) = adopted_set("shep", |rig| with_rounds(rig, 1));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["coderabbit"]["rounds"],
        json!(1),
        "the review from before the adoption spent the round"
    );
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    let summon = now(&rig);
    rig.forge
        .coderabbit
        .review(80, &head, summon + 600, &["Name the flag."]);
    rig.clock.advance(600);
    step(&runner).unwrap(); // the review lands
    rig.claude.script([Scripted::Text(HOLDS)]);
    step(&runner).unwrap(); // the judge holds it
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitJudged { held: 1, .. })
    ));
    assert!(matches!(
        last_fix(&rig, &runner, "fix/timeline"),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let asked: Vec<_> = rig.forge.comments();
    assert_eq!(asked, [(80, FULL_REVIEW.to_owned())], "one full review");
    assert_eq!(merged_on_yes(&rig, &runner), Some(true));
}
