//! A bot file's `rounds`: the most reads it makes of a work item's pull
//! request, whatever its passes

use std::sync::Mutex;

use serde_json::json;

use super::super::{FULL_REVIEW, HEARD_WAIT};
use super::full::adopted_set;
use super::{fixed, hold_a_finding, labels, now, off, on, reviewed_by_qwen};
use crate::ports::Checks;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted};

// CodeRabbit's file with `rounds` set, read when a runner next opens.
fn with_rounds(rig: &Rig, rounds: u32) {
    let file = format!(
        "---\nrole: reviewer\nharness: bot\nbot: coderabbit\nreviews: 1\nhours: 1\n\
         rounds: {rounds}\n---\n"
    );
    rig.write_agent("coderabbit", &file);
}

// CodeRabbit's round summoned, on a project whose file allows it `rounds` reads.
fn summoned_with(project: &str, rounds: u32) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = reviewed_by_qwen(project);
    with_rounds(&rig, rounds);
    drop(runner);
    let runner = rig.open().unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    (rig, runner, head)
}

// The merge ruling on `head`, its no, and the noted turn's pass up to the
// round after the Claude round.
fn noted(rig: &Rig, runner: &Mutex<Runner>, head: &str) {
    rig.forge.set_checks(head, Checks::Passed);
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.ask(runner, "rule", Some("1 no name the flag"));
    rig.claude.script([
        Scripted::Push("named.txt", "named\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(runner).unwrap(); // the noted turn: pushes, and a pass begins
    step(runner).unwrap(); // round 1, qwen: clean by default
    step(runner).unwrap(); // round 2, claude: scripted clean above
}

#[test]
fn one_round_reads_the_first_pass_and_not_the_next() {
    let (rig, runner, head) = summoned_with("shep", 1);
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    let head = fixed(&rig, &runner, "flag.txt");
    noted(&rig, &runner, &head);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"]["state"], "ci",
        "the second pass went on without CodeRabbit"
    );
    assert_eq!(status["work_item"]["bot_reads"], json!({ "coderabbit": 1 }));
    assert_eq!(labels(&rig), [on(), off()], "no second summon");
    assert!(
        !rig.forge.comments().iter().any(|(_, c)| c == FULL_REVIEW),
        "no summon by comment either"
    );
}

#[test]
fn two_rounds_read_two_passes_and_not_a_third() {
    let (rig, runner, head) = summoned_with("golbat", 2);
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    step(&runner).unwrap(); // the first read lands clean
    noted(&rig, &runner, &head);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    let second = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge
        .coderabbit
        .review(71, &second, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    step(&runner).unwrap(); // the second read lands clean
    rig.forge.set_checks(&second, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 2, .. })
    ));
    rig.ask(&runner, "rule", Some("2 no once more"));
    rig.claude.script([
        Scripted::Push("again.txt", "again\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the noted turn
    step(&runner).unwrap(); // round 1, qwen
    step(&runner).unwrap(); // round 2, claude
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["work_item"]["bot_reads"], json!({ "coderabbit": 2 }));
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
}

#[test]
fn a_re_send_in_the_one_round_is_that_read() {
    let (rig, runner, head) = summoned_with("xilriws", 1);
    rig.clock.advance(HEARD_WAIT);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SummonedAgain { .. })
    ));
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::ReviewFindingsSent { round: 3, .. })
    ));
    fixed(&rig, &runner, "flag.txt");
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["bot_reads"], json!({ "coderabbit": 1 }));
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
        rig.ask(&runner, "status", None)["work_item"]["bot_reads"],
        json!({ "coderabbit": 1 }),
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
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    rig.claude.script([Scripted::Push("flag.txt", "named\n")]);
    step(&runner).unwrap(); // the fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    let fixed = rig.forge.head_of("fix/timeline").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let asked: Vec<_> = rig.forge.comments();
    assert_eq!(asked, [(80, FULL_REVIEW.to_owned())], "one full review");
    assert_eq!(merged_on_yes(&rig, &runner), Some(true));
}
