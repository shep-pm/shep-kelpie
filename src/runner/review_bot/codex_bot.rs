//! Rounds with Codex listed, through the runner's stand-ins

use std::num::NonZeroU32;

use crate::codex::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Leases, Role, Timestamp};
use crate::review_bot::{Bot, CodexReviewer, Reviewers};
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{
    HOLDS, comments_of, cr, green, labels, listing, ready, summoned,
};
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted, Told};

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
    let week = CodexReviewer {
        reviews: NonZeroU32::new(10).unwrap(),
        hours: NonZeroU32::new(168).unwrap(),
        reviews_on_ready: false,
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

// A project listing `list`, with Codex reviewing a pull request when it
// leaves draft, which its definition says unless it says otherwise.
fn on_ready(list: &str) -> Rig {
    let rig = listing("shep", list);
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&kelpie.replace("reviews_on_ready = false\n", ""));
    rig
}

#[test]
fn marking_a_draft_ready_is_codexs_summon_under_its_lease_and_no_comment_follows() {
    let rig = on_ready(r#"["codex"]"#);
    let (runner, head) = green(&rig);
    assert!(rig.forge.draft(71));
    assert_eq!(rig.verdict(&runner), summoned(&head));
    assert!(!rig.forge.draft(71), "it was marked ready");
    assert_eq!(comments_of(&rig, SUMMON), 0, "the mark is the summon");
    assert_eq!(rig.leases.told(), [Told::Want(codex())]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["bot"], "codex");
    assert_eq!(status["leases"][0]["resource"], "codex");

    let summon = now(&rig);
    rig.forge.coderabbit.codex_start(71, summon + 8);
    rig.forge.coderabbit.codex_clean(71, &head, summon + 230);
    rig.clock.advance(230);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed { .. } | StepReport::CodeRabbitSatisfied { .. })
    ));
    assert_eq!(
        comments_of(&rig, SUMMON),
        0,
        "one review, and the book counted one"
    );
}

#[test]
fn a_summon_by_marking_ready_that_codex_never_acknowledges_is_asked_again_by_comment() {
    let rig = on_ready(r#"["codex"]"#);
    let (runner, head) = green(&rig);
    assert_eq!(rig.verdict(&runner), summoned(&head));
    rig.clock.advance(300);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(comments_of(&rig, SUMMON), 0);
    rig.clock.advance(700);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SummonedAgain { .. })
    ));
    assert_eq!(comments_of(&rig, SUMMON), 1);
}

#[test]
fn a_pull_request_kelpie_already_marked_ready_is_summoned_by_comment() {
    let rig = on_ready(r#"["codex"]"#);
    rig.leases.close(&codex(), true);
    let (runner, head) = green(&rig);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.leases.grant(&codex());
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(comments_of(&rig, SUMMON), 1, "a mark cannot be made twice");
}

#[test]
fn a_round_that_goes_to_another_bot_marks_ready_and_asks_codex_for_nothing() {
    let rig = on_ready(r#"["codex", "coderabbit"]"#);
    rig.leases.close(&codex(), true);
    let (runner, head) = green(&rig);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig).len(), 1, "CodeRabbit's");
    assert_eq!(comments_of(&rig, SUMMON), 0);
}

#[test]
fn a_thumbs_up_alone_is_a_review_with_nothing_in_it() {
    let rig = listing("shep", r#"["codex"]"#);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.codex_start(71, summon + 8);
    rig.forge.coderabbit.codex_thumbs_up(71, summon + 230);
    rig.clock.advance(230 + 60);
    assert!(
        matches!(
            step(&runner).unwrap(),
            Some(StepReport::CodeRabbitReviewed { .. } | StepReport::CodeRabbitSatisfied { .. })
        ),
        "the round does not wait out two hours for a silent ruling"
    );
}
