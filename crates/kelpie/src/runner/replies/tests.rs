use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::{Clock, Session};
use crate::runner::{OpenError, step};
use crate::test::{Rig, Scripted};
use crate::totp::STEP;

// A merge ruling on #71, alerted on the rig's ntfy webhook
fn alerted(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = Rig::parked(project);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    (rig, runner, head)
}

// What kelpie posted to the topic after the alert
fn lines(rig: &Rig) -> Vec<String> {
    let posts = rig.alerts.posts().into_iter().skip(1);
    posts.map(|(_, alert)| alert.text).collect()
}

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

// Rulings 1 and 2 pending, both alerted, with the work item parked on 1
fn two_rulings(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = Rig::parked(project);
    drop(runner);
    let state = rig.paths().state;
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let mut second = saved["rulings"][0].clone();
    second["id"] = json!(2);
    saved["rulings"].as_array_mut().unwrap().push(second);
    saved["last_ruling"] = json!(2);
    std::fs::write(&state, saved.to_string()).unwrap();
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    (rig, runner)
}

#[test]
fn the_alert_says_how_to_reply_and_carries_no_code() {
    let (rig, _runner, _) = alerted("koji");
    let [(webhook, alert)] = rig.alerts.posts().try_into().unwrap();
    assert_eq!(webhook, rig.webhook());
    assert_eq!(
        alert.reply,
        Some(ReplyWith {
            id: 1,
            takes: Takes::YesOrNo
        })
    );
    let code = rig.code_at(rig.clock.now());
    assert!(!alert.text.contains(&code), "{}", alert.text);
    assert!(!alert.text.contains(Rig::TOTP_SECRET), "{}", alert.text);
}

#[test]
fn a_reply_with_the_code_of_the_moment_answers_the_ruling() {
    let (rig, runner, head) = alerted("koji");
    assert!(lock(&runner).awaits_reply());
    rig.reply("1 yes");
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
fn a_code_is_taken_in_its_step_and_the_one_after() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let previous = rig.code_at(Timestamp(now.0 - STEP));
    rig.alerts.reply(&format!("1 no rename it {previous}"), now);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
}

#[test]
fn a_reply_without_the_right_code_is_ignored() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let code = rig.code_at(now);
    let stale = rig.code_at(Timestamp(now.0 - 2 * STEP));
    let wrong = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
    for reply in [
        "1 yes".to_owned(),
        format!("1 yes {wrong}"),
        format!("1 yes {stale}"),
        format!("1 yes {}", &code[..5]),
        format!("1 yes {code}0"),
        format!("{code} 1 yes"),
        format!("1 merge {code}"),
        format!("2 yes {code}"),
        "hello from the phone".to_owned(),
    ] {
        rig.alerts.reply(&reply, now);
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
}

// Anyone reading the topic sees each code the maintainer sends.
#[test]
fn a_code_answers_once_across_every_ruling() {
    let (rig, runner) = two_rulings("koji");
    let code = rig.reply("1 yes");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    rig.alerts.reply(&format!("2 yes {code}"), rig.clock.now());
    rig.clock.advance(READ_EVERY);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyCodeUsed {
            id: 2,
            line_failed: None
        })
    );
    let rulings = rig.ask(&runner, "status", None)["rulings"].clone();
    assert_eq!(rulings.as_array().unwrap().len(), 1);
    assert_eq!(rulings[0]["id"], 2, "ruling 2 still waits");
    assert_eq!(
        lines(&rig)[1..],
        ["Ruling 2 was not answered: that code was used already. \
          Send the reply again with the next one."]
    );

    // The next step's code answers it.
    rig.clock.advance(STEP);
    rig.reply("2 no not yet");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 2 })
    );
}

#[test]
fn a_reply_to_a_settled_ruling_runs_nothing_and_the_topic_is_told() {
    let (rig, runner, _) = alerted("koji");
    rig.ask(&runner, "rule", Some("1 no rename the flag"));
    let status = rig.ask(&runner, "status", None);
    rig.reply("1 yes");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyToSettled {
            id: 1,
            line_failed: None
        })
    );
    assert_eq!(rig.ask(&runner, "status", None), status, "nothing ran");
    assert_eq!(rig.forge.merges(), []);
    assert_eq!(
        lines(&rig),
        ["Ruling 1 on koji is already settled, so that reply ran nothing."]
    );
    let (_, line) = &rig.alerts.posts()[1];
    assert_eq!(line.title, "kelpie: koji ruling 1");
    assert_eq!(line.reply, None);
}

#[test]
fn a_reply_rule_refuses_is_told_on_the_topic() {
    let (rig, runner, _) = alerted("koji");
    rig.reply("1 answer merge it");
    let reason = "ruling 1 is not a question, so it takes a yes, or a no with a note";
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyRefused {
            id: 1,
            reason: reason.into(),
            line_failed: None
        })
    );
    assert_eq!(
        lines(&rig),
        [format!("Ruling 1 was not answered: {reason}.")]
    );
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));

    rig.clock.advance(STEP);
    rig.reply("1 yes");
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
    let reply = rig.alerts.posts()[0].1.reply.unwrap();
    assert_eq!(reply.takes, Takes::Answer);

    rig.reply("1 answer  use --dry-run, it matches shep.");
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
    rig.reply("1 yes");
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
    let (rig, runner, _) = alerted("koji");
    rig.alerts.reply("hello from the phone", rig.clock.now());
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    let floor = Timestamp(rig.clock.now().0 - WINDOW);
    assert_eq!(rig.alerts.reads(), [Since::Time(floor)]);
    drop(runner);

    let runner = rig.open().unwrap();
    rig.reply("1 yes");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    // The alert was m1 and the phone's hello m2.
    assert_eq!(rig.alerts.reads()[1], Since::After("m2".into()));
}

// A position saved long ago may name a message ntfy no longer holds, and
// then a read returns the topic's whole cache.
#[test]
fn an_old_position_reads_from_the_window_instead() {
    let (rig, runner, _) = Rig::parked("koji");
    drop(runner);
    let state = rig.paths().state;
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let old = rig.clock.now().0 - WINDOW - 1;
    saved["replies"] = json!({ "last": { "id": "lapsed", "time": old } });
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    step(&runner).unwrap();
    let floor = Timestamp(rig.clock.now().0 - WINDOW);
    assert_eq!(rig.alerts.reads(), [Since::Time(floor)]);
}

#[test]
fn the_topic_is_read_every_few_seconds_less_while_it_fails_and_not_long_after() {
    let (rig, runner, _) = alerted("koji");
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
    rig.alerts.set_down(false);

    // An hour after the last ruling waiting on it, the topic is let be.
    rig.ask(&runner, "rule", Some("1 no try again"));
    rig.ask(&runner, "pause", None);
    rig.clock.advance(LATE);
    step(&runner).unwrap();
    let reads = rig.alerts.reads().len();
    rig.clock.advance(READ_EVERY);
    step(&runner).unwrap();
    assert_eq!(rig.alerts.reads().len(), reads);
}

#[test]
fn a_discord_webhook_says_nothing_of_replies_and_is_never_read() {
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

#[test]
fn with_no_secret_yet_ntfy_is_alerted_and_never_read() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        std::fs::remove_file(rig.paths().totp.join("secret")).unwrap();
    });
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(rig.alerts.posts()[0].1.reply, None);
    step(&runner).unwrap();
    assert_eq!(rig.alerts.reads(), []);
}

#[test]
fn a_secret_kelpie_did_not_write_stops_the_runner_naming_the_file() {
    let rig = Rig::new("koji");
    let secret = rig.paths().totp.join("secret");
    std::fs::write(&secret, "hunter2\n").unwrap();
    let Err(OpenError::Settings(e)) = rig.open() else {
        panic!("the runner started");
    };
    let e = e.to_string();
    assert!(e.contains(&secret.display().to_string()), "{e}");
    assert!(!e.contains("hunter2"), "{e}");
}
