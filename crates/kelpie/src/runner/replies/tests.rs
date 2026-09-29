use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::{Clock, Session};
use crate::runner::step;
use crate::test::{Rig, Scripted};

// A merge ruling on #71, alerted on the rig's ntfy webhook, and its code
fn alerted(project: &str) -> (Rig, Mutex<Runner>, String, String) {
    let (rig, runner, head) = Rig::parked(project);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let code = code_of(&rig, 0);
    (rig, runner, head, code)
}

// The code the `nth` post carried
fn code_of(rig: &Rig, nth: usize) -> String {
    let (_, alert) = &rig.alerts.posts()[nth];
    let reply = alert.reply.as_ref().expect("the alert carries a reply");
    reply.code.expose().to_owned()
}

// What kelpie posted to the topic after the alert
fn lines(rig: &Rig) -> Vec<String> {
    let posts = rig.alerts.posts().into_iter().skip(1);
    posts.map(|(_, alert)| alert.text).collect()
}

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

#[test]
fn the_alert_carries_the_rulings_code_and_buttons_by_what_it_takes() {
    let (rig, _runner, _, code) = alerted("koji");
    let [(webhook, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(webhook, rig.webhook());
    let reply = alert.reply.unwrap();
    assert_eq!(
        (reply.id, reply.takes),
        (1, Takes::YesOrNo { yes: "Merge" })
    );
    assert_eq!(code.len(), 8);
    let alphabet = "0123456789abcdefghjkmnpqrstvwxyz";
    assert!(code.chars().all(|c| alphabet.contains(c)), "{code}");
    assert!(
        !alert.text.contains(&code),
        "the code rides beside the question"
    );
}

#[test]
fn a_reply_with_the_right_code_answers_the_ruling() {
    let (rig, runner, head, code) = alerted("koji");
    assert!(lock(&runner).awaits_reply());
    // A phone keyboard may capitalise it.
    rig.alerts.reply(&format!("1 yes {}", code.to_uppercase()));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    assert_eq!(
        phase(&rig, &runner),
        json!({ "state": "merge", "head": head, "readied": null })
    );
    assert!(!lock(&runner).awaits_reply());
    assert_eq!(lines(&rig), [""; 0], "an answer posts nothing");
}

#[test]
fn a_reply_without_the_right_code_is_ignored() {
    let (rig, runner, _, code) = alerted("koji");
    let (other, other_runner, _, other_code) = alerted("rotom");
    assert_ne!(code, other_code);
    let wrong = if code.starts_with('a') { "b" } else { "a" };
    for reply in [
        "1 yes".to_owned(),
        format!("1 yes {wrong}{}", &code[1..]),
        format!("1 yes {}", &code[..7]),
        format!("1 yes {code}x"),
        format!("1 yes {other_code}"),
        format!("2 yes {code}"),
        format!("{code} 1 yes"),
        format!("1 merge {code}"),
        "hello from the phone".to_owned(),
    ] {
        rig.alerts.reply(&reply);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ReplyIgnored),
            "{reply}"
        );
        rig.clock.advance(READ_EVERY);
    }
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
    assert_eq!(lines(&rig), [""; 0], "nothing is posted back");

    // The other project's ruling 1 is untouched by the first project's code.
    other.alerts.reply(&format!("1 yes {code}"));
    assert_eq!(step(&other_runner).unwrap(), Some(StepReport::ReplyIgnored));
    assert_eq!(
        phase(&other, &other_runner),
        json!({ "state": "ruling", "id": 1 })
    );
}

#[test]
fn a_replayed_code_runs_nothing_and_the_topic_is_told() {
    let (rig, runner, _, code) = alerted("koji");
    rig.alerts.reply(&format!("1 no rename the flag {code}"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["turn"]["state"], "next");

    for replay in [
        format!("1 yes {code}"),
        format!("1 no rename the flag {code}"),
    ] {
        rig.alerts.reply(&replay);
        rig.clock.advance(READ_EVERY);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ReplyToSettled { id: 1, told: None }),
            "{replay}"
        );
    }
    assert_eq!(rig.ask(&runner, "status", None), status, "nothing ran");
    assert_eq!(rig.forge.merges(), []);
    assert_eq!(
        lines(&rig),
        ["Ruling 1 is already settled, so that reply ran nothing."; 2]
    );
    let (_, line) = &rig.alerts.posts()[1];
    assert_eq!(line.title, "kelpie: koji ruling 1");
    assert_eq!(line.reply, None);
}

#[test]
fn a_ruling_settled_by_trigger_tells_a_late_tap_so() {
    let (rig, runner, _, code) = alerted("koji");
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.alerts.reply(&format!("1 yes {code}"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyToSettled { id: 1, told: None })
    );
}

#[test]
fn a_reply_rule_refuses_is_told_on_the_topic() {
    let (rig, runner, _, code) = alerted("koji");
    rig.alerts.reply(&format!("1 answer merge it {code}"));
    let reason = "ruling 1 is not a question, so it takes a yes, or a no with a note";
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyRefused {
            id: 1,
            reason: reason.into(),
            told: None
        })
    );
    assert_eq!(
        lines(&rig),
        [format!("Ruling 1 was not answered: {reason}.")]
    );
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));

    // The code still answers it the right way.
    rig.alerts.reply(&format!("1 yes {code}"));
    rig.clock.advance(READ_EVERY);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
}

#[test]
fn a_questions_answer_by_reply_is_the_workers_next_turn() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(
        "Done.\n\n<kelpie-question>\nShould it be `--dry-run` or `--check`?\n</kelpie-question>\n",
    )]);
    step(&runner).unwrap();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let reply = rig.alerts.posts()[0].1.reply.clone().unwrap();
    assert_eq!(reply.takes, Takes::Answer);
    let code = reply.code.expose();

    rig.alerts
        .reply(&format!("1 answer  use --dry-run, it matches shep. {code}"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    rig.claude.script([Scripted::Say("renamed")]);
    step(&runner).unwrap();
    let [first, answered] = rig.claude.calls().try_into().unwrap();
    assert_eq!(
        answered.session,
        Session::Resume(first.session.id().clone())
    );
    assert_eq!(
        answered.prompt,
        "The maintainer answered your question:\n\nuse --dry-run, it matches shep.\n"
    );
}

#[test]
fn a_relayed_ruling_answered_by_reply_is_told_to_the_relay() {
    let (rig, runner, _) = Rig::parked("golbat");
    rig.relay.set_up(true);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let code = code_of(&rig, 0);
    rig.alerts.reply(&format!("1 yes {code}"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    step(&runner).unwrap();
    let [told] = rig.relay.told().try_into().unwrap();
    assert!(told.contains("ruling=1 settled=yes"), "{told}");
}

#[test]
fn reading_resumes_after_the_last_reply_across_a_restart() {
    let (rig, runner, _, code) = alerted("koji");
    rig.alerts.reply("hello from the phone");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    let first = Rig::EPOCH;
    assert!(matches!(rig.alerts.reads()[..], [Since::Time(Timestamp(at))] if at >= first));
    drop(runner);

    let runner = rig.open().unwrap();
    rig.alerts.reply(&format!("1 yes {code}"));
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    // The alert was m1 and the phone's hello m2.
    assert_eq!(rig.alerts.reads()[1], Since::After("m2".into()));
}

#[test]
fn the_topic_is_read_at_most_every_few_seconds_and_less_while_it_fails() {
    let (rig, runner, _, _) = alerted("koji");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.alerts.reads().len(), 1);
    rig.clock.advance(READ_EVERY);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.alerts.reads().len(), 2);

    rig.alerts.set_down(true);
    rig.clock.advance(READ_EVERY);
    let now = rig.clock.now().0;
    let failed = |at| StepReport::RepliesFailed {
        reason: "the webhook answered HTTP 503".into(),
        retry_at: Timestamp(at),
    };
    assert_eq!(step(&runner).unwrap(), Some(failed(now + READ_EVERY)));
    rig.clock.advance(READ_EVERY);
    assert_eq!(step(&runner).unwrap(), Some(failed(now + 3 * READ_EVERY)));
    rig.clock.advance(READ_EVERY);
    assert_eq!(step(&runner).unwrap(), None, "not before its retry");
}

#[test]
fn a_settled_rulings_code_is_let_go_after_a_day() {
    let (rig, runner, _, code) = alerted("koji");
    rig.ask(&runner, "rule", Some("1 no try again"));
    rig.ask(&runner, "pause", None);
    step(&runner).unwrap();
    rig.clock.advance(Rig::DAY + READ_EVERY);
    step(&runner).unwrap();
    let reads = rig.alerts.reads().len();
    rig.alerts.reply(&format!("1 yes {code}"));
    rig.clock.advance(READ_EVERY);
    step(&runner).unwrap();
    assert_eq!(rig.alerts.reads().len(), reads, "nothing left to read for");
    assert_eq!(lines(&rig), [""; 0]);
}

#[test]
fn a_discord_webhook_carries_no_code_and_is_never_read() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        rig.set_kelpie_settings(&format!(
            "[webhook]\nkind = \"discord\"\nurl = \"{}\"\n",
            Rig::WEBHOOK_URL
        ));
    });
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(rig.alerts.posts()[0].1.reply, None);
    rig.clock.advance(READ_EVERY);
    step(&runner).unwrap();
    assert_eq!(rig.alerts.reads(), []);
    assert!(!lock(&runner).awaits_reply());
}
