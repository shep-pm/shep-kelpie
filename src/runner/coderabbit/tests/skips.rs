//! CodeRabbit's round passed over: a lease never granted, a bot the list
//! dropped, a repo that is not public, and a label left on before the fix

use serde_json::json;

use super::super::{HEARD_WAIT, LABEL, REVIEW_WAIT};
use super::{cr, labels, now, off, on, reviewed, reviewed_by_qwen, skipped, summoned};
use crate::runner::{StepReport, step};

// A dog that never grants, or a book the runner cannot read, holds the pass
// no longer than a bot that never answers.
#[test]
fn a_lease_never_granted_passes_the_bot_over_after_two_hours() {
    let (rig, runner, _) = reviewed_by_qwen("shep");
    rig.leases.withhold(true);
    let started = now(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(REVIEW_WAIT - 1);
    assert_eq!(step(&runner).unwrap(), None, "still asking");
    rig.clock.advance(1);
    assert_eq!(
        step(&runner).unwrap(),
        skipped("it could not be summoned in the two hours after its round began")
    );
    assert_eq!(labels(&rig), []);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["bots_skipped"],
        json!([{ "why": "waited", "reviewer": "coderabbit", "since": started }])
    );
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
}

#[test]
fn a_bot_the_list_drops_mid_round_is_never_summoned_again() {
    let (rig, runner, _) = summoned("koji");
    rig.edit_settings(|s| {
        s.replace(
            crate::test::RIG_REVIEWERS_AND_CODERABBIT,
            crate::test::RIG_REVIEWERS,
        )
    });
    runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        skipped("the project no longer lists it")
    );
    assert_eq!(labels(&rig), [on(), off()]);
    assert!(!rig.leases.held(&cr()));
    rig.clock.advance(HEARD_WAIT);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI waits; nothing is sent again"
    );
    assert_eq!(labels(&rig), [on(), off()]);
}

// The repo was made private after the runner started.
#[test]
fn coderabbit_is_never_summoned_on_a_repo_that_is_not_public() {
    let (rig, runner, _) = reviewed_by_qwen("acme");
    rig.forge.set_visibility(crate::ports::Visibility::Private);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(
        step(&runner).unwrap(),
        skipped("the repo is not public, and CodeRabbit's free plan reviews public repos only")
    );
    assert_eq!(labels(&rig), []);
    assert!(!rig.leases.held(&cr()));
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_label_left_on_after_the_bots_read_comes_off_before_the_fix_turn() {
    let (rig, runner, head) = summoned("rotom");
    rig.forge
        .coderabbit
        .review(71, &head, now(&rig) + 60, &["Name the flag."]);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), reviewed(1));
    rig.forge.label_pull_request(71, LABEL);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    assert_eq!(labels(&rig), [on(), off(), off()]);
    assert!(!rig.forge.pull_request_labels(71).iter().any(|l| l == LABEL));
}

// Refusals inside the hour send the round back to wait, on the clock its
// round started: they cannot hold the pass for good.
#[test]
fn refusals_inside_the_hour_do_not_restart_the_wait_to_summon() {
    let (rig, runner, _) = summoned("reactmap");
    let started = now(&rig);
    rig.forge.coderabbit.refuse(71, started + 20, 12);
    rig.clock.advance(30);
    rig.leases.withhold(true);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused { .. })
    ));
    rig.clock.advance(REVIEW_WAIT - 31);
    assert_eq!(step(&runner).unwrap(), None, "still inside the two hours");
    rig.clock.advance(1);
    assert_eq!(
        step(&runner).unwrap(),
        skipped("it could not be summoned in the two hours after its round began")
    );
    let skipped = &rig.ask(&runner, "status", None)["work_item"]["bots_skipped"];
    assert_eq!(skipped[0]["since"], json!(started));
}
