use crate::ports::{Clock, Timestamp};
use crate::runner::{OpenError, StepReport, step};
use crate::test::Rig;

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
    assert!(err.contains("`webhook` table"), "{err}");

    rig.set_ruling_channels(r#"["relay"]"#);
    rig.open().expect("relay only needs no webhook");

    std::fs::remove_file(rig.paths().kelpie_settings).unwrap();
    rig.open().expect("relay only needs no settings file");
    rig.edit_settings(|s| s.replacen("[\"relay\"]", "[\"webhook\"]", 1));
    let err = rig
        .open()
        .expect_err("no file to hold the webhook")
        .to_string();
    assert!(err.contains("`webhook` table"), "{err}");
}
