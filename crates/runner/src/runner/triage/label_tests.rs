//! `triage.pick = "label"`: someone else's issue is triaged only once the
//! maintainer puts `needs-triage` on it, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::ask_tests::{asked as questions, record};
use super::check_tests::{FAILING, PASSING, PASSING_LABELS, REPO_LABELS, checking, filed};
use super::pick_tests::{MAINTAINER, list, rig_with};
use super::research_tests::{WORDS, calls, crashed_with, moves, reply};
use crate::board::{TRIAGE, TRIAGE_GO};
use crate::ports::{Clock, Role, Timestamp};
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted};

// A rig under `pick = "label"` whose issue 30, by `author`, was opened before
// `pick` was turned on and carries `labels` and the maintainer's go
fn asked(author: &str, body: &str, labels: &[&str]) -> (Rig, Mutex<Runner>) {
    found(author, body, labels, json!({}))
}

// The same, with the triager finding `found`
fn found(author: &str, body: &str, labels: &[&str], found: Value) -> (Rig, Mutex<Runner>) {
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
    rig.claude.script_for(Role::Triager, [reply(found)]);
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

#[test]
fn an_issue_swapped_on_upgrade_is_not_an_approval() {
    let rig = rig_with("pick = \"label\"");
    rig.forge.set_repo_labels(&[TRIAGE_GO]);
    rig.forge.label(30, TRIAGE_GO);
    let mut old = filed(&rig, "newcomer", FAILING, &[TRIAGE]);
    old.created = Timestamp(rig.clock.now().0.saturating_sub(1));
    list(&rig, old);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(rig.github.comments(), []);
    assert_eq!(rig.github.label_takes(), []);
    let renamed = rig.github.writes();
    let renamed = renamed.iter().filter(|(_, c)| c.path.ends_with(TRIAGE_GO));
    assert_eq!(renamed.count(), 1, "the swap renamed the old label");
}

#[test]
fn a_go_on_a_triaged_issue_starts_nothing() {
    let (rig, runner) = asked("newcomer", FAILING, &[TRIAGE]);
    drop(runner);
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": 101, "step": "done",
            "author": "newcomer", "seen": [TRIAGE, TRIAGE_GO],
        }),
    );
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0);
    assert_eq!(rig.github.comments(), []);
    assert_eq!(rig.github.label_takes(), [(30, TRIAGE_GO.to_owned())]);

    let reply = rig.ask(&runner, "triage", Some("30"));
    assert_eq!(reply.get("error"), None, "the verb still asks: {reply}");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}

#[test]
fn a_go_put_on_again_after_an_edit_starts_a_pass() {
    let (rig, runner) = asked("newcomer", FAILING, &[TRIAGE]);
    drop(runner);
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": 101, "step": "done",
            "author": "newcomer", "seen": [TRIAGE],
        }),
    );
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "the old pass saw no go"
    );
}

#[test]
fn a_new_repo_under_label_gets_both_waiting_labels() {
    let rig = rig_with("pick = \"label\"");
    rig.forge.set_repo_labels(&["bug"]);

    let _runner = rig.open().unwrap();

    let made = (rig.github.writes().into_iter())
        .filter_map(|(_, call)| serde_json::from_str::<Value>(call.body.as_deref()?).ok())
        .filter_map(|body| body["name"].as_str().map(str::to_owned))
        .filter(|name| name == TRIAGE || name == TRIAGE_GO);
    assert_eq!(made.collect::<Vec<_>>(), [TRIAGE, TRIAGE_GO]);
}

#[test]
fn an_approved_outside_issue_is_asked_as_under_pick_all() {
    let (rig, runner) = found("newcomer", FAILING, &[], questions());

    assert_eq!(step(&runner).unwrap(), None);

    let held = record(&rig, &runner);
    assert!(held["questions"]["at"].is_number(), "{held}");
    assert_eq!(moves(&rig), ["comment"], "asked, so no label yet");
}
