use crate::ports::Checks;
use crate::ports::{Clock, Timestamp};
use crate::runner::{CHECKS_SETTLE, OpenError, Runner, StepReport, step};
use crate::test::Rig;
use std::sync::Mutex;

const NO_WEBHOOK: &str = "";

// Kelpie's settings with the rig's webhook and `channels` chosen
fn kelpie_settings(channels: &str) -> String {
    format!(
        "ruling_channels = {channels}\n[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    )
}

#[test]
fn webhook_only_never_starts_a_relay() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        rig.relay.set_up(true);
        rig.set_ruling_channels(r#"["webhook"]"#);
    });
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(rig.alerts.posts().len(), 1);
    assert!(rig.relay.sent().is_empty());
    assert_eq!(rig.relay.clears(), 0, "not even a clear of one");

    rig.ask(&runner, "rule", Some("1 no try again"));
    step(&runner).unwrap();
    assert!(rig.relay.told().is_empty());
}

#[test]
fn relay_only_posts_no_webhook_and_needs_none() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        rig.relay.set_up(true);
        rig.set_kelpie_settings(NO_WEBHOOK);
        rig.set_ruling_channels(r#"["relay"]"#);
    });
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert!(rig.alerts.posts().is_empty());
    assert_eq!(rig.relay.sent().len(), 1);
    assert_eq!(
        rig.ask(&runner, "status", None)["rulings"][0]["alerted"],
        true
    );
}

#[test]
fn relay_only_keeps_a_ruling_owed_until_the_relay_takes_it() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        rig.set_kelpie_settings(NO_WEBHOOK);
        rig.set_ruling_channels(r#"["relay"]"#);
    });
    let offset = rig.clock.now().0 - Rig::EPOCH;
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::AlertFailed {
            id: 1,
            reason: "cannot reach the relay: the rig's relay is down".into(),
            retry_at: Timestamp(Rig::EPOCH + offset + 60),
        })
    );
    assert!(rig.alerts.posts().is_empty());

    rig.relay.set_up(true);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(rig.relay.sent().len(), 1);
}

#[test]
fn kelpies_settings_choose_when_the_project_names_no_channels() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        rig.relay.set_up(true);
        rig.set_kelpie_settings(&kelpie_settings(r#"["relay"]"#));
    });
    step(&runner).unwrap();
    assert!(rig.alerts.posts().is_empty());
    assert_eq!(rig.relay.sent().len(), 1);
}

#[test]
fn a_projects_choice_wins_over_kelpies() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| {
        rig.relay.set_up(true);
        rig.set_kelpie_settings(&kelpie_settings(r#"["relay"]"#));
        rig.set_ruling_channels(r#"["webhook"]"#);
    });
    step(&runner).unwrap();
    assert_eq!(rig.alerts.posts().len(), 1);
    assert!(rig.relay.sent().is_empty());
}

#[test]
fn a_project_that_names_nothing_keeps_both() {
    let (rig, runner, _) = Rig::parked_set("shep", |rig| rig.relay.set_up(true));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(rig.alerts.posts().len(), 1);
    assert_eq!(rig.relay.sent().len(), 1);
}

#[test]
fn neither_channel_is_refused_at_start() {
    let rig = Rig::new("shep");
    rig.set_ruling_channels("[]");
    let err = rig.open().expect_err("no channel");
    assert!(matches!(err, OpenError::Settings(_)), "{err}");
    assert!(err.to_string().contains("at least one"), "{err}");

    let rig = Rig::new("shep");
    rig.set_kelpie_settings("ruling_channels = []\n");
    let err = rig.open().expect_err("no channel");
    assert!(err.to_string().contains("not right"), "{err}");
}

#[test]
fn the_webhook_is_required_only_when_rulings_go_to_it() {
    let rig = Rig::new("shep");
    rig.set_kelpie_settings(NO_WEBHOOK);
    let err = rig
        .open()
        .expect_err("both channels, no webhook")
        .to_string();
    assert!(err.contains("`ruling_channels`"), "{err}");
    assert!(err.contains("`[webhook]`"), "{err}");

    rig.set_ruling_channels(r#"["relay"]"#);
    rig.open().expect("relay only needs no webhook");

    std::fs::remove_file(rig.paths().kelpie_settings).unwrap();
    rig.open().expect("relay only needs no settings file");
    rig.edit_settings(|s| s.replacen("[\"relay\"]", "[\"webhook\"]", 1));
    let err = rig
        .open()
        .expect_err("no file to hold the webhook")
        .to_string();
    assert!(err.contains("`[webhook]`"), "{err}");
}

// A project under `auto` with `channels` chosen and its pull request's
// merge just landed, so the next step is the merge's notice
fn just_merged(channels: &str, relay_only: bool) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = Rig::with_pull_request_set("shep", |rig| {
        rig.relay.set_up(true);
        rig.merge_auto();
        rig.set_ruling_channels(channels);
        if relay_only {
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
    (rig, runner, head)
}

#[test]
fn relay_only_sends_the_merge_notice_to_the_relay() {
    let (rig, runner, head) = just_merged(r#"["relay"]"#, true);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed {
            issue: 7,
            pull_request: 71,
        })
    );
    assert!(rig.alerts.posts().is_empty());
    let [(text, _, _)] = rig.relay.sent().try_into().unwrap();
    assert_eq!(
        text,
        format!(
            "[kelpie]\nproject=shep notice=merged\n\n\
             Pull request #71 for issue #7 merged into main at {} on shep, \
             every gate passed. Nothing to answer.",
            &head[..7]
        )
    );
    assert_eq!(rig.relay.clears(), 0, "a notice never ends a question");
    assert_eq!(step(&runner).unwrap(), None, "and is sent once");
    assert_eq!(rig.relay.sent().len(), 1);
}

#[test]
fn a_notice_the_relay_cannot_take_is_kept_and_tried_again() {
    let (rig, runner, _) = just_merged(r#"["relay"]"#, true);
    rig.relay.set_up(false);
    let offset = rig.clock.now().0 - Rig::EPOCH;
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::NoticeFailed {
            issue: 7,
            pull_request: 71,
            reason: "cannot reach the relay: the rig's relay is down".into(),
            retry_at: Timestamp(Rig::EPOCH + offset + 60),
        })
    );
    rig.relay.set_up(true);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed { .. })
    ));
    assert_eq!(rig.relay.sent().len(), 1);
}

#[test]
fn both_channels_keep_the_merge_notice_on_the_webhook_alone() {
    let (rig, runner, _) = just_merged(r#"["webhook", "relay"]"#, false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed { .. })
    ));
    let [(webhook, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(webhook, rig.webhook());
    assert_eq!(alert.title, "kelpie: shep merged #71");
    assert!(rig.relay.sent().is_empty());
}
