//! `triage.pick = "label"`: someone else's issue is triaged only once the
//! maintainer puts `needs-triage` on it, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::check_tests::{FAILING, PASSING, PASSING_LABELS, REPO_LABELS, checking, filed};
use super::pick_tests::{MAINTAINER, list, rig_with};
use super::research_tests::{WORDS, calls, moves, reply};
use crate::board::{TRIAGE, TRIAGE_GO};
use crate::ports::{Clock, Role, Timestamp};
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted};

// A rig under `pick = "label"` whose issue 30, by `author`, was opened before
// `pick` was turned on and carries `labels` and the maintainer's go
fn asked(author: &str, body: &str, labels: &[&str]) -> (Rig, Mutex<Runner>) {
    let rig = rig_with("pick = \"label\"");
    rig.forge.set_repo_labels(&REPO_LABELS);
    let mut labels = labels.to_vec();
    labels.push(TRIAGE_GO);
    let mut old = filed(&rig, author, body, &labels);
    old.created = Timestamp(rig.clock.now().0.saturating_sub(1));
    rig.forge.label(30, TRIAGE_GO);
    list(&rig, old);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    let runner = rig.open().unwrap();
    (rig, runner)
}

#[test]
fn someone_elses_issue_gets_needs_approval_with_no_call() {
    let (rig, runner) = checking("pick = \"label\"", "newcomer", PASSING, &PASSING_LABELS);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(moves(&rig), [format!("label {TRIAGE}")]);
    assert_eq!(rig.github.comments(), []);
}

#[test]
fn a_failing_issue_of_someone_elses_also_waits_with_no_call() {
    let (rig, runner) = checking("pick = \"label\"", "newcomer", FAILING, &[]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(moves(&rig), [format!("label {TRIAGE}")]);
    assert_eq!(rig.github.comments(), []);
}

#[test]
fn the_go_label_starts_the_triager_on_a_passing_issue() {
    let (rig, runner) = asked("newcomer", PASSING, &PASSING_LABELS);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments().len(), 1);
}

#[test]
fn the_maintainers_own_issue_triages_with_no_label() {
    let (rig, runner) = checking("pick = \"label\"", MAINTAINER, FAILING, &[]);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}

#[test]
fn the_pass_takes_the_go_off_and_ends_at_needs_approval() {
    let (rig, runner) = asked("newcomer", FAILING, &[]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(rig.github.comments().len(), 1);
    let moved = moves(&rig);
    assert_eq!(moved.last(), Some(&format!("label {TRIAGE}")), "{moved:?}");

    // At the next poll the go is spent and comes off. The listing still shows
    // it, since the stand-in's listing does not follow the writes.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.label_takes(), [(30, TRIAGE_GO.to_owned())]);
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "nothing starts again");
    assert_eq!(rig.github.comments().len(), 1);
}
