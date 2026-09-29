//! Full reviews: asked for by comment when CodeRabbit read the pull request
//! before and would find nothing new, and once more when an owed summon is
//! marked done with nothing posted

use std::sync::Mutex;

use serde_json::json;

use super::super::{DONE_SETTLE, FULL_REVIEW, LABEL, REVIEW_WAIT};
use super::{cr, fixed, hold_a_finding, labels, now, off, on, reviewed_by_qwen, summoned};
use crate::lease::wire::WindowFact;
use crate::ports::Checks;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told};

fn full_review(number: u64) -> Vec<(u64, String)> {
    vec![(number, FULL_REVIEW.to_owned())]
}

fn summoned_80(head: &str) -> Option<StepReport> {
    Some(StepReport::Summoned {
        issue: 5,
        pull_request: 80,
        head: head.to_owned(),
    })
}

// Pull request 80, ready, adopted with CodeRabbit on, and green. CodeRabbit
// reviewed its first commit clean before the adoption.
fn adopted(project: &str) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.coderabbit_on();
    let reviewed = rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge
        .coderabbit
        .review(80, &reviewed, Rig::EPOCH - 60, &[]);
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.ready_pull_request(80);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    (rig, runner, head)
}

#[test]
fn an_adopted_pull_requests_owed_summon_asks_for_a_full_review_under_the_lease() {
    let (rig, runner, head) = adopted("shep");
    rig.forge.watch_state(rig.paths().state);
    assert_eq!(rig.verdict(&runner), summoned_80(&head));
    assert_eq!(rig.forge.comments(), full_review(80));
    assert_eq!(rig.forge.pull_request_labels(80), Vec::<String>::new());
    let summon = now(&rig);
    assert_eq!(
        rig.forge.saved_at_comment()[0]["work_items"][0]["phase"],
        json!({ "state": "coderabbit", "stage": "summoned", "head": head, "at": summon, "full": true }),
        "the summon is saved before the comment goes out, so no restart posts it twice"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["leases"],
        json!([{ "resource": "coderabbit", "issue": 5, "since": summon }])
    );

    rig.forge.coderabbit.review(80, &head, summon + 600, &[]);
    rig.clock.advance(600);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summon)),
        "the dog counts the comment as it counts the label"
    );
    assert_eq!(rig.forge.comments(), full_review(80), "asked once");
}

#[test]
fn a_fresh_pull_request_is_summoned_by_the_label_alone() {
    let (rig, _runner, _head) = summoned("shep");
    assert_eq!(labels(&rig), [on()]);
    assert_eq!(rig.forge.comments(), []);
}

// shep#614 as main's build left it: adopted, its owed summon made by label
// at 05:53 on a head that only merged `main`, CodeRabbit marking that head
// done 24 seconds on with nothing posted, and the runner paused for hours.
fn paused_like_614(project: &str) -> (Rig, String) {
    let (rig, runner, head) = adopted(project);
    rig.leases.withhold(true);
    assert_eq!(rig.verdict(&runner), None, "no lease, no summon");
    drop(runner);
    let summon = now(&rig);
    let state = rig.paths().state;
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let item = &mut saved["work_items"][0];
    item["phase"] =
        json!({ "state": "coderabbit", "stage": "summoned", "head": head, "at": summon });
    item["known"]["labels"] = json!([LABEL]);
    std::fs::write(&state, saved.to_string()).unwrap();
    rig.forge.label_pull_request(80, LABEL);
    rig.forge.coderabbit.complete(80, &head, summon + 24);
    rig.clock.advance(REVIEW_WAIT + 3 * 3600);
    rig.leases.withhold(false);
    (rig, head)
}

#[test]
fn an_owed_summon_marked_done_with_nothing_posted_asks_once_for_a_full_review_then_waits() {
    let (rig, head) = paused_like_614("shep");
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), summoned_80(&head));
    assert_eq!(rig.forge.comments(), full_review(80));
    assert_eq!(
        rig.forge.pull_request_labels(80),
        Vec::<String>::new(),
        "the label comes off"
    );
    let asked = now(&rig);
    assert!(
        rig.leases.held(&cr()),
        "the full review is summoned under the lease"
    );

    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None, "the old done is no answer");
    rig.forge.coderabbit.complete(80, &head, asked + 90);
    rig.clock.advance(60 + DONE_SETTLE);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "done again with nothing: it waits"
    );
    assert_eq!(rig.forge.comments(), full_review(80), "asked once");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    rig.forge.coderabbit.review(80, &head, asked + 900, &[]);
    rig.clock.advance(900);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
}

#[test]
fn a_full_review_answered_with_nothing_again_is_the_maintainers_after_two_hours() {
    let (rig, runner, head) = adopted("rotom");
    assert_eq!(rig.verdict(&runner), summoned_80(&head));
    rig.forge.coderabbit.complete(80, &head, now(&rig) + 30);
    rig.clock.advance(REVIEW_WAIT);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { .. })
    ));
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["kind"]["kind"],
        "coderabbit-silent"
    );
    let comments = rig.forge.comments().into_iter();
    assert_eq!(comments.filter(|(_, c)| c == FULL_REVIEW).count(), 1);
}

#[test]
fn a_head_marked_done_with_nothing_posted_is_a_clean_read_once_it_settles() {
    let (rig, runner, head) = summoned("shep");
    let summon = now(&rig);
    rig.forge.coderabbit.complete(71, &head, summon + 10);
    rig.clock.advance(10);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "a review may still be posting"
    );
    assert!(!rig.leases.held(&cr()), "but the summon was answered");

    rig.clock.advance(DONE_SETTLE);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied {
            issue: 7,
            pull_request: 71,
            rounds: 1,
        })
    );
    assert_eq!(labels(&rig), [on(), off()]);
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn the_summon_after_kelpie_catches_the_branch_up_asks_for_a_full_review() {
    let (rig, runner, head) = summoned("shep");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &head, "Name the flag."),
        Some(StepReport::CodeRabbitJudged { .. })
    ));
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    rig.land_on_origin("landed.txt");
    let Some(StepReport::Rebased { head: rebased, .. }) = rig.verdict(&runner) else {
        panic!("the branch was not caught up");
    };
    rig.forge.set_checks(&rebased, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(rig.forge.comments(), full_review(71));
    assert_eq!(labels(&rig), [on(), off()], "round one's label only");

    // A round past the default cap's two would park on a ruling instead.
    drop(runner);
    rig.edit_settings(|s| s.replace("divisor = 1000", "divisor = 1"));
    let runner = rig.open().unwrap();
    rig.forge.coderabbit.settle("PRRT_71_0");
    assert!(matches!(
        hold_a_finding(&rig, &runner, &rebased, "Name it again."),
        Some(StepReport::CodeRabbitJudged { .. })
    ));
    fixed(&rig, &runner, "fix-2.txt");
    assert_eq!(
        labels(&rig),
        [on(), off(), on()],
        "the full review read the catch-up, so the next round asks by label"
    );
    assert_eq!(rig.forge.comments(), full_review(71));
}

#[test]
fn a_full_review_the_forge_would_not_post_is_asked_again_and_never_twice() {
    let (rig, runner, head) = adopted("xilriws");
    rig.forge.set_comments_down(true);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::GateFailed { .. })
    ));
    rig.forge.set_comments_down(false);
    assert_eq!(step(&runner).unwrap(), summoned_80(&head));
    drop(runner);
    let runner = rig.open().unwrap();
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.comments(), full_review(80));
}

#[test]
fn an_owed_summon_waiting_for_the_lease_to_ask_again_takes_the_label_off_first() {
    let (rig, head) = paused_like_614("chelone");
    rig.leases.withhold(true);
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), None, "no lease, no full review yet");
    assert_eq!(
        rig.forge.pull_request_labels(80),
        Vec::<String>::new(),
        "a push while it waits summons nothing outside the lease"
    );
    assert_eq!(rig.forge.comments(), []);

    rig.leases.withhold(false);
    assert_eq!(step(&runner).unwrap(), summoned_80(&head));
    assert_eq!(rig.forge.comments(), full_review(80));
}

#[test]
fn a_catch_up_before_coderabbits_first_read_is_summoned_by_the_label_alone() {
    let (rig, runner, head) = reviewed_by_qwen("golbat");
    rig.land_on_origin("landed.txt");
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Rebased { head: rebased, .. }) = rig.verdict(&runner) else {
        panic!("the branch was not caught up");
    };
    rig.forge.set_checks(&rebased, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [on()]);
    assert_eq!(rig.forge.comments(), []);
}
