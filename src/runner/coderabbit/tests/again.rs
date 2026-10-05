//! A summon CodeRabbit gave no sign of is sent once more, in the same round

use super::super::{HEARD_WAIT, REVIEW_WAIT};
use super::{cr, labels, now, off, on, summoned};
use crate::lease::wire::WindowFact;
use crate::runner::{StepReport, step};
use crate::test::Told;

fn again(head: &str) -> Option<StepReport> {
    Some(StepReport::SummonedAgain {
        issue: 7,
        pull_request: 71,
        head: head.to_owned(),
    })
}

#[test]
fn a_summon_with_no_sign_is_sent_once_more_after_fifteen_minutes() {
    let (rig, runner, head) = summoned("zeus");
    let summon = now(&rig);
    rig.clock.advance(HEARD_WAIT - 1);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(labels(&rig), [on()]);
    assert!(rig.leases.held(&cr()), "the lease stays for the re-send");

    rig.clock.advance(1);
    assert_eq!(step(&runner).unwrap(), again(&head));
    assert_eq!(labels(&rig), [on(), off(), on()], "a fresh event");
    assert!(rig.leases.held(&cr()));
    assert!(
        !rig.leases
            .told()
            .iter()
            .any(|t| matches!(t, Told::Window(WindowFact::Summoned, _))),
        "the re-send opens no window of its own"
    );

    step(&runner).unwrap();
    assert!(!rig.leases.held(&cr()));
    let told = rig.leases.told();
    let first = Told::Window(WindowFact::Summoned, summon);
    let windows = told.iter().filter(|t| **t == first);
    assert_eq!(windows.count(), 1, "the hour runs from the first summon");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(labels(&rig), [on(), off(), on()], "sent once more, no more");
}

#[test]
fn a_summon_with_an_in_progress_status_is_not_sent_again() {
    let (rig, runner, head) = summoned("zeus");
    rig.forge.coderabbit.progress(71, &head, now(&rig) + 20);
    rig.clock.advance(HEARD_WAIT);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(labels(&rig), [on()]);
    assert!(!rig.leases.held(&cr()), "heard, so the lease is spent");
}

#[test]
fn a_change_to_its_comment_is_a_sign_too() {
    let (rig, runner, _) = summoned("zeus");
    rig.forge.coderabbit.start(71, now(&rig) + 20);
    rig.clock.advance(HEARD_WAIT);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(labels(&rig), [on()]);
}

#[test]
fn a_rate_limit_notice_is_a_sign_and_reschedules_as_before() {
    let (rig, runner, _) = summoned("zeus");
    rig.forge.coderabbit.refuse(71, now(&rig) + 20, 30);
    rig.clock.advance(30);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused { .. })
    ));
}

#[test]
fn a_review_stuck_in_progress_is_passed_over_after_two_hours() {
    let (rig, runner, head) = summoned("zeus");
    rig.forge.coderabbit.progress(71, &head, now(&rig) + 20);
    rig.clock.advance(REVIEW_WAIT - 1);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(1);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["bots_skipped"][0]["why"], "silent");
    assert_eq!(status["rulings"], serde_json::json!([]));
}

#[test]
fn a_second_silence_passes_the_bot_over_at_two_hours() {
    let (rig, runner, head) = summoned("zeus");
    rig.clock.advance(HEARD_WAIT);
    assert_eq!(step(&runner).unwrap(), again(&head));
    rig.clock.advance(REVIEW_WAIT - HEARD_WAIT - 1);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(1);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    assert_eq!(labels(&rig), [on(), off(), on(), off()]);
}
