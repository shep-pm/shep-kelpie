use crate::ports::Checks;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::Rig;
use std::sync::Mutex;

const NO_WEBHOOK: &str = "";

#[test]
fn a_ruling_with_no_webhook_posts_nothing_and_blocks_nothing() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| rig.set_kelpie_settings(NO_WEBHOOK));
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.alerts.posts().is_empty());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["id"], 1, "still pending for `rule`");

    rig.clock.advance(Rig::DAY);
    assert_eq!(step(&runner).unwrap(), None, "and never retried");
    let status = rig.ask(&runner, "rule", Some("1 no try again"));
    assert_eq!(status["rulings"], serde_json::json!([]));
}

// A project under `auto` with its pull request's merge just landed, so the
// next step is the merge's notice
fn just_merged(no_webhook: bool) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_set("shep", |rig| {
        rig.merge_auto();
        if no_webhook {
            rig.set_kelpie_settings(NO_WEBHOOK);
        }
    });
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { merged: true, .. })
    ));
    (rig, runner)
}

#[test]
fn a_merge_notice_with_no_webhook_is_logged_and_dropped() {
    let (rig, runner) = just_merged(true);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed {
            issue: 7,
            pull_request: 71,
        })
    );
    assert!(rig.alerts.posts().is_empty());
    assert_eq!(step(&runner).unwrap(), None, "and is not kept");
}

#[test]
fn a_merge_notice_goes_to_the_webhook_alone() {
    let (rig, runner) = just_merged(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed { .. })
    ));
    let [(webhook, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(webhook, rig.webhook());
    assert_eq!(alert.title, "kelpie: shep merged #71");
}
