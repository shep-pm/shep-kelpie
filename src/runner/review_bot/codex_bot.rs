//! Passes with Codex listed, through the runner's stand-ins

use super::{HEARD_WAIT, REVIEW_WAIT};
use crate::codex::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::review_bot::Bot;
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{
    bot_reviewed, comments_of, labels, listing, reviewed, summoned,
};
use crate::runner::{StepReport, step};
use crate::settings::AgentName;
use crate::test::{Rig, Scripted, Told};

const WEEK: u64 = 168 * 3600;

fn codex() -> LeaseKind {
    Bot::Codex.lease()
}

// Pull request 71 read by the qwen and Claude rounds, with Codex next and
// the draft already marked ready.
fn ready(rig: &Rig) -> (std::sync::Mutex<crate::runner::Runner>, String) {
    let (runner, head) = reviewed(rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    (runner, head)
}

#[test]
fn a_codex_review_reaches_the_worker_with_its_badge() {
    let rig = listing("shep", &["codex"]);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(comments_of(&rig, SUMMON), 1);
    assert_eq!(rig.leases.told(), [Told::Want(codex())]);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["stage"]["bot"],
        "codex"
    );

    let summon = now(&rig);
    rig.forge.coderabbit.codex_start(71, summon + 8);
    rig.clock.advance(8);
    assert_eq!(step(&runner).unwrap(), None, "heard, not yet reviewed");
    assert!(!rig.leases.held(&codex()), "its answer took the summon");
    assert_eq!(comments_of(&rig, SUMMON), 1);

    let finding = "P1|Keep the stamped prefix|`bleats` strips a prefix shep did not stamp.";
    rig.forge
        .coderabbit
        .codex_review(71, &head, summon + 240, &[finding]);
    rig.clock.advance(240);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(3, "codex", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        file.contains("HIGH|work.txt:1|Keep the stamped prefix: `bleats` strips a prefix shep did not stamp.|Codex rates it P1."),
        "{file}"
    );
    rig.claude
        .script([Scripted::Push("work.txt", "stripped\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("Round 3 of the review on your pull request #71"),
        "{}",
        fix.prompt
    );
}

#[test]
fn a_codex_review_with_nothing_to_raise_ends_its_round() {
    let rig = listing("shep", &["codex"]);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.codex_clean(71, &head, summon + 230);
    rig.clock.advance(230);
    assert_eq!(
        step(&runner).unwrap(),
        bot_reviewed(3, "codex", 0),
        "its clean comment answers the summon"
    );
    assert_eq!(comments_of(&rig, SUMMON), 1);
}

#[test]
fn a_usage_limit_reply_passes_codex_over_for_its_week_and_the_pass_goes_on() {
    let rig = listing("shep", &["codex", "coderabbit"]);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.codex_refuse(71, summon + 20);
    rig.clock.advance(20);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped {
            issue: 7,
            pull_request: 71,
            round: 3,
            reviewer: AgentName::kelpies("codex"),
            reason: "its window opens more than an hour on".into(),
        })
    );
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Opens, summon + 20 + WEEK))
    );
    assert!(!rig.leases.held(&codex()));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig).len(), 1, "CodeRabbit reads next");
    assert_eq!(comments_of(&rig, SUMMON), 1, "no second summon of Codex");
}

// A project listing `bots` after its qwen and Claude rounds, whose Codex
// file says it reviews a pull request when it leaves draft.
fn on_ready(bots: &[&str]) -> Rig {
    let rig = listing("shep", bots);
    let file = "---\nrole: reviewer\nharness: bot\nbot: codex\nreviews: 10\nhours: 168\n\
                reviews_on_ready: true\n---\n";
    rig.write_agent("codex", file);
    rig
}

#[test]
fn marking_a_draft_ready_is_codexs_summon_under_its_lease_and_no_comment_follows() {
    let rig = on_ready(&["codex"]);
    let (runner, head) = reviewed(&rig);
    assert!(rig.forge.draft(71));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert!(!rig.forge.draft(71), "it was marked ready");
    assert_eq!(comments_of(&rig, SUMMON), 0, "the mark is the summon");
    assert_eq!(rig.leases.told(), [Told::Want(codex())]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["stage"]["bot"], "codex");
    assert_eq!(status["leases"][0]["resource"], "codex");

    let summon = now(&rig);
    rig.forge.coderabbit.codex_start(71, summon + 8);
    rig.forge.coderabbit.codex_clean(71, &head, summon + 230);
    rig.clock.advance(230);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(3, "codex", 0));
    assert_eq!(comments_of(&rig, SUMMON), 0, "one review");
    let counted = rig
        .leases
        .told()
        .into_iter()
        .filter(|told| matches!(told, Told::Window(WindowFact::Summoned, _)))
        .count();
    assert_eq!(counted, 1, "and the book counted one");
}

// A comment on top would spend a second review, so a summon by marking
// ready is never sent again: unanswered, it is passed over at two hours.
#[test]
fn a_summon_by_marking_ready_is_never_sent_again_by_comment() {
    let rig = on_ready(&["codex"]);
    let (runner, head) = reviewed(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    for _ in 0..4 {
        rig.clock.advance(HEARD_WAIT);
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(comments_of(&rig, SUMMON), 0);
    assert!(!rig.leases.held(&codex()), "counted spent, unanswered");
    rig.clock.advance(REVIEW_WAIT - 4 * HEARD_WAIT);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    assert_eq!(comments_of(&rig, SUMMON), 0, "never by comment");
}

// Marking it ready outside the lease would draw a review the window never counts.
#[test]
fn a_draft_waits_for_codexs_lease_before_the_mark_that_summons_it() {
    let rig = on_ready(&["codex"]);
    rig.leases.close(&codex(), true);
    let (runner, head) = reviewed(&rig);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.forge.draft(71), "not marked without the lease");
    rig.leases.grant(&codex());
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert!(!rig.forge.draft(71));
    assert_eq!(comments_of(&rig, SUMMON), 0);
}

#[test]
fn a_pull_request_already_ready_is_summoned_by_comment() {
    let rig = on_ready(&["codex"]);
    let (runner, head) = reviewed(&rig);
    rig.forge.ready_pull_request(71);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(comments_of(&rig, SUMMON), 1, "a mark cannot be made twice");
}

#[test]
fn a_thumbs_up_alone_is_a_review_with_nothing_in_it() {
    let rig = listing("shep", &["codex"]);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.codex_start(71, summon + 8);
    rig.forge.coderabbit.codex_thumbs_up(71, summon + 230);
    rig.clock.advance(230 + 60);
    assert_eq!(
        step(&runner).unwrap(),
        bot_reviewed(3, "codex", 0),
        "the round does not wait out two hours"
    );
}
