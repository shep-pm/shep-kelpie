use serde_json::{Value, json};
use shep_client::dogs::{Destructive, Pane, Row, Section};
use shep_client::shep_core::config::{AppConfig, DogTable};
use shep_client::shep_core::protocol::{Request, SelectorSpec};

use super::*;
use crate::test::{CALL_PATIENCE, FakeShepherd};

fn koji() -> Value {
    json!({
        "project": "koji",
        "active_items": 2,
        "working": ["71"],
        "waiting_for_slot": [],
        "parked": ["72"],
        "work_items": [
            {
                "issue": "71", "title": "Add a thing", "agent": "opus-high",
                "phase": { "state": "ci", "head": null, "since": 5 },
            },
            {
                "issue": "72", "title": "Fix a bug", "agent": "sonnet-high",
                "phase": { "state": "ruling", "id": "73" },
            },
        ],
        "rulings": [{
            "id": "73", "issue": "72", "question": "Merge the fix?\nIt is green.",
            "pull_request": 73, "kind": { "kind": "merge", "head": "abc" },
        }],
    })
}

fn rotom() -> Value {
    json!({
        "project": "rotom",
        "active_items": 1,
        "working": [],
        "waiting_for_slot": [],
        "parked": ["71"],
        "run": "finishing",
        "work_items": [{
            "issue": "71", "title": "Move it", "agent": "opus-high",
            "phase": { "state": "ruling", "id": "71" },
        }],
        "rulings": [{
            "id": "71", "issue": "71", "question": "Which way?",
            "pull_request": null, "kind": { "kind": "question" },
        }],
    })
}

// A kelpie runner, running, that answers `status` with `status`.
fn runner(shepherd: &FakeShepherd, name: &str, status: &Value) {
    let mut app = AppConfig::minimal(name, "/opt/kelpie");
    app.dogs
        .insert("kelpie".into(), DogTable::from(serde_json::Map::new()));
    shepherd.holds(app, true);
    shepherd.says(name, &[&status.to_string()]);
}

fn two_runners() -> FakeShepherd {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", &koji());
    runner(&shepherd, "rotom", &rotom());
    shepherd
}

async fn seen(shepherd: &FakeShepherd) -> Seen {
    let client = shepherd.connect().await;
    tokio::time::timeout(CALL_PATIENCE, Seen::look(&client))
        .await
        .expect("the look neither ended nor failed in time")
        .unwrap()
}

fn trigger(project: &str, action: &str, params: Option<&str>) -> Request {
    Request::Trigger {
        selector: SelectorSpec::Name(project.into()),
        action: action.into(),
        params: params.map(str::to_owned),
    }
}

#[tokio::test]
async fn the_pane_lists_each_runners_items_and_rulings() {
    let shepherd = two_runners();
    let seen = seen(&shepherd).await;

    let expected = Pane::new()
        .fields(
            "board",
            [
                ("koji slots", "1 of 2"),
                ("koji phase", "picking"),
                ("rotom slots", "0 of 1"),
                ("rotom phase", "finishing"),
            ],
        )
        .table(
            "work items",
            ["PROJECT", "ISSUE", "TITLE", "PHASE", "AGENT"],
            [
                Row::new("koji#71", ["koji", "71", "Add a thing", "ci", "opus-high"]),
                Row::new(
                    "koji#72",
                    ["koji", "72", "Fix a bug", "ruling", "sonnet-high"],
                ),
                Row::new(
                    "rotom#71",
                    ["rotom", "71", "Move it", "ruling", "opus-high"],
                ),
            ],
        )
        .table(
            "rulings",
            ["PROJECT", "ID", "KIND", "QUESTION"],
            [
                Row::new("koji#73", ["koji", "73", "merge", "Merge the fix?"]),
                Row::new("rotom#71", ["rotom", "71", "question", "Which way?"]),
            ],
        )
        .row_action("rule", "Say yes", Destructive::Yes)
        .action("pause", "Pause every runner", Destructive::Yes);

    assert_eq!(build(&seen), expected);
}

#[tokio::test]
async fn a_ruling_row_is_keyed_by_project_and_id() {
    let shepherd = two_runners();
    let pane = build(&seen(&shepherd).await);

    let Some(Section::Table(rulings)) = pane.sections.last() else {
        panic!("the rulings table comes last: {pane:?}");
    };
    let keys: Vec<_> = rulings.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["koji#73", "rotom#71"]);
    // Two projects can each have a ruling or item `71`: lookout refuses a pane that repeats a key.
    let body = pane.body().expect("no key repeats");
    assert_eq!(Pane::read(&body).unwrap().sections, pane.sections);
}

#[tokio::test]
async fn rule_forwards_yes_to_the_rulings_project() {
    let mut shepherd = two_runners();
    let seen = seen(&shepherd).await;
    shepherd.writes();

    let (body, forwards) = act(&seen, "rule", Some(r##"{"row":"koji#73"}"##));
    let client = shepherd.connect().await;
    tokio::time::timeout(CALL_PATIENCE, forward(&client, forwards))
        .await
        .expect("the forward neither ended nor failed in time");

    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "rule": "koji#73", "answer": "yes" })
    );
    assert_eq!(shepherd.writes(), [trigger("koji", "rule", Some("73 yes"))]);
}

#[tokio::test]
async fn rule_refuses_a_row_it_cannot_read() {
    let shepherd = two_runners();
    let seen = seen(&shepherd).await;

    for params in [
        None,
        Some("koji#73"),
        Some("{}"),
        Some(r##"{"row":"koji"}"##),
        Some(r##"{"row":"nowhere#1"}"##),
        Some(r##"{"row":"koji#99"}"##),
        // An id that would carry another answer to the runner
        Some(r##"{"row":"koji#73 rework not this"}"##),
    ] {
        let (body, forwards) = act(&seen, "rule", params);
        let error = serde_json::from_str::<Value>(&body).unwrap();
        assert!(error["error"].is_string(), "{params:?}: {body}");
        assert!(forwards.is_empty(), "{params:?}: {forwards:?}");
    }
}

#[tokio::test]
async fn pause_sends_pausing_to_every_runner() {
    let mut shepherd = two_runners();
    let seen = seen(&shepherd).await;
    shepherd.writes();

    let (_, forwards) = act(&seen, "pause", None);
    let client = shepherd.connect().await;
    tokio::time::timeout(CALL_PATIENCE, forward(&client, forwards))
        .await
        .expect("the forward neither ended nor failed in time");

    assert_eq!(
        shepherd.writes(),
        [
            trigger("koji", "pausing", None),
            trigger("rotom", "pausing", None)
        ]
    );
}
