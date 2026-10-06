//! An adopted pull request with both bots listed: each owes it a summon of
//! kelpie's own

use std::sync::Mutex;

use super::two_bots::{listing, summons_by_comment};
use crate::coderabbit::FULL_REVIEW;
use crate::lease::LeaseKind;
use crate::review_bot::Bot;
use crate::runner::coderabbit::tests::now;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

const MONTH: u64 = 720 * 3600;

fn cubic() -> LeaseKind {
    Bot::Cubic.lease()
}

// Pull request 80, ready, adopted by a project listing both bots, each of
// which reviewed its head an hour before the adoption.
fn adopted_by_both(rig: &Rig) -> (Mutex<Runner>, String) {
    rig.push_by_hand("fix/timeline", "work.txt");
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge
        .coderabbit
        .review(80, &head, Rig::EPOCH - 3600, &[]);
    rig.forge
        .coderabbit
        .cubic_review(80, &head, Rig::EPOCH - 3600, &[]);
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.ready_pull_request(80);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    (runner, head)
}

fn summoned_80(head: &str) -> Option<StepReport> {
    Some(StepReport::Summoned {
        issue: 5,
        pull_request: 80,
        head: head.to_owned(),
    })
}

// CodeRabbit answering its own summon settles only its own debt.
#[test]
fn each_listed_bot_owes_an_adopted_pull_request_its_own_summon() {
    let rig = listing("shep", &["coderabbit", "cubic"]);
    let (runner, head) = adopted_by_both(&rig);
    assert_eq!(step(&runner).unwrap(), summoned_80(&head));
    rig.forge.coderabbit.review(80, &head, now(&rig) + 300, &[]);
    rig.clock.advance(300);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::BotReviewed { round: 1, .. })
    ));
    assert_eq!(
        step(&runner).unwrap(),
        summoned_80(&head),
        "cubic's review from before the adoption stands for no read"
    );
    assert_eq!(summons_by_comment(&rig), 1);
}

// cubic, passed over while it owes its summon, still owes it, and its debt
// lifts its own `rounds` alone: CodeRabbit, answered, keeps to its one read.
#[test]
fn a_bot_skipped_while_owed_keeps_its_debt_and_lifts_no_other_bots_rounds() {
    let rig = listing("shep", &["coderabbit", "cubic"]);
    for (bot, reviews, hours) in [("coderabbit", 1, 1), ("cubic", 20, 720)] {
        let file = format!(
            "---\nrole: reviewer\nharness: bot\nbot: {bot}\nreviews: {reviews}\n\
             hours: {hours}\nrounds: 1\n---\n"
        );
        rig.write_agent(bot, &file);
    }
    rig.leases
        .opens_at(&cubic(), crate::ports::Timestamp(Rig::EPOCH + MONTH));
    let (runner, head) = adopted_by_both(&rig);
    assert_eq!(step(&runner).unwrap(), summoned_80(&head));
    rig.forge.coderabbit.review(80, &head, now(&rig) + 300, &[]);
    rig.clock.advance(300);
    step(&runner).unwrap(); // CodeRabbit's read lands
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 2, .. })
    ));
    rig.leases
        .opens_at(&cubic(), crate::ports::Timestamp(now(&rig)));

    rig.forge.set_checks(&head, crate::ports::Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.ask(&runner, "rule", Some("1 no once more"));
    rig.claude.script([
        Scripted::Push("again.txt", "again\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the noted turn: pushes, and a pass begins
    step(&runner).unwrap(); // round 1, qwen
    step(&runner).unwrap(); // round 2, claude
    let again = rig.forge.head_of("fix/timeline").unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        summoned_80(&again),
        "cubic's own read"
    );
    let full = rig.forge.comments().into_iter();
    assert_eq!(
        full.filter(|(_, c)| c == FULL_REVIEW).count(),
        1,
        "CodeRabbit, answered, keeps to its one read"
    );
    assert_eq!(summons_by_comment(&rig), 1);
}
