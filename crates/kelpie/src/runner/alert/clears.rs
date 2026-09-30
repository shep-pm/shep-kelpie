use std::sync::Mutex;

use serde_json::json;

use crate::ports::Clock;
use crate::runner::{Runner, StepReport, step};
use crate::test::Rig;

// Ruling 1 already sent to the relay, then a second ruling raised, as a
// restart reads it
fn second_ruling(rig: &Rig, runner: Mutex<Runner>) -> Mutex<Runner> {
    drop(runner);
    let path = rig.paths().state;
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut second = state["rulings"][0].clone();
    second["id"] = json!(2);
    second["kind"] = json!({ "kind": "closed" });
    second["alerted"] = json!(false);
    second["relayed"] = json!(false);
    state["rulings"].as_array_mut().unwrap().push(second);
    std::fs::write(&path, state.to_string()).unwrap();
    rig.open().unwrap()
}

// The header line of each message sent to the relay, in order
fn sent_headers(rig: &Rig) -> Vec<String> {
    let texts = rig.relay.sent().into_iter().map(|(text, ..)| text);
    texts
        .map(|text| text.lines().nth(1).unwrap().to_owned())
        .collect()
}

fn relayed(rig: &Rig, runner: &Mutex<Runner>) -> Vec<serde_json::Value> {
    let status = rig.ask(runner, "status", None);
    let rulings = status["rulings"].as_array().unwrap();
    rulings.iter().map(|r| r["relayed"].clone()).collect()
}

#[test]
fn the_daily_clear_sends_an_open_ruling_to_the_relay_again() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.relay.set_up(true);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let runner = second_ruling(&rig, runner);
    rig.clock.advance(Rig::DAY);

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.relay.clears(), 2);
    assert_eq!(
        sent_headers(&rig),
        [
            "project=rotom ruling=1 wants=yes-or-no",
            "project=rotom ruling=1 wants=yes-or-no",
            "project=rotom ruling=2 wants=yes-or-no",
        ]
    );
    assert_eq!(
        rig.alerts.posts().len(),
        2,
        "the webhook is not posted twice"
    );
    assert_eq!(relayed(&rig, &runner), [json!(true), json!(true)]);

    rig.ask(&runner, "rule", Some("1 no not yet"));
    step(&runner).unwrap();
    assert_eq!(
        rig.relay.told().len(),
        1,
        "the relay is told it was answered"
    );
}

#[test]
fn a_clear_because_the_relays_files_changed_sends_an_open_ruling_again() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.relay.set_up(true);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let runner = second_ruling(&rig, runner);
    rig.relay.set_stale();

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    assert_eq!(rig.relay.clears(), 2);
    assert_eq!(sent_headers(&rig).len(), 3);
    assert_eq!(rig.alerts.posts().len(), 2);
    assert_eq!(relayed(&rig, &runner), [json!(true), json!(true)]);
}

#[test]
fn a_resend_the_relay_refuses_is_tried_again_and_kept_owed() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.relay.set_up(true);
    step(&runner).unwrap();
    let runner = second_ruling(&rig, runner);
    rig.relay.set_stale();
    rig.relay.set_up(false);

    let Some(StepReport::AlertFailed {
        id: 1, retry_at, ..
    }) = step(&runner).unwrap()
    else {
        panic!("the resend was not tried");
    };
    assert_eq!(relayed(&rig, &runner), [json!(false), json!(false)]);
    rig.relay.set_up(true);
    rig.clock.advance(retry_at.0 - rig.clock.now().0);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(relayed(&rig, &runner)[0], json!(true));
}

#[test]
fn a_resend_waiting_to_be_retried_holds_back_no_newer_ruling() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.relay.set_up(true);
    step(&runner).unwrap();
    let runner = second_ruling(&rig, runner);
    rig.relay.set_stale();
    rig.relay.set_up(false);

    let Some(StepReport::AlertFailed {
        id: 1, retry_at, ..
    }) = step(&runner).unwrap()
    else {
        panic!("the resend was not tried");
    };
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    assert_eq!(rig.alerts.posts().len(), 2, "ruling 2 reached the webhook");

    rig.relay.set_up(true);
    rig.clock.advance(retry_at.0 - rig.clock.now().0);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
}

#[test]
fn a_resend_owed_to_a_channel_since_turned_off_is_not_sent() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.relay.set_up(true);
    step(&runner).unwrap();
    let runner = second_ruling(&rig, runner);
    rig.relay.set_stale();
    rig.relay.set_up(false);
    step(&runner).unwrap();
    drop(runner);

    rig.set_ruling_channels(r#"["webhook"]"#);
    let runner = rig.open().unwrap();
    rig.relay.set_up(true);
    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.relay.sent().len(), 1, "only the first ruling's send");
}
