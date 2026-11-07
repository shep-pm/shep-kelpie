//! A priority the issue already carries, which the pass's own never replaces,
//! through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::pick_tests::list;
use super::research_tests::{crash, crashed_with, moves, reply, untouched};
use crate::ports::Role;
use crate::runner::{Runner, step};
use crate::test::Rig;

// Issue 30 filed carrying `label`, in a repo that names its priorities the
// board's way
fn filed_with(label: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = untouched();
    rig.forge
        .set_repo_labels(&["priority: P1", "priority: P2", "needs-triage"]);
    let mut filed = crash(&rig, "It crashes when I run it.");
    filed.labels = vec![label.to_owned()];
    list(&rig, filed);
    rig.forge.label(30, label);
    (rig, runner)
}

#[test]
fn the_priority_the_issue_was_filed_with_stands_over_the_one_the_pass_ranks() {
    for filed in ["priority: P2", "P2"] {
        let (rig, runner) = filed_with(filed);
        let found = json!({ "priority": "P1" });
        rig.claude.script_for(Role::Triager, [reply(found)]);

        assert_eq!(step(&runner).unwrap(), None, "{filed}");

        assert_eq!(moves(&rig), ["comment", "label needs-triage"], "{filed}");
        assert_eq!(rig.github.label_takes(), [], "{filed}");
    }
}

#[test]
fn a_pass_that_names_the_priority_the_issue_carries_changes_no_label() {
    let (rig, runner) = filed_with("priority: P2");
    let found = json!({ "priority": "P2" });
    rig.claude.script_for(Role::Triager, [reply(found)]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["comment", "label needs-triage"]);
    assert_eq!(rig.github.label_takes(), []);
}

#[test]
fn a_pass_that_names_the_priority_the_issue_carries_under_the_bare_name_changes_no_label() {
    let (rig, runner) = filed_with("P1");
    rig.forge
        .set_repo_labels(&["P1", "priority: P1", "needs-triage"]);
    let found = json!({ "priority": "P1" });
    rig.claude.script_for(Role::Triager, [reply(found)]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["comment", "label needs-triage"]);
    assert_eq!(rig.github.label_takes(), []);
}

#[test]
fn a_priority_put_on_by_hand_while_the_pass_ran_stands_and_the_pass_priority_is_not_put_on() {
    let (rig, runner) = untouched();
    drop(runner);
    rig.forge
        .set_repo_labels(&["priority: P1", "priority: P3", "needs-triage"]);
    let record = json!({
        "issue": 30, "comment": 101, "step": "commented", "labels": ["priority: P1"],
    });
    crashed_with(&rig, record);
    let runner = rig.open().unwrap();
    rig.forge.label(30, "priority: P3");

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label needs-triage"]);
    assert_eq!(rig.github.label_takes(), []);
    assert_eq!(rig.forge.issue_labels(30), ["priority: P3"]);
}
