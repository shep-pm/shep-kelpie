//! The maintainer's `ready-for-agent` that the check put on and the maintainer
//! took off, which the check does not put back

use std::sync::Mutex;

use serde_json::json;

use super::check_tests::{ASSUMED, FAILING, PASSING, PASSING_LABELS, checking, checks, filed};
use super::pick_tests::{MAINTAINER, list};
use super::ready_tests::said;
use super::research_tests::{WORDS, calls, moves, pump, reply, restart, took_back};
use crate::github::ApiError;
use crate::ports::Role;
use crate::runner::{Pass, Runner, advance, step};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
// A passing line more than `PASSING`, so the edit changes the key and still passes
fn edited(n: u32) -> String {
    format!("{PASSING}\nEdit {n}.\n")
}

// Issue 30 as the forge lists it with `body` and `labels`
fn relisted(rig: &Rig, body: &str, labels: &[&str]) {
    list(rig, filed(rig, MAINTAINER, body, labels));
    rig.next_look();
}

// A rig whose passing issue the check made ready, which the maintainer then took
// the label off
fn taken_off(pick: &str) -> (Rig, Mutex<Runner>) {
    let lines = format!("pick = \"{pick}\"");
    let (rig, runner) = checking(&lines, MAINTAINER, PASSING, &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["label ready-for-agent"]);
    (rig, runner)
}

fn ready_seen(rig: &Rig) -> bool {
    checks(rig)[0]["ready_seen"] == true
}

#[test]
fn a_first_pass_still_labels() {
    let (rig, runner) = checking("pick = \"off\"", MAINTAINER, PASSING, &PASSING_LABELS);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"]);
    assert!(ready_seen(&rig), "{}", checks(&rig));
}

#[test]
fn a_label_the_maintainer_took_off_stays_off_after_an_edit() {
    let (rig, runner) = taken_off("off");

    relisted(&rig, &edited(1), &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"], "nothing put back");
    assert!(ready_seen(&rig), "{}", checks(&rig));
    relisted(&rig, &edited(2), &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["label ready-for-agent"], "still off");
}

#[test]
fn a_label_the_maintainer_took_off_stays_off_after_the_check_version_rises() {
    let (rig, runner) = taken_off("off");
    // A raised `check::VERSION` changes every key, so the kept one no longer matches
    runner.lock().unwrap().state.checks[0].key = 0;

    relisted(&rig, PASSING, &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"], "nothing put back");
    assert_ne!(checks(&rig)[0]["key"], 0, "it was checked again");
    assert!(ready_seen(&rig), "{}", checks(&rig));
}

#[test]
fn a_label_the_maintainer_took_off_stays_off_after_a_failing_edit_and_a_fix() {
    let (rig, runner) = taken_off("off");

    relisted(&rig, FAILING, &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(
        ready_seen(&rig),
        "a failing result keeps it: {}",
        checks(&rig)
    );
    relisted(&rig, &edited(1), &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"], "nothing put back");
}

#[test]
fn a_label_put_back_by_hand_stands_and_can_come_off_again() {
    // Unlabelled and failing, so its first result has no `ready_seen`
    let (rig, runner) = checking("pick = \"off\"", MAINTAINER, FAILING, &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(!ready_seen(&rig), "{}", checks(&rig));

    // The maintainer labels it by hand: the same text, so the same key
    let mut labels = PASSING_LABELS.to_vec();
    labels.push("ready-for-agent");
    relisted(&rig, FAILING, &labels);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(ready_seen(&rig), "seen on the listing: {}", checks(&rig));

    // Fixed, with the label on, it stands: nothing is written
    relisted(&rig, &edited(1), &labels);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new());

    // Taken off again, it stays off
    relisted(&rig, &edited(2), &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new());
}

#[test]
fn kelpies_own_swap_is_not_the_maintainers() {
    let (rig, runner) = checking(
        "pick = \"maintainer\"",
        MAINTAINER,
        FAILING,
        &["ready-for-agent"],
    );
    took_back(&rig, 30, MAINTAINER, "Crash on start", FAILING);
    let runner = restart(&rig, runner);
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.github.label_takes(),
        [(30, "ready-for-agent".to_owned())]
    );
    assert!(!ready_seen(&rig), "{}", checks(&rig));

    // The edit passes: it is ready, as the first pass of any issue is
    let mut labels = PASSING_LABELS.to_vec();
    labels.push("needs-triage");
    relisted(&rig, PASSING, &labels);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig).last().unwrap(), "label ready-for-agent");
}

#[test]
fn a_triage_pass_leaves_it_off() {
    let (rig, runner) = taken_off("maintainer");
    // The edit fails, so the pass runs, and its rewrite would pass
    let found = json!({ "type": "bug", "kind": "fix", "priority": "P2", "body": PASSING });
    rig.claude.script_for(Role::Triager, [reply(found)]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    relisted(&rig, FAILING, &PASSING_LABELS);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 1, "the pass ran");
    assert_eq!(
        rig.github.issue_edits(),
        [],
        "no rewrite is promised or written"
    );
    let moved = moves(&rig);
    assert_eq!(moved[0], "label ready-for-agent");
    assert!(
        !moved[1..].contains(&"label ready-for-agent".to_owned()),
        "{moved:?}"
    );
    assert_eq!(moved.last().unwrap(), "label needs-triage");
}

#[test]
fn a_label_taken_off_while_the_triager_ran_is_not_put_back() {
    // The maintainer labelled it by hand, so the pass begins with it
    let (rig, runner) = checking(
        "pick = \"maintainer\"",
        MAINTAINER,
        FAILING,
        &["ready-for-agent"],
    );
    let found = json!({ "type": "bug", "kind": "fix", "priority": "P2", "body": ASSUMED });
    let hold = Hold::default();
    let held = Scripted::HoldThenSay(hold.clone(), said(found));
    rig.claude.script_for(Role::Triager, [held]);

    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the research never began");
    list(&rig, filed(&rig, MAINTAINER, FAILING, &[]));
    hold.release();
    pump(&runner, || {
        let moved = moves(&rig);
        moved.contains(&"label ready-for-agent".to_owned())
            || moved.contains(&"label needs-triage".to_owned())
    });

    let moved = moves(&rig);
    assert!(
        !moved.contains(&"label ready-for-agent".to_owned()),
        "{moved:?}"
    );
    assert_eq!(moved.last().unwrap(), "label needs-triage");
}

#[test]
fn a_passing_issue_whose_needs_triage_will_not_come_off_gets_no_ready_label_yet() {
    let mut labels = PASSING_LABELS.to_vec();
    labels.push("needs-triage");
    let (rig, runner) = checking("pick = \"maintainer\"", MAINTAINER, PASSING, &labels);
    rig.github.refuse_label_removals(ApiError::Refused(422));

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new(), "nothing went on");

    rig.github.accept_label_removals();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), ["label ready-for-agent"]);
    assert!(ready_seen(&rig), "{}", checks(&rig));
}

#[test]
fn a_label_taken_off_stays_off_when_the_issue_is_closed_and_reopened() {
    let (rig, runner) = taken_off("off");

    rig.forge.unlist_open_issue(30);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    relisted(&rig, &edited(1), &PASSING_LABELS);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"], "nothing put back");
}
