//! An issue whose triage asked questions no one has answered, and the
//! `ready-for-agent` its pass put on by hand, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::ask_tests::{ASK_WORDS, asked, record};
use super::check_tests::{PASSING, checks};
use super::pick_tests::{MAINTAINER, issue, list, rig_with};
use super::research_tests::{crashed_with, moves, reply};
use crate::board::{READY, TRIAGE, TRIAGE_GO};
use crate::ports::Role;
use crate::runner::slots_tests::dispatched;
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted, ticket};

// The labels the repo holds, so none is made on the way
const REPO_LABELS: [&str; 5] = ["bug", "kind:fix", "priority: P2", TRIAGE, TRIAGE_GO];
const HELD_LABELS: [&str; 8] = [
    "bug",
    "kind:fix",
    "enhancement",
    "P1",
    "P2",
    TRIAGE,
    "needs-info",
    READY,
];

// Issue 30, the maintainer's own, carrying `ready-for-agent` put on by hand and
// the `needs-triage` go that asks for a pass over an issue the check already
// passes. The board and the triage poll read the same labels.
fn ready_by_hand(rig: &Rig) {
    let labels = [READY, "kind:fix", "priority: P2", TRIAGE_GO];
    rig.forge.list_ready(30, false);
    rig.forge.set_ready_body(30, PASSING);
    for label in labels {
        rig.forge.label(30, label);
    }
    let mut listed = issue(rig, 30, MAINTAINER, 5);
    listed.body = PASSING.to_owned();
    listed.labels = labels.iter().map(|l| (*l).to_owned()).collect();
    list(rig, listed);
}

// A rig whose App covers the repo, with `[triage]` set to `lines`, issue 30
// ready by hand, and a triager whose pass asks `asked()`
fn ready_and_asked(lines: &str) -> (Rig, Mutex<Runner>) {
    let rig = rig_with(lines);
    rig.forge.set_repo_labels(&REPO_LABELS);
    ready_by_hand(&rig);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(asked())]);
    let runner = rig.open().unwrap();
    (rig, runner)
}

// A rig whose App covers the repo, with `[triage]` set to `lines`, and issue 30
// the maintainer's own, failing the issue check, its triager replying `found`.
// `hand` puts `ready-for-agent` on before the pass begins, on the board's
// listing and where the issue is read.
fn waiting(lines: &str, hand: bool, found: Value) -> (Rig, Mutex<Runner>) {
    let rig = rig_with(lines);
    rig.forge.set_repo_labels(&HELD_LABELS);
    let mut crash = issue(&rig, 30, MAINTAINER, 5);
    crash.title = "Crash on start".to_owned();
    crash.body = "It crashes when I run it.".to_owned();
    if hand {
        rig.forge.label(30, READY);
        crash.labels.push(READY.to_owned());
    }
    list(&rig, crash);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(found)]);
    let runner = rig.open().unwrap();
    (rig, runner)
}

// What the pass found: a type and a priority, no question and no body
fn settled_labels() -> Value {
    json!({ "type": "bug", "priority": "P2" })
}

#[test]
fn an_issue_with_open_triage_questions_is_held_off_the_board() {
    let (rig, runner) = ready_and_asked("pick = \"label\"\nwait_days = 30");

    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "commented", "{held}");
    assert!(held["questions"]["at"].is_number(), "{held}");
    assert_eq!(
        held["seen"],
        json!([READY, "kind:fix", "priority: P2", TRIAGE_GO])
    );

    assert_eq!(step(&runner).unwrap(), None, "the board holds it");
    let skipped = rig.ask(&runner, "status", None)["skipped"].clone();
    assert_eq!(skipped[0]["reason"], "asked", "{skipped}");
    assert_eq!(skipped[0]["issue"], "30", "{skipped}");

    // Its questions answered, the record `done`, the label still on: the board
    // takes it as any ready issue.
    drop(runner);
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": 101, "step": "done",
            "author": MAINTAINER, "seen": [READY, "kind:fix", "priority: P2", TRIAGE_GO],
        }),
    );
    let runner = rig.open().unwrap();

    assert_eq!(dispatched(step(&runner).unwrap()), (ticket(30), vec![]));
}

#[test]
fn a_lapsed_wait_takes_off_a_ready_label_that_was_there_before_the_pass() {
    let (rig, runner) = waiting("pick = \"maintainer\"\nwait_days = 1", true, asked());

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        moves(&rig),
        ["comment"],
        "no label goes on while the author is asked"
    );

    rig.clock.advance(Rig::DAY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        [
            "comment".to_owned(),
            "label bug".to_owned(),
            "label P2".to_owned(),
            format!("label {TRIAGE}"),
        ]
    );
    assert_eq!(rig.github.label_takes(), [(30, READY.to_owned())]);
    assert_ne!(
        checks(&rig)[0]["ready_seen"],
        json!(true),
        "kelpie took it off"
    );
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done", "{held}");
    assert!(held.get("questions").is_none(), "{held}");
}

#[test]
fn a_ready_label_put_on_during_the_wait_stands() {
    let (rig, runner) = waiting("pick = \"maintainer\"\nwait_days = 2", false, asked());
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["comment"], "the wait holds its labels");

    let mut labelled = issue(&rig, 30, MAINTAINER, 5);
    labelled.title = "Crash on start".to_owned();
    labelled.body = "It crashes when I run it.".to_owned();
    labelled.labels = vec![READY.to_owned()];
    list(&rig, labelled);
    rig.forge.label(30, READY);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["comment"], "kelpie labels it no further");
    assert_eq!(rig.github.label_takes(), []);
    assert_eq!(rig.forge.issue_labels(30), [READY]);
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done", "{held}");
    assert!(held.get("questions").is_none(), "{held}");
}

#[test]
fn a_pass_ending_at_needs_approval_never_leaves_both_labels() {
    let (rig, runner) = waiting("pick = \"maintainer\"", true, settled_labels());

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        [
            "comment".to_owned(),
            "label bug".to_owned(),
            "label P2".to_owned(),
            format!("label {TRIAGE}"),
        ]
    );
    assert_eq!(rig.github.label_takes(), [(30, READY.to_owned())]);
    assert_ne!(
        checks(&rig)[0]["ready_seen"],
        json!(true),
        "kelpie took it off"
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}
