use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::ports::{Clock, Session};
use crate::runner::{OpenError, step};
use crate::state::ids::RulingIds;
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
    rig.reply("koji 1 yes");
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
    rig.alerts
        .reply(&format!("koji 1 no rename it {previous}"), now);
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
        "koji 1 yes".to_owned(),
        format!("1 yes {wrong}"),
        format!("1 yes {stale}"),
        format!("1 yes {}", &code[..5]),
        format!("1 yes {code}0"),
        format!("{code} 1 yes"),
        format!("1 {code}"),
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
    let code = rig.reply("koji 1 yes");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    rig.alerts
        .reply(&format!("koji 2 yes {code}"), rig.clock.now());
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
    rig.reply("koji 2 no not yet");
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
    rig.reply("koji 1 yes");
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
    rig.reply("koji 1 answer merge it");
    let reason = "it takes `yes`, or `no <note>`";
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
    rig.reply("koji 1 yes");
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

    rig.reply("rotom 1 answer  use --dry-run, it matches shep.");
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
fn a_reply_takes_the_same_answers_as_the_terminal_without_the_project() {
    let (rig, runner, _) = alerted("koji");
    rig.reply("1 no rename it");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_ne!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": 1 })
    );
}

#[test]
fn a_yes_by_reply_to_a_question_is_the_answer_s_text() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(
        "Done.\n\n<kelpie-question>\nShall I keep the old flag?\n</kelpie-question>\n",
    )]);
    step(&runner).unwrap();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));

    rig.reply("1 yes");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
    rig.claude.script([Scripted::Say("kept")]);
    step(&runner).unwrap();
    let [_, answered] = rig.claude.calls().try_into().unwrap();
    assert_eq!(
        answered.prompt,
        "The maintainer answered your question:\n\nyes\n"
    );
}

// Every project reads the one topic, and an id names its project.
#[test]
fn a_reply_to_another_project_s_ruling_answers_nothing_here() {
    let (rig, runner, _) = alerted("koji");
    let ids = RulingIds::under(&rig.paths().kelpie_home);
    assert_eq!(ids.claim("rotom", 0), 2);
    rig.reply("2 yes");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
    assert_eq!(lines(&rig), [""; 0]);
}

// Probed in review: an id from before claims, open here, may be another
// project's too when that project's state is open or cannot be read.
#[test]
fn an_unclaimed_id_another_project_may_hold_needs_the_project_named() {
    let (rig, runner, _) = alerted("koji");
    let kelpie = rig.paths().kelpie_home;
    std::fs::remove_file(kelpie.join("rulings/1")).unwrap();
    let lab = kelpie.join("lab/state.json");
    std::fs::create_dir_all(lab.parent().unwrap()).unwrap();
    for (lab_state, why) in [
        ("{".to_owned(), "lab's rulings cannot be read"),
        (
            std::fs::read_to_string(rig.paths().state).unwrap(),
            "ruling 1 is also waiting on lab",
        ),
    ] {
        std::fs::write(&lab, lab_state).unwrap();
        rig.reply("1 yes");
        let reason = format!("{why}, so name the project first: `koji 1 <answer> <code>`");
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ReplyRefused {
                id: 1,
                reason: reason.clone(),
                line_failed: None
            })
        );
        assert_eq!(
            lines(&rig).last().unwrap(),
            &format!("Ruling 1 was not answered: {reason}.")
        );
        assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
        rig.clock.advance(STEP);
    }
    rig.reply("koji 1 yes");
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyAnswered { id: 1 })
    );
}

#[test]
fn reading_resumes_after_the_last_reply_across_a_restart() {
    let (rig, runner, _) = alerted("koji");
    rig.alerts.reply("hello from the phone", rig.clock.now());
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    let floor = Timestamp(rig.clock.now().0 - WINDOW - 3 * STEP);
    assert_eq!(rig.alerts.reads(), [Since::Time(floor)]);
    drop(runner);

    let runner = rig.open().unwrap();
    rig.reply("koji 1 yes");
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
    let old = rig.clock.now().0 - WINDOW - 3 * STEP - 1;
    saved["replies"] = json!({ "last": { "id": "lapsed", "time": old } });
    std::fs::write(&state, saved.to_string()).unwrap();

    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    step(&runner).unwrap();
    let floor = Timestamp(rig.clock.now().0 - WINDOW - 3 * STEP);
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

// Probed in review: a code in a reply that answered nothing stayed
// unclaimed, and a reader of the topic reused it to merge.
#[test]
fn a_right_code_is_spent_even_when_its_reply_answers_nothing() {
    let (rig, runner, _) = alerted("koji");
    let code = rig.reply("koji 1");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    rig.alerts
        .reply(&format!("koji 1 yes {code}"), rig.clock.now());
    rig.clock.advance(READ_EVERY);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyCodeUsed {
            id: 1,
            line_failed: None
        })
    );
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
}

// Ruling ids are per project, and every project reads the one topic.
#[test]
fn a_reply_for_another_project_answers_nothing_here() {
    let (rig, runner, _) = alerted("koji");
    let code = rig.reply("rotom 1 yes");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
    assert_eq!(lines(&rig), [""; 0]);

    // Its code is spent all the same.
    rig.alerts
        .reply(&format!("koji 1 yes {code}"), rig.clock.now());
    rig.clock.advance(READ_EVERY);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyCodeUsed { id: 1, .. })
    ));
}

#[test]
fn five_wrong_codes_turn_answers_off_until_the_terminal_turns_them_on() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let right = rig.code_at(now);
    let wrong = |n: u32| format!("{:06}", (right.parse::<u32>().unwrap() + n) % 1_000_000);
    for n in 1..5 {
        rig.alerts.reply(&format!("koji 1 yes {}", wrong(n)), now);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ReplyIgnored),
            "{n}"
        );
        rig.clock.advance(READ_EVERY);
    }
    rig.alerts.reply(&format!("koji 1 yes {}", wrong(5)), now);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::RepliesLocked { line_failed: None })
    );
    assert_eq!(
        lines(&rig),
        ["Answers from ntfy are off after 5 wrong codes. \
          Turn them back on with `shep kelpie totp --unlock` on the terminal. \
          Anyone can post to a topic whose name they know, so think \
          about moving to a new one."]
    );
    assert!(!lock(&runner).awaits_reply());

    // The right code of the moment answers nothing now, and is not read.
    rig.clock.advance(STEP);
    rig.reply("koji 1 yes");
    rig.clock.advance(READ_EVERY);
    step(&runner).unwrap();
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));

    crate::totp::answers::Answers::in_folder(rig.paths().totp)
        .unlock()
        .unwrap();
    rig.clock.advance(STEP);
    rig.reply("koji 1 yes");
    rig.clock.advance(READ_EVERY);
    let mut answered = false;
    for _ in 0..4 {
        answered |= step(&runner).unwrap() == Some(StepReport::ReplyAnswered { id: 1 });
    }
    assert!(answered);
}

#[test]
fn a_secret_others_may_read_stops_the_runner() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("koji");
    let secret = rig.paths().totp.join("secret");
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    let Err(OpenError::Settings(e)) = rig.open() else {
        panic!("the runner started");
    };
    assert!(e.to_string().contains("may be read by others"), "{e}");
}

// Probed in review: a right code in any shape but the exact one was never
// claimed, so a reader of the topic could reuse it with the exact shape.
#[test]
fn a_right_code_in_any_shape_is_spent_before_a_replay() {
    for shape in [
        "koji 1 no rename it {code}.",
        "{code} koji 1 no rename it",
        "koji 1 no rename it {spaced}",
        "{code}",
        "koji 1 no rename it {wide}",
        "koji 1 no rename it {dashed}",
        "koji 1 no rename it {arabic}",
    ] {
        let (rig, runner, _) = alerted("koji");
        let now = rig.clock.now();
        let code = rig.code_at(now);
        let spaced = format!("{} {}", &code[..3], &code[3..]);
        let wide: String = code
            .chars()
            .map(|c| char::from_u32(u32::from(c) - u32::from('0') + 0xff10).unwrap())
            .collect();
        let dashed = format!("{}-{}", &code[..3], &code[3..]);
        let arabic: String = code
            .chars()
            .map(|c| char::from_u32(u32::from(c) - u32::from('0') + 0x660).unwrap())
            .collect();
        let sent = shape
            .replace("{dashed}", &dashed)
            .replace("{arabic}", &arabic)
            .replace("{code}", &code)
            .replace("{spaced}", &spaced)
            .replace("{wide}", &wide);
        rig.alerts.reply(&sent, now);
        assert_eq!(
            step(&runner).unwrap(),
            Some(StepReport::ReplyIgnored),
            "{sent}"
        );
        rig.alerts.reply(&format!("koji 1 yes {code}"), now);
        rig.clock.advance(READ_EVERY);
        assert!(
            matches!(
                step(&runner).unwrap(),
                Some(StepReport::ReplyCodeUsed { id: 1, .. })
            ),
            "{sent}"
        );
        assert_eq!(
            phase(&rig, &runner),
            json!({ "state": "ruling", "id": 1 }),
            "{sent}"
        );
    }
}

// A reply just too old to act on still spends its code, so a replay just
// inside the window finds it used.
#[test]
fn a_reply_older_than_the_window_is_read_only_to_spend_its_code() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let sent = Timestamp(now.0 - WINDOW - 10);
    let code = rig.code_at(sent);
    rig.alerts
        .reply(&format!("koji 1 no not this {code}"), sent);
    rig.alerts
        .reply(&format!("koji 1 yes {code}"), Timestamp(sent.0 + 20));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyCodeUsed { id: 1, .. })
    ));
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
}

// Probed in review: an attacker's fifth wrong code, read with the
// maintainer's reply, turned answers off before that reply's code was
// spent, and a replay after `--unlock` merged.
#[test]
fn a_code_is_spent_even_when_answers_are_off() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let right = rig.code_at(now);
    let wrong = |n: u32| format!("{:06}", (right.parse::<u32>().unwrap() + n) % 1_000_000);
    for n in 1..=5 {
        rig.alerts.reply(&format!("koji 1 yes {}", wrong(n)), now);
    }
    rig.alerts
        .reply(&format!("koji 1 no rename it {right}"), now);
    for _ in 1..5 {
        assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    }
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::RepliesLocked { .. })
    ));
    step(&runner).unwrap();

    rig.clock.advance(25);
    rig.alerts
        .reply(&format!("koji 1 yes {right}"), rig.clock.now());
    crate::totp::answers::Answers::in_folder(rig.paths().totp)
        .unlock()
        .unwrap();
    rig.clock.advance(READ_EVERY);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyCodeUsed { id: 1, .. })
    ));
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
}

// Only a message's text answers, but a code anywhere in a post is spent:
// in its title, or in a post kelpie reads as its own.
#[test]
fn a_code_in_a_title_or_a_tagged_post_is_spent() {
    for (text, others) in [
        (Some("koji 1 no rename it"), vec!["{code}"]),
        (None, vec!["koji 1 no {code}", "kelpie"]),
    ] {
        let (rig, runner, _) = alerted("koji");
        let now = rig.clock.now();
        let code = rig.code_at(now);
        let others: Vec<String> = others.iter().map(|o| o.replace("{code}", &code)).collect();
        let others: Vec<&str> = others.iter().map(String::as_str).collect();
        rig.alerts.post_raw(text, &others, now);
        rig.alerts.reply(&format!("koji 1 yes {code}"), now);
        let mut used = false;
        for _ in 0..3 {
            used |= matches!(
                step(&runner).unwrap(),
                Some(StepReport::ReplyCodeUsed { id: 1, .. })
            );
        }
        assert!(used, "{text:?} {others:?}");
        assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
    }
}

// ntfy turns a message over 4,095 bytes into an attachment, whose text
// kelpie never reads, so any code in it would stay unspent.
#[test]
fn a_post_too_long_to_read_spends_every_step_it_could_name() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let code = rig.code_at(now);
    rig.alerts.post_cut(
        Some("You received a file: attachment.txt"),
        &["attachment.txt"],
        true,
        now,
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyTooLong { line_failed: None })
    );
    assert_eq!(
        lines(&rig),
        [
            "That reply was too long to read, so it answered nothing and any code \
          in it is spent. Send a shorter reply with a new code."
        ]
    );
    for sent in [now, Timestamp(now.0 + STEP)] {
        rig.alerts.reply(&format!("koji 1 yes {code}"), sent);
        rig.clock.advance(READ_EVERY);
        assert!(
            matches!(
                step(&runner).unwrap(),
                Some(StepReport::ReplyCodeUsed { id: 1, .. })
            ),
            "{sent:?}"
        );
    }
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
}

// A phone whose clock runs ahead sends the next step's code, which a
// reader could replay once that step comes.
#[test]
fn a_code_for_the_next_step_is_spent_before_it_comes() {
    let (rig, runner, _) = alerted("koji");
    let now = rig.clock.now();
    let ahead = rig.code_at(Timestamp(now.0 + STEP));
    rig.alerts
        .reply(&format!("koji 1 no rename it {ahead}"), now);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::ReplyIgnored));
    rig.clock.advance(STEP);
    rig.alerts
        .reply(&format!("koji 1 yes {ahead}"), rig.clock.now());
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReplyCodeUsed { id: 1, .. })
    ));
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
}

// Probed in review: a secret that could not be read, or a claim that could
// not be written, let the read move past the reply with its code unspent,
// and a replay behind it answered.
#[test]
fn a_code_that_cannot_be_spent_holds_the_reply_until_it_can() {
    use std::os::unix::fs::PermissionsExt;
    let set = |path: &std::path::Path, mode| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    for fault in ["the secret others may read", "no claim can be written"] {
        let (rig, runner, _) = alerted("koji");
        let totp = rig.paths().totp;
        let (path, broken, fixed) = if fault.starts_with("the secret") {
            (totp.join("secret"), 0o644, 0o600)
        } else {
            crate::totp::private_dir(&totp.join("used")).unwrap();
            (totp.join("used"), 0o500, 0o700)
        };
        let now = rig.clock.now();
        let code = rig.code_at(now);
        set(&path, broken);
        rig.alerts
            .reply(&format!("koji 1 no rename it {code}"), now);
        rig.alerts.reply(&format!("koji 1 yes {code}"), now);
        let held = step(&runner).unwrap();
        assert!(
            matches!(held, Some(StepReport::RepliesFailed { .. })),
            "{fault}: {held:?}"
        );
        assert_eq!(step(&runner).unwrap(), None, "{fault}: held, not skipped");
        assert_eq!(lines(&rig), [""; 0], "{fault}: nothing says a code is live");

        set(&path, fixed);
        rig.clock.advance(READ_EVERY);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.extend(step(&runner).unwrap());
        }
        assert!(
            seen.contains(&StepReport::ReplyAnswered { id: 1 }),
            "{fault}: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|r| matches!(r, StepReport::ReplyCodeUsed { id: 1, .. })),
            "{fault}: {seen:?}"
        );
        let phase = phase(&rig, &runner);
        assert_ne!(phase["state"], "merge", "{fault}: {phase}");
    }
}

// Probed in review: a reply spends the next step's code, which a replay
// can still send three steps later. With kelpie down, a read that starts
// only two steps before the window missed the reply but read the replay.
#[test]
fn a_replay_of_a_next_step_code_after_a_gap_finds_it_spent() {
    let (rig, runner, _) = alerted("koji");
    // The start of a step to come, so the replay lands two steps later.
    let sent = (rig.clock.now().0 / STEP + 1) * STEP;
    let ahead = rig.code_at(Timestamp(sent + STEP));
    rig.alerts
        .reply(&format!("koji 1 no rename it {ahead}"), Timestamp(sent));
    rig.alerts
        .reply(&format!("koji 1 yes {ahead}"), Timestamp(sent + 85));
    // Kelpie comes back with the replay just inside the window.
    let back = sent + WINDOW + 80;
    rig.clock.advance(back - rig.clock.now().0);
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.extend(step(&runner).unwrap());
    }
    assert!(
        seen.iter()
            .any(|r| matches!(r, StepReport::ReplyCodeUsed { id: 1, .. })),
        "{seen:?}"
    );
    assert_eq!(phase(&rig, &runner), json!({ "state": "ruling", "id": 1 }));
}
