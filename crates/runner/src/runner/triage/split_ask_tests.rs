//! Every split, on the maintainer's own issue or anyone else's, held for the
//! maintainer's yes, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::answers_tests::step_until;
use super::ask_tests::{posted, wrote};
use super::pick_tests::MAINTAINER;
use super::research_tests::APP;
use super::split_tests::{
    BODY, app_writes, draft, drawn, heard, judged, planned_asking, saved_split, splitting_with, yes,
};
use crate::runner::{Runner, step};
use crate::state::{StateStore, TriageQuestions, TriageStep};
use crate::test::Rig;

// The drafts the triager answers for an issue it judges too big
fn two() -> [Value; 2] {
    [
        drawn("fix(a): first", BODY, &[], Value::Null),
        drawn("fix(b): second", BODY, &[0], Value::Null),
    ]
}

// What the triager answers when it followed the split the issue spells out
fn spelled(drafts: &[Value]) -> Value {
    json!({
        "type": "enhancement", "kind": "fix", "priority": "P2",
        "body": BODY, "split": drafts, "split_from": "issue"
    })
}

// Issue 30 by `author`, which the triager splits in two, up to the new comment
// that asks the maintainer: the pass ended and the ask is the second comment
fn asked_of(lines: &str, author: &str, found: impl FnOnce(&Rig) -> Value) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = splitting_with(found, lines, author, &[]);
    step_until(&runner, || posted(&rig).len() == 2);
    (rig, runner)
}

// Someone else's issue 30, split by kelpie
fn asked_to_split(lines: &str) -> (Rig, Mutex<Runner>) {
    asked_of(lines, "newcomer", |_| judged(&two()))
}

// The maintainer's own issue 30, split by kelpie
fn asked_of_maintainer(lines: &str) -> (Rig, Mutex<Runner>) {
    asked_of(lines, MAINTAINER, |_| judged(&two()))
}

// What was filed, left alone, and asked: nothing filed, and the one ask held
fn still_asking(rig: &Rig) {
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(rig.forge.sub_issues_of(30), Vec::<u64>::new());
    assert_eq!(
        posted(rig).len(),
        2,
        "no answer to anyone: {:?}",
        posted(rig)
    );
    assert!(saved_split(rig)["asked"]["comment"].is_u64());
}

// The parent's triage record as the state file holds it, with no verb run
pub(super) fn saved_record(rig: &Rig) -> Value {
    let store = StateStore::new(rig.paths().state);
    let saved = serde_json::to_value(store.load().unwrap().unwrap()).unwrap();
    saved["triage"]["issues"][0].clone()
}

const PICK_ALL: &str = "pick = \"all\"";
const PICK_MAINTAINER: &str = "pick = \"maintainer\"";

#[test]
fn a_split_on_someone_elses_issue_asks_in_a_new_comment_and_files_nothing() {
    let (rig, _runner) = asked_to_split(PICK_ALL);

    let comments = posted(&rig);
    assert!(!comments[0].contains("fix(a): first"), "{}", comments[0]);
    let ask = &comments[1];
    assert!(ask.contains(&format!("@{MAINTAINER}")), "{ask}");
    assert!(ask.contains("fix(a): first"), "{ask}");
    assert!(ask.contains("fix(b): second (after 1)"), "{ask}");
    assert!(ask.contains("`yes`"), "{ask}");
    still_asking(&rig);
    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(rig.github.issue_edits(), [], "the author's body stands");
    assert!(app_writes(&rig, 0).contains(&"label #30 needs-approval".to_owned()));
}

#[test]
fn a_split_on_the_maintainers_own_issue_asks_and_files_nothing() {
    let (rig, _runner) = asked_of_maintainer(PICK_MAINTAINER);

    let comments = posted(&rig);
    assert_eq!(comments.len(), 2, "{comments:?}");
    let ask = &comments[1];
    assert!(ask.contains(&format!("@{MAINTAINER}")), "{ask}");
    assert!(ask.contains("fix(a): first"), "{ask}");
    assert!(ask.contains("fix(b): second (after 1)"), "{ask}");
    assert!(ask.contains("`yes`"), "{ask}");
    still_asking(&rig);
    assert_eq!(
        rig.github.issue_edits(),
        [(30, BODY.trim_end().to_owned())],
        "the maintainer's body is rewritten at once, as ever"
    );
    assert!(app_writes(&rig, 0).contains(&"label #30 needs-approval".to_owned()));
    let asked = saved_split(&rig)["asked"]["comment"].as_u64().unwrap();
    assert_ne!(asked, saved_record(&rig)["comment"].as_u64().unwrap());
}

#[test]
fn a_split_on_the_apps_own_issue_asks_too() {
    let (rig, _runner) = asked_of(PICK_MAINTAINER, APP, |_| judged(&two()));

    let ask = &posted(&rig)[1];
    assert!(ask.contains(&format!("@{MAINTAINER}")), "{ask}");
    assert!(ask.contains("if this is the split you meant"), "{ask}");
    still_asking(&rig);
    assert_eq!(rig.github.issues_created(), 0);
}

#[test]
fn the_ask_says_every_draft_passed_the_issue_check() {
    let (rig, _runner) = asked_to_split(PICK_ALL);
    let ask = &posted(&rig)[1];
    assert!(
        ask.contains("Every draft below passes kelpie's issue check"),
        "{ask}"
    );

    let (own, _) = asked_of_maintainer(PICK_MAINTAINER);
    let ask = &posted(&own)[1];
    assert!(
        ask.contains("Every draft below passes kelpie's issue check"),
        "{ask}"
    );
}

#[test]
fn the_maintainers_ask_asks_to_confirm_and_an_outsiders_asks_to_approve() {
    let (own, _) = asked_of_maintainer(PICK_MAINTAINER);
    let ask = &posted(&own)[1];
    assert!(
        ask.contains("Reply `yes` if this is the split you meant"),
        "{ask}"
    );

    let (other, _) = asked_to_split(PICK_ALL);
    let ask = &posted(&other)[1];
    assert!(
        ask.contains("Reply `yes` to approve the split and the directions its author gave"),
        "{ask}"
    );
}

#[test]
fn the_ask_says_a_split_the_author_spelled_out_is_theirs() {
    let (other, _) = asked_of(PICK_ALL, "newcomer", |_| spelled(&two()));
    let ask = &posted(&other)[1];
    assert!(ask.contains("Its author spelled this split out"), "{ask}");
    assert!(!ask.contains("Kelpie worked the split out"), "{ask}");

    let (own, _) = asked_of(PICK_MAINTAINER, MAINTAINER, |_| spelled(&two()));
    let ask = &posted(&own)[1];
    assert!(ask.contains("This split is yours"), "{ask}");
    assert!(!ask.contains("Kelpie worked the split out"), "{ask}");
}

// A split the issue spells out is still read against the issue check, and a
// failing draft asks nothing
#[test]
fn a_spelled_out_split_that_fails_the_issue_check_asks_nothing() {
    let failing = [
        drawn("fix(a): first", BODY, &[], Value::Null),
        drawn("fix(b): second", "Touches: `src/b.rs`\n", &[0], Value::Null),
    ];
    let (rig, runner) = splitting_with(|_| spelled(&failing), PICK_ALL, "newcomer", &[]);
    step_until(&runner, || posted(&rig).len() == 1);

    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(posted(&rig).len(), 1, "no ask follows a refused split");
    assert_eq!(saved_split(&rig), Value::Null);
}

#[test]
fn the_maintainers_yes_files_in_order() {
    let (rig, runner) = asked_to_split(PICK_ALL);
    let start = app_writes(&rig, 0).len();

    wrote(&rig, MAINTAINER, "yes");
    heard(&rig, &runner);
    step_until(&runner, || rig.github.issues_created() == 2);

    assert_eq!(
        app_writes(&rig, start),
        [
            "file fix(a): first",
            "label #900 ready-for-agent",
            "file fix(b): second",
            "label #901 ready-for-agent",
        ]
    );
    assert_eq!(rig.forge.sub_issues_of(30), [900, 901]);
    assert_eq!(rig.forge.links_added(), [(901, 900)]);
    assert_eq!(rig.github.issue_edits(), [], "the author's body stands");
    assert_eq!(saved_split(&rig)["asked"], Value::Null);
}

#[test]
fn the_maintainers_yes_on_their_own_issue_files_in_order() {
    let (rig, runner) = asked_of_maintainer(PICK_MAINTAINER);
    // The forge holds the `needs-approval` the pass put on, as GitHub does
    rig.forge.label(30, "needs-approval");
    let start = rig.github.writes().len();

    wrote(&rig, MAINTAINER, "yes");
    heard(&rig, &runner);
    step_until(&runner, || rig.github.issues_created() == 2);

    assert_eq!(
        app_writes(&rig, start),
        [
            "unlabel #30 needs-approval",
            "file fix(a): first",
            "label #900 ready-for-agent",
            "file fix(b): second",
            "label #901 ready-for-agent",
        ]
    );
    assert_eq!(rig.forge.sub_issues_of(30), [900, 901]);
    assert_eq!(rig.forge.links_added(), [(901, 900)]);
    assert_eq!(saved_split(&rig)["asked"], Value::Null);
    assert_eq!(saved_split(&rig)["filed"], json!(["900", "901"]));
}

#[test]
fn a_strangers_yes_files_nothing() {
    let (rig, runner) = asked_to_split(PICK_ALL);

    wrote(&rig, "passerby", "yes");
    heard(&rig, &runner);

    still_asking(&rig);
}

#[test]
fn a_writers_yes_files_nothing() {
    let (rig, runner) = asked_to_split(PICK_ALL);

    wrote(&rig, "collaborator", "yes");
    heard(&rig, &runner);

    still_asking(&rig);
}

#[test]
fn the_authors_own_yes_files_nothing() {
    let (rig, runner) = asked_to_split(PICK_ALL);

    wrote(&rig, "newcomer", "yes please, ok?");
    wrote(&rig, "newcomer", "yes");
    heard(&rig, &runner);

    still_asking(&rig);
}

#[test]
fn a_no_drops_the_split() {
    let (rig, runner) = asked_to_split(PICK_ALL);

    wrote(&rig, MAINTAINER, "No.");
    heard(&rig, &runner);

    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(saved_split(&rig), Value::Null);
    assert_eq!(posted(&rig).len(), 2, "no answer to the no");
    wrote(&rig, MAINTAINER, "yes");
    heard(&rig, &runner);
    assert_eq!(rig.github.issues_created(), 0, "a later yes has no plan");
}

#[test]
fn other_words_leave_the_wait_open() {
    let (rig, runner) = asked_to_split(PICK_ALL);

    wrote(&rig, MAINTAINER, "Make it three, then yes");
    wrote(&rig, MAINTAINER, "okay, but later");
    heard(&rig, &runner);
    still_asking(&rig);

    wrote(&rig, MAINTAINER, "ok");
    heard(&rig, &runner);
    step_until(&runner, || rig.github.issues_created() == 2);
}

#[test]
fn silence_drops_the_split() {
    let (rig, runner) = asked_to_split("pick = \"all\"\nwait_days = 2");

    rig.clock.advance(Rig::DAY);
    heard(&rig, &runner);
    still_asking(&rig);

    rig.clock.advance(Rig::DAY);
    heard(&rig, &runner);

    assert_eq!(saved_split(&rig), Value::Null);
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(posted(&rig).len(), 2, "nothing more is posted");
}

// The maintainer's own split is dropped by the same silence
#[test]
fn silence_drops_the_split_of_the_maintainers_own_issue() {
    let (rig, runner) = asked_of_maintainer("pick = \"maintainer\"\nwait_days = 2");

    rig.clock.advance(Rig::DAY);
    heard(&rig, &runner);
    still_asking(&rig);

    rig.clock.advance(Rig::DAY);
    heard(&rig, &runner);

    assert_eq!(saved_split(&rig), Value::Null);
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(
        rig.github.issue_edits().len(),
        1,
        "the body stays rewritten"
    );
}

// The saved plan put back to what it was before its ask went up: held by the
// triage comment, with the record at the `labelled` step. This is what a pass
// that stopped before the ask leaves.
fn before_the_ask(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap();
    let mut next = runner.state.clone();
    let triage = next.triage.as_mut().unwrap();
    let record = triage.issues.iter_mut().find(|t| t.issue == 30).unwrap();
    record.step = TriageStep::Labelled;
    let comment = record.comment;
    let at = record.at;
    triage.splits[0].asked = Some(TriageQuestions { comment, at });
    runner.save(next).unwrap();
}

#[test]
fn an_ask_whose_post_answer_was_lost_is_found_and_not_posted_again() {
    let (rig, runner) = asked_to_split(PICK_ALL);
    let asked = saved_split(&rig)["asked"]["comment"].as_u64().unwrap();

    before_the_ask(&runner);
    assert!(runner.lock().unwrap().ask_split(30).unwrap());

    assert_eq!(posted(&rig).len(), 2, "the ask is not posted twice");
    assert_eq!(saved_split(&rig)["asked"]["comment"], asked);
}

#[test]
fn a_hand_label_that_ends_the_pass_drops_the_plan_it_never_asked() {
    let (rig, runner) = asked_to_split(PICK_ALL);
    before_the_ask(&runner);
    rig.forge.label(30, "ready-for-agent");

    runner.lock().unwrap().label_waiting_step(30).unwrap();

    assert_eq!(saved_split(&rig), Value::Null, "no plan waits on no ask");
    wrote(&rig, MAINTAINER, "ok");
    heard(&rig, &runner);
    assert_eq!(rig.github.issues_created(), 0, "a bare ok files nothing");
}

// What a lost ask post leaves: the parent back at `body-written`, the plan held
// by its triage comment, and `needs-approval` on the issue from the call that
// did not finish. That label is the ask's own, so the next poll asks again
// rather than taking it for one the maintainer put on and dropping the plan.
#[test]
fn a_lost_ask_post_asks_again_and_keeps_the_split() {
    let held = json!({ "comment": 101, "at": 4_000_000_000_u64 });
    let drafts = [
        draft("fix(a): first", BODY, &[]),
        draft("fix(b): second", BODY, &[0]),
    ];
    let (rig, runner) = planned_asking(&drafts, &[], held);
    rig.forge.label(30, "needs-approval");
    let start = rig.github.writes().len();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(posted(&rig).len(), 1, "the ask goes up: {:?}", posted(&rig));
    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(2),
        "the plan stands"
    );
    assert_ne!(
        saved_split(&rig)["asked"]["comment"].as_u64().unwrap(),
        saved_record(&rig)["comment"].as_u64().unwrap(),
        "the wait is dated from the ask"
    );
    assert_eq!(saved_record(&rig)["step"], "done");
    assert_eq!(
        rig.github.issues_created(),
        0,
        "nothing files before the yes"
    );
    assert_eq!(
        app_writes(&rig, start),
        ["label #30 needs-approval", "comment #30"],
        "the parent waits at `needs-approval`"
    );

    yes(&rig, &runner, 2);
    assert_eq!(rig.forge.sub_issues_of(30), [900, 901]);
    assert_eq!(saved_split(&rig)["filed"], json!(["900", "901"]));
}
