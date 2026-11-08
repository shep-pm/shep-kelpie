//! The one-time swap of the waiting label, through the runner's stand-ins

use serde_json::json;

use super::ask_tests::record;
use super::check_tests::{PASSING, PASSING_LABELS, filed};
use super::pick_tests::MAINTAINER;
use super::pick_tests::{list, rig_with};
use super::research_tests::{WORDS, calls, crash, crashed_with, moves, reply};
use crate::board::{TRIAGE, TRIAGE_GO};
use crate::github::ApiError;
use crate::ports::Role;
use crate::runner::step;
use crate::test::{Rig, Scripted};

// A rig whose repo holds `labels`, which an issue 30 carries `on`
fn repo_with(labels: &[&str], on: &[&str]) -> Rig {
    let rig = Rig::new("shep");
    rig.forge.set_repo_labels(labels);
    for label in on {
        rig.forge.label(30, label);
    }
    rig
}

// The repo's two waiting labels, sorted, among the labels it holds
fn waiting_labels(rig: &Rig) -> Vec<String> {
    let mut labels = rig.forge.repo_labels_now();
    labels.retain(|l| l == TRIAGE || l == TRIAGE_GO);
    labels.sort();
    labels
}

// What the forge was told about the two waiting labels, in order
fn swap_writes(rig: &Rig) -> Vec<String> {
    let mut writes = rig.forge.issue_writes();
    writes.retain(|w| w.contains("needs-"));
    writes
}

#[test]
fn an_issue_on_the_old_label_ends_on_needs_approval() {
    let rig = repo_with(&[TRIAGE_GO], &[TRIAGE_GO]);

    let _runner = rig.open().unwrap();

    assert_eq!(rig.forge.issue_labels(30), [TRIAGE]);
    assert_eq!(waiting_labels(&rig), [TRIAGE, TRIAGE_GO]);
    assert_eq!(
        swap_writes(&rig),
        [
            format!("label renamed: {TRIAGE_GO} to {TRIAGE}"),
            format!("label made: {TRIAGE_GO}"),
        ]
    );
}

#[test]
fn a_second_start_writes_nothing() {
    let rig = repo_with(&[TRIAGE_GO], &[TRIAGE_GO]);
    drop(rig.open().unwrap());
    let written = rig.forge.issue_writes();

    let _runner = rig.open().unwrap();

    assert_eq!(
        rig.forge.issue_writes()[written.len()..],
        Vec::<String>::new()
    );
}

#[test]
fn both_labels_in_the_repo_count_as_swapped() {
    let rig = repo_with(&[TRIAGE, TRIAGE_GO], &[TRIAGE]);

    let _runner = rig.open().unwrap();

    assert_eq!(swap_writes(&rig), Vec::<String>::new());
    assert_eq!(rig.forge.issue_labels(30), [TRIAGE]);
}

#[test]
fn a_repo_with_neither_label_is_left_as_it_is() {
    let rig = repo_with(&["bug"], &[]);

    let _runner = rig.open().unwrap();

    assert_eq!(waiting_labels(&rig), Vec::<String>::new());
}

#[test]
fn a_swap_cut_after_the_rename_finishes_on_the_next_try() {
    let rig = repo_with(&[TRIAGE], &[TRIAGE]);

    let _runner = rig.open().unwrap();

    assert_eq!(swap_writes(&rig), [format!("label made: {TRIAGE_GO}")]);
    assert_eq!(waiting_labels(&rig), [TRIAGE, TRIAGE_GO]);
}

#[test]
fn a_local_project_swaps_nothing() {
    let rig = repo_with(&[TRIAGE_GO], &[TRIAGE_GO]);
    rig.tickets_local();

    let _runner = rig.open().unwrap();

    assert_eq!(waiting_labels(&rig), [TRIAGE_GO]);
    assert_eq!(rig.forge.issue_labels(30), [TRIAGE_GO]);
}

#[test]
fn a_project_with_triage_off_still_swaps() {
    let rig = repo_with(&[TRIAGE_GO], &[TRIAGE_GO]);

    let runner = rig.open().unwrap();

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["triage"], serde_json::Value::Null, "triage is off");
    assert_eq!(rig.forge.issue_labels(30), [TRIAGE]);
}

// The App's writes that name label `name`, in order
fn app_writes(rig: &Rig, name: &str) -> Vec<String> {
    let writes = rig.github.writes();
    let named = writes.iter().filter(|(_, call)| call.path.ends_with(name));
    named
        .map(|(_, call)| format!("{:?} {}", call.verb, call.path))
        .collect()
}

// A rig whose App covers the repo and refuses to rename labels, on a repo that
// holds only the old label and a `kind:fix`, with triage on for everyone
fn refused() -> Rig {
    refused_with("pick = \"all\"")
}

fn refused_with(lines: &str) -> Rig {
    let rig = rig_with(lines);
    rig.forge
        .set_repo_labels(&[TRIAGE_GO, "kind:fix", "ready-for-agent"]);
    rig.github.refuse_label_renames(ApiError::Refused(403));
    rig
}

#[test]
fn a_failed_swap_retries_on_the_next_poll_and_holds_triage() {
    let rig = refused();
    let runner = rig.open().unwrap();
    list(&rig, crash(&rig, "It crashes when I run it."));
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 0, "triage is held");
    assert_eq!(rig.github.comments(), []);
    let tried = app_writes(&rig, "needs-triage").len();
    assert!(tried >= 2, "the poll retried the rename");

    rig.github.accept_label_renames();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    // The listing that poll held predates the swap, so it is read again.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "triage starts once swapped"
    );
    let tried_again = app_writes(&rig, "needs-triage");
    assert!(tried_again.len() > tried, "{tried_again:?}");
}

#[test]
fn nothing_makes_needs_approval_while_the_swap_is_owed() {
    let rig = refused();
    list(&rig, filed(&rig, "newcomer", PASSING, &PASSING_LABELS));
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        Vec::<String>::new(),
        "no label is made or put on"
    );

    rig.github.accept_label_renames();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    // The listing that poll held predates the swap, so it is read again.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        [format!("make {TRIAGE}"), format!("label {TRIAGE}")]
    );
}

#[test]
fn a_saved_seen_label_is_mapped_so_it_does_not_read_as_a_hand_label() {
    let rig = rig_with("pick = \"all\"");
    drop(rig.open().unwrap());
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": 101, "step": "done",
            "author": "newcomer", "seen": [TRIAGE_GO, "bug"],
        }),
    );
    rig.forge.set_repo_labels(&[TRIAGE_GO]);

    let runner = rig.open().unwrap();

    assert_eq!(record(&rig, &runner)["seen"], json!([TRIAGE, "bug"]));
}

#[test]
fn a_pass_left_commented_waits_out_the_swap_instead_of_failing_on_it() {
    let rig = refused();
    list(&rig, crash(&rig, "It crashes when I run it."));
    drop(rig.open().unwrap());
    crashed_with(
        &rig,
        json!({"issue": 30, "comment": 101, "step": "commented", "author": "newcomer"}),
    );
    let runner = rig.open().unwrap();
    for _ in 0..6 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(moves(&rig), Vec::<String>::new());

    rig.github.accept_label_renames();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    // The listing that poll held predates the swap, so it is read again.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        [format!("make {TRIAGE}"), format!("label {TRIAGE}")]
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}

#[test]
fn a_listing_read_before_the_swap_is_not_acted_on() {
    let rig = refused_with("pick = \"maintainer\"");
    let mut labels = PASSING_LABELS.to_vec();
    labels.push(TRIAGE_GO);
    list(&rig, filed(&rig, MAINTAINER, PASSING, &labels));
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new(), "owed: nothing is read");

    rig.github.accept_label_renames();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(moves(&rig), Vec::<String>::new(), "the listing is stale");
    assert_eq!(rig.github.label_takes(), []);

    let mut labels = PASSING_LABELS.to_vec();
    labels.push(TRIAGE);
    list(&rig, filed(&rig, MAINTAINER, PASSING, &labels));
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(moves(&rig), ["label ready-for-agent"]);
    assert_eq!(rig.github.label_takes(), [(30, TRIAGE.to_owned())]);
}
