//! An issue whose triage asked questions no one has answered, and the
//! `ready-for-agent` its pass put on by hand, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::ask_tests::{ASK_WORDS, asked, record};
use super::check_tests::PASSING;
use super::pick_tests::{MAINTAINER, issue, list, rig_with};
use super::research_tests::{crashed_with, reply};
use crate::board::{READY, TRIAGE, TRIAGE_GO};
use crate::ports::Role;
use crate::runner::slots_tests::dispatched;
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted, ticket};

// The labels the repo holds, so none is made on the way
const REPO_LABELS: [&str; 5] = ["bug", "kind:fix", "priority: P2", TRIAGE, TRIAGE_GO];

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

#[test]
fn an_issue_with_open_triage_questions_is_held_off_the_board() {
    let (rig, runner) = ready_and_asked("pick = \"label\"\nwait_days = 30");

    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "commented", "{held}");
    assert!(held["questions"]["at"].is_number(), "{held}");
    assert_eq!(held["seen"], json!([READY, "kind:fix", "priority: P2", TRIAGE_GO]));

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
