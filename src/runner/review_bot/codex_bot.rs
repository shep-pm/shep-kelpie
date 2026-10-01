//! Rounds with Codex listed, through the runner's stand-ins

use std::num::NonZeroU32;

use crate::codex::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Leases, Role, Timestamp};
use crate::review_bot::{Bot, ReviewWindow, Reviewers};
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{
    HOLDS, comments_of, cr, labels, listing, ready, summoned,
};
use crate::runner::{StepReport, step};
use crate::test::{Scripted, Told};

const WEEK: u64 = 168 * 3600;

fn codex() -> LeaseKind {
    Bot::Codex.lease()
}

#[test]
fn a_codex_review_reaches_the_judge_with_its_badge_and_the_fix_names_codex() {
    let rig = listing("shep", r#"["codex"]"#);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(comments_of(&rig, SUMMON), 1);
    assert_eq!(rig.leases.told(), [Told::Want(codex())]);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["bot"],
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
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            issue: 7,
            pull_request: 71,
            round: 1,
            open_threads: 1
        })
    );
    rig.claude.script([Scripted::Text(HOLDS)]);
    step(&runner).unwrap();
    let calls = rig.claude.all_calls();
    let judged = calls.iter().rfind(|c| c.role == Role::Judge).unwrap();
    assert!(
        judged.prompt.contains(
            "severity: HIGH\nlocation: work.txt:1\nwhat: Keep the stamped prefix: `bleats` strips \
             a prefix shep did not stamp.\nwhy: Codex rates it P1."
        ),
        "{}",
        judged.prompt
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitJudged { held: 1, .. })
    ));
    rig.claude
        .script([Scripted::Push("work.txt", "stripped\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("Codex round 1 on your pull request #71"),
        "{}",
        fix.prompt
    );
}

#[test]
fn a_codex_review_with_nothing_to_raise_settles_the_round() {
    let rig = listing("shep", r#"["codex"]"#);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.codex_clean(71, &head, summon + 230);
    rig.clock.advance(230);
    assert!(
        matches!(
            step(&runner).unwrap(),
            Some(
                StepReport::CodeRabbitReviewed {
                    open_threads: 0,
                    ..
                } | StepReport::CodeRabbitSatisfied { .. }
            )
        ),
        "its clean comment answers the summon"
    );
    assert_eq!(comments_of(&rig, SUMMON), 1);
}

#[test]
fn a_usage_limit_reply_parks_codex_and_the_round_goes_to_the_next_reviewer() {
    let rig = listing("shep", r#"["codex", "coderabbit"]"#);
    let week = ReviewWindow {
        reviews: NonZeroU32::new(10).unwrap(),
        hours: NonZeroU32::new(168).unwrap(),
    };
    let dog = Reviewers {
        codex: Some(week),
        ..Reviewers::default()
    };
    rig.leases.use_book(rig.clock.clone(), "shep", dog);
    rig.leases.window(&cr(), WindowFact::Summoned, now(&rig));
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.codex_refuse(71, summon + 20);
    rig.clock.advance(20);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused {
            issue: 7,
            pull_request: 71,
            opens: Timestamp(summon + 20 + WEEK),
        })
    );
    assert!(!rig.leases.held(&codex()));
    for _ in 0..3 {
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), None, "both windows closed");
    }
    assert_eq!(comments_of(&rig, SUMMON), 1, "no second summon of Codex");

    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig).len(), 1, "CodeRabbit takes the round");
    assert_eq!(comments_of(&rig, SUMMON), 1);
}
