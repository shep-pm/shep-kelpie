//! `shep kelpie triage <n>`, through the runner's trigger

use std::sync::Arc;

use serde_json::{Value, json};

use super::marker::MARKER;
use super::pick_tests::{issue, list, rig_with, triaging};
use super::research_tests::handed;
use crate::board::TRIAGE;
use crate::github::ApiError;
use crate::ports::{Role, Timestamp};
use crate::runner::{advance, step};
use crate::test::{CALL_PATIENCE, Scripted};

const WORDS: &str = "Thanks for the report. I looked it over.";

const NOTHING: &str = r#"{"type": null, "priority": null, "related": [], "files": [],
    "tests": [], "criteria": [], "open": [], "body": null}"#;

fn refusal(reply: &Value) -> String {
    reply["error"].as_str().expect("a refusal").to_owned()
}

#[test]
fn triage_queues_an_issue_opened_before_pick_was_turned_on() {
    let rig = rig_with("pick = \"all\"");
    let old = issue(&rig, 30, "newcomer", 0);
    rig.clock.advance(100);
    list(&rig, old);
    let runner = rig.open().unwrap();
    rig.claude
        .script_for(Role::Triager, [Scripted::Text(NOTHING)]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);

    // The pick rule leaves it, since it was opened before `since`.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.comments(), []);

    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(reply.get("error"), None, "{reply}");
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.github.comments(), [(30, handed(WORDS))]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["triage"]["issues"][0]["issue"], 30);
    assert_eq!(status["triage"]["issues"][0]["step"], "done");
}

#[test]
fn triage_is_refused_with_pick_off_on_a_labelled_issue_and_on_a_closed_one() {
    let off = rig_with("pick = \"off\"");
    list(&off, issue(&off, 30, "newcomer", 5));
    let runner = off.open().unwrap();
    let reply = off.ask(&runner, "triage", Some("30"));
    assert_eq!(
        refusal(&reply),
        "`triage.pick` is `off`, so no issue is triaged"
    );

    let (rig, runner) = triaging("all");
    let mut labelled = issue(&rig, 31, "newcomer", 5);
    labelled.labels = vec!["bug".to_owned(), "needs-info".to_owned()];
    list(&rig, labelled);
    let reply = rig.ask(&runner, "triage", Some("31"));
    assert_eq!(
        refusal(&reply),
        "#31 carries `needs-info`, so it is not untouched"
    );
    let reply = rig.ask(&runner, "triage", Some("32"));
    assert_eq!(refusal(&reply), "#32 is not an open issue");
    let reply = rig.ask(&runner, "triage", Some("many"));
    assert_eq!(refusal(&reply), "\"many\" is not an issue number");
    let reply = rig.ask(&runner, "triage", None);
    assert_eq!(refusal(&reply), "`triage` takes an issue number");

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.comments(), []);
    assert_eq!(rig.claude.all_seen().len(), 0, "nothing was queued");
}

#[test]
fn triage_is_refused_while_its_call_is_in_flight() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    let hold = crate::test::Hold::default();
    rig.claude
        .script_for(Role::Triager, [Scripted::Hold(hold.clone())]);

    let started = crate::runner::advance(&runner).unwrap();
    assert!(matches!(started, crate::runner::Pass::Started));
    assert!(hold.entered(crate::test::CALL_PATIENCE));

    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(refusal(&reply), "the triage call on #30 is in flight");
    assert_eq!(rig.ask(&runner, "status", None)["triage"]["running"], 30);

    hold.release();
    assert!(hold.answered(crate::test::CALL_PATIENCE));
}

#[test]
fn triage_on_a_recorded_issue_posts_a_new_comment_and_replaces_the_record() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.claude.script_for(
        Role::Triager,
        [Scripted::Text(NOTHING), Scripted::Text(NOTHING)],
    );
    rig.claude.script_for(
        Role::Representative,
        [
            Scripted::Say(WORDS),
            Scripted::Say("Here is a second look."),
        ],
    );
    assert_eq!(step(&runner).unwrap(), None);
    let first = rig.ask(&runner, "status", None)["triage"]["issues"][0].clone();
    assert_eq!(first["step"], "done");
    // The forge now shows the label its own record put on.
    let mut labelled = issue(&rig, 30, "newcomer", 5);
    labelled.labels = vec![TRIAGE.to_owned()];
    list(&rig, labelled);

    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(reply.get("error"), None, "{reply}");
    assert_eq!(step(&runner).unwrap(), None);

    let comments = rig.github.comments();
    assert_eq!(comments.len(), 2, "{comments:?}");
    assert!(comments[1].1.starts_with("Here is a second look."));
    let status = rig.ask(&runner, "status", None);
    let records = status["triage"]["issues"].as_array().unwrap();
    assert_eq!(records.len(), 1, "the record is replaced: {records:?}");
    assert_eq!(records[0]["issue"], 30);
    assert_ne!(records[0]["comment"], first["comment"]);
    assert_eq!(records[0]["step"], "done");
    assert_eq!(json!(records[0]["comment"].as_u64().is_some()), json!(true));
}

#[test]
fn a_triage_asked_again_finds_its_own_comment_after_a_post_that_seemed_to_fail() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.claude.script_for(
        Role::Triager,
        [Scripted::Text(NOTHING), Scripted::Text(NOTHING)],
    );
    rig.claude.script_for(
        Role::Representative,
        [
            Scripted::Say(WORDS),
            Scripted::Say("Here is a second look."),
        ],
    );
    assert_eq!(step(&runner).unwrap(), None);
    let first = rig.ask(&runner, "status", None)["triage"]["issues"][0].clone();
    let mut labelled = issue(&rig, 30, "newcomer", 5);
    labelled.labels = vec![TRIAGE.to_owned()];
    list(&rig, labelled);

    rig.github.refuse_comments(ApiError::Refused(502));
    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(reply.get("error"), None, "{reply}");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.github.comments().len(),
        2,
        "the first, and the refused try"
    );

    // The forge took the second post whose answer was lost.
    let marked = format!("Here is a second look.\n\n{MARKER}");
    let landed = rig
        .forge
        .threads
        .write(30, "kelpie-shep-pm[bot]", &marked, Timestamp(1));
    rig.github.accept_comments();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.github.comments().len(), 2, "no third comment");
    let status = rig.ask(&runner, "status", None);
    let records = status["triage"]["issues"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["comment"], landed);
    assert_ne!(records[0]["comment"], first["comment"]);
    assert_eq!(records[0]["step"], "done");
}

// The poll's listing is read with the runner's lock let go, so the verb can queue an issue
// that listing does not hold. The pass that applies it must not take that listing for a
// fresh one: it would drop the queued issue unasked, or leave it a whole poll.
#[test]
fn an_issue_queued_while_the_triage_listing_was_read_is_triaged_at_once() {
    let rig = rig_with("pick = \"all\"");
    let old = issue(&rig, 30, "newcomer", 0);
    rig.clock.advance(100);
    let runner = Arc::new(rig.open().unwrap());
    rig.claude
        .script_for(Role::Triager, [Scripted::Text(NOTHING)]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    let read = rig.forge.hold_next_listing_read();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(
            read.waiting(CALL_PATIENCE),
            "the pass never reached the listing"
        );
        // Listed after the read began, so the read does not hold it
        list(&rig, old);
        let reply = rig.ask(&runner, "triage", Some("30"));
        assert_eq!(reply.get("error"), None, "{reply}");
        drop(read);
        pass.join().expect("the pass thread").unwrap();
    });
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.comments(),
        [(30, handed(WORDS))],
        "the queued issue was triaged"
    );
}
