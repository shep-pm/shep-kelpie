//! The swap a `taken` record owes, and the maintainer's hand-labelled
//! `ready-for-agent` issues that are taken back by nothing, through the runner's
//! stand-ins

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use super::ask_tests::{A_WORDS, asked, filed_before, record, settled_ready, wrote};
use super::check_tests::{PASSING, PASSING_LABELS, checks};
use super::pick_tests::{MAINTAINER, issue, list, rig_with};
use super::research_tests::{calls, moves, nothing, reply, took_back, triage};
use crate::board::{TRIAGE, TRIAGE_GO};
use crate::github::ApiError;
use crate::ports::Role;
use crate::runner::slots_tests::{ASKS, dispatched, issue_of};
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted};

const PICK: &str = "pick = \"maintainer\"";

// Issue 30, which the issue check refuses
const TITLE: &str = "Crash on start";
const BODY: &str = "It crashes when I run it.";

// Issue 30 by `author`, as the forge lists it with `ready-for-agent` put on by hand
fn hand_labelled(rig: &Rig, author: &str) {
    let mut crash = issue(rig, 30, author, 5);
    crash.title = TITLE.to_owned();
    crash.body = BODY.to_owned();
    crash.labels = vec!["ready-for-agent".to_owned()];
    list(rig, crash);
}

// Issue 30 as the forge lists it once `labels` are on it
fn relist(rig: &Rig, labels: &[&str]) {
    let mut listed = issue(rig, 30, MAINTAINER, 5);
    listed.title = TITLE.to_owned();
    listed.body = BODY.to_owned();
    listed.labels = labels.iter().map(|&l| l.to_owned()).collect();
    list(rig, listed);
}

// A rig whose first triager reply asks a question, with issue 30 labelled by hand
// and ready on the board, which is what the board would start
fn on_the_board(lines: &str, author: &str) -> (Rig, Mutex<Runner>) {
    filed_before(lines, author, asked(), |rig| {
        hand_labelled(rig, author);
        rig.forge.list_ready(30, false);
    })
}

// `runner` reopened on what an older build left when it took issue 30 back
fn reopened_as_taken(rig: &Rig, runner: Mutex<Runner>) -> Mutex<Runner> {
    drop(runner);
    took_back(rig, 30, MAINTAINER, TITLE, BODY);
    rig.open().unwrap()
}

// A `taken` record finishes its swap, and the pass starts ahead of the board's pick
#[test]
fn a_taken_record_an_older_build_saved_still_finishes_its_swap() {
    let (rig, runner) = on_the_board(PICK, MAINTAINER);
    let runner = reopened_as_taken(&rig, runner);

    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the triage call, not the board"
    );

    assert_eq!(
        rig.github.label_takes(),
        [(30, "ready-for-agent".to_owned())]
    );
    assert_eq!(
        moves(&rig)[..2],
        [format!("label {TRIAGE}"), "comment".to_owned()]
    );
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["working"],
        serde_json::json!([]),
        "no work item: {status}"
    );
}

// A swap owed on the labels waits, and is not counted as the issue's failure
#[test]
fn a_taken_record_waits_out_a_label_swap_that_is_owed() {
    let (rig, runner) = filed_before(PICK, MAINTAINER, asked(), |rig| {
        hand_labelled(rig, MAINTAINER);
    });
    rig.forge
        .set_repo_labels(&[TRIAGE_GO, "ready-for-agent", "kind:fix"]);
    rig.github.refuse_label_renames(ApiError::Refused(403));
    let runner = reopened_as_taken(&rig, runner);
    for _ in 0..6 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(rig.github.label_takes(), []);

    rig.github.accept_label_renames();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    // The listing that poll held predates the swap, so it is read again.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.label_takes(),
        [(30, "ready-for-agent".to_owned())]
    );
}

// The issue check refuses it, so it is held off the board and the triager reaches
// it, with the label the maintainer put on still on it
#[test]
fn a_hand_labelled_issue_that_fails_the_check_keeps_its_label_and_goes_to_the_triager() {
    let (rig, runner) = on_the_board(PICK, MAINTAINER);

    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the triage call, not the board"
    );

    assert_eq!(rig.github.label_takes(), []);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["working"], serde_json::json!([]), "{status}");
}

#[test]
fn no_issue_without_a_taken_record_is_taken_back() {
    let (rig, runner) = filed_before(PICK, MAINTAINER, nothing(), |rig| {
        hand_labelled(rig, MAINTAINER);
    });
    rig.github.refuse_label_removals(ApiError::Refused(500));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.label_takes(),
        [],
        "no `ready-for-agent` comes off"
    );
    assert_eq!(record(&rig, &runner)["step"], "done", "its pass ran");
}

#[test]
fn an_issue_someone_else_opened_goes_to_the_triager_and_is_not_taken_back() {
    let (rig, runner) = on_the_board("pick = \"all\"", "newcomer");

    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the triage call, not the board"
    );

    assert_eq!(rig.github.label_takes(), []);
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
}

#[test]
fn an_issue_with_a_work_item_is_never_taken_back() {
    let rig = rig_with(PICK);
    let runner = rig.open().unwrap();
    hand_labelled(&rig, MAINTAINER);
    rig.forge.list_ready(30, false);
    took_back(&rig, 30, MAINTAINER, TITLE, BODY);
    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script([Scripted::Say(ASKS)]);
    rig.ask(&runner, "add", Some("30"));
    assert_eq!(issue_of(step(&runner).unwrap()).github(), Some(30));

    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(rig.github.label_takes(), []);
    assert_eq!(calls(&rig, Role::Triager).len(), 0);
}

#[test]
fn an_issue_an_open_pull_request_closes_is_never_taken_back() {
    let (rig, runner) = on_the_board(PICK, MAINTAINER);
    let runner = reopened_as_taken(&rig, runner);
    rig.forge.list_open_pull(77, "Fix the crash", &[30]);

    step(&runner).unwrap();

    assert_eq!(rig.github.label_takes(), []);
    assert_eq!(calls(&rig, Role::Triager).len(), 0);
}

// Listing the open pull requests is a call of its own, so a poll makes it only where the answer
// is used: an issue a `taken` record owes a swap to, or one the pick rule may list
#[test]
fn the_open_pulls_are_read_only_when_a_taken_back_issue_is_listed() {
    let pulls = |rig: &Rig| {
        rig.forge
            .pull_request_reads()
            .closing
            .load(Ordering::SeqCst)
    };
    // Nothing is queued and the issue is the maintainer's own and decided, so no
    // research is started on it either
    let (rig, runner) = filed_before(PICK, MAINTAINER, nothing(), |rig| {
        let mut decided = issue(rig, 30, MAINTAINER, 5);
        decided.labels = vec!["needs-info".to_owned()];
        list(rig, decided);
    });
    step(&runner).unwrap();
    assert_eq!(pulls(&rig), 0, "no issue is taken back or untouched");

    // A listed issue carries `ready-for-agent` and owes the swap its record says
    hand_labelled(&rig, MAINTAINER);
    rig.github.refuse_label_removals(ApiError::Refused(500));
    let runner = reopened_as_taken(&rig, runner);
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(pulls(&rig), 1, "a taken-back issue is listed");
    assert_eq!(record(&rig, &runner)["step"], "taken");

    // The swap stayed owed, and the recorded issue is listed without the label
    relist(&rig, &["needs-info"]);
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(pulls(&rig), 2, "a taken-back issue is listed");

    // The recorded issue is no longer listed, and nothing carries the label
    rig.forge.unlist_open_issue(30);
    rig.next_look();
    step(&runner).unwrap();
    assert_eq!(
        pulls(&rig),
        2,
        "a taken-back issue that is not listed is not a use"
    );
}

#[test]
fn its_answered_pass_makes_it_ready_again_and_the_next_poll_does_not_take_it_back() {
    // The forge as the swap leaves it, since the stand-in does not follow the App's writes
    let (rig, runner) = filed_before(PICK, MAINTAINER, asked(), |rig| {
        hand_labelled(rig, MAINTAINER);
        rig.forge.label(30, TRIAGE);
    });
    let runner = reopened_as_taken(&rig, runner);
    assert_eq!(step(&runner).unwrap(), None);
    relist(&rig, &[TRIAGE]);

    wrote(&rig, MAINTAINER, "Q1: Linux, on start. That is all of it.");
    rig.claude
        .script_for(Role::Triager, [reply(settled_ready())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let body = settled_ready()["body"]
        .as_str()
        .unwrap()
        .trim_end()
        .to_owned();
    assert_eq!(rig.github.issue_edits(), [(30, body)]);
    assert_eq!(moves(&rig).last().unwrap(), "label ready-for-agent");
    assert_eq!(record(&rig, &runner)["step"], "ready");
    let taken = rig.github.label_takes();

    relist(&rig, &["ready-for-agent"]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.github.label_takes(), taken, "not taken back again");
    assert_eq!(calls(&rig, Role::Triager).len(), 2);
}

#[test]
fn a_swap_the_forge_refuses_leaves_a_taken_record_and_is_finished_at_the_next_poll() {
    let (rig, runner) = filed_before(PICK, MAINTAINER, nothing(), |rig| {
        hand_labelled(rig, MAINTAINER)
    });
    let runner = reopened_as_taken(&rig, runner);
    rig.github.refuse_label_removals(ApiError::Refused(500));

    step(&runner).unwrap();

    let held = record(&rig, &runner);
    assert_eq!(held["step"], "taken", "{held}");
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        0,
        "nothing is asked while it is ready"
    );

    rig.github.accept_label_removals();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig)[..2],
        [format!("label {TRIAGE}"), "comment".to_owned()]
    );
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(record(&rig, &runner)["step"], "done");
}

#[test]
fn a_taken_issue_whose_ready_label_is_gone_still_gets_needs_triage_before_its_pass() {
    let (rig, runner) = filed_before(PICK, MAINTAINER, nothing(), |rig| {
        hand_labelled(rig, MAINTAINER)
    });
    let runner = reopened_as_taken(&rig, runner);

    // The forge as a stop between the two writes leaves it
    relist(&rig, &[]);
    step(&runner).unwrap();

    assert_eq!(
        moves(&rig)[..2],
        [format!("label {TRIAGE}"), "comment".to_owned()]
    );
    assert_eq!(
        rig.github.label_takes(),
        [],
        "ready-for-agent is not taken off where the listing shows it off already"
    );
}

// A decision the maintainer puts on after the swap stopped is the record's match
#[test]
fn a_taken_record_does_not_outrank_a_decision_put_on_later() {
    let (rig, runner) = filed_before(PICK, MAINTAINER, nothing(), |rig| {
        hand_labelled(rig, MAINTAINER);
    });
    let runner = reopened_as_taken(&rig, runner);
    rig.github.refuse_label_removals(ApiError::Refused(500));

    // The maintainer answers it `needs-info` by hand
    relist(&rig, &["ready-for-agent", "needs-info"]);
    step(&runner).unwrap();

    assert_eq!(rig.github.label_takes(), [], "the decision stands");
    assert_eq!(
        calls(&rig, Role::Triager).len(),
        0,
        "a decided issue is not due"
    );
}

// What becomes of a `taken` record whose issue was rewritten into one the check
// would pass: the record still owes its swap and its pass, the check reads the
// new body only once the pass ends, and it is ready from there
#[test]
fn a_taken_record_whose_body_now_passes_gets_its_swap_then_its_pass() {
    let (rig, runner) = filed_before(PICK, MAINTAINER, nothing(), |rig| {
        hand_labelled(rig, MAINTAINER);
    });
    let runner = reopened_as_taken(&rig, runner);

    let mut rewritten = issue(&rig, 30, MAINTAINER, 5);
    rewritten.title = TITLE.to_owned();
    rewritten.body = PASSING.to_owned();
    let mut labels = PASSING_LABELS.to_vec();
    labels.push("ready-for-agent");
    rewritten.labels = labels.iter().map(|&l| l.to_owned()).collect();
    list(&rig, rewritten);
    step(&runner).unwrap();

    assert_eq!(
        rig.github.label_takes(),
        [(30, "ready-for-agent".to_owned())],
        "the record owes the swap, and the check has not read the new body"
    );
    assert_eq!(checks(&rig)[0]["triage"], "ran", "{}", checks(&rig));
    assert_eq!(calls(&rig, Role::Triager).len(), 1, "and its owed pass");
    assert_eq!(record(&rig, &runner)["step"], "done");

    // The forge as the swap and the pass left it, the rewritten body still on
    let mut after = issue(&rig, 30, MAINTAINER, 5);
    after.title = TITLE.to_owned();
    after.body = PASSING.to_owned();
    let after_labels = [TRIAGE, "kind:fix", "priority: P3"];
    after.labels = after_labels.iter().map(|&l| l.to_owned()).collect();
    list(&rig, after);
    rig.next_look();
    step(&runner).unwrap();

    assert_eq!(
        moves(&rig).last().unwrap(),
        "label ready-for-agent",
        "the check reads the body once its pass ends: {}",
        checks(&rig)
    );
    assert_eq!(
        rig.github.label_takes().last().unwrap(),
        &(30, TRIAGE.to_owned()),
        "and it is ready from there"
    );
}

#[test]
fn an_issue_is_not_taken_back_where_no_app_covers_the_repo() {
    let (rig, runner) = on_the_board(PICK, MAINTAINER);
    let runner = reopened_as_taken(&rig, runner);
    rig.without_app();

    let (started, _) = dispatched(step(&runner).unwrap());

    assert_eq!(started.github(), Some(30), "the board keeps its pick");
    let writes = rig.forge.issue_writes();
    assert!(
        !writes
            .iter()
            .any(|w| w.contains("ready-for-agent") || w.contains(TRIAGE)),
        "{writes:?}"
    );
    assert_eq!(
        record(&rig, &runner)["step"],
        "taken",
        "the swap stays owed"
    );
    assert_eq!(triage(&rig, &runner)["running"], serde_json::Value::Null);
}
