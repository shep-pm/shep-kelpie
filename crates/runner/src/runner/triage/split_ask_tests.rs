//! Every split, on the maintainer's own issue or anyone else's, held for the
//! maintainer's yes, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::answers_tests::step_until;
use super::ask_tests::{posted, wrote};
use super::pick_tests::MAINTAINER;
use super::research_tests::{APP, calls, reply};
use super::split_tests::{
    BODY, app_writes, draft, drawn, heard, judged, planned_asking, saved_split, splitting_with, yes,
};
use crate::ports::Role;
use crate::runner::{Runner, step};
use crate::state::{StateStore, TriageQuestions, TriageStep};
use crate::test::{Rig, Scripted};

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

// What the maintainer writes that is neither `yes` nor `no` corrects the plan,
// so the wait is not left open with nothing done: the split is planned again
#[test]
fn other_words_under_the_ask_plan_the_split_again() {
    let (rig, runner, first) = corrected(|_| judged(&two()));
    step_until(&runner, || posted(&rig).len() == 4);

    assert_eq!(rig.github.issues_created(), 0, "no old plan files");
    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(2),
        "the plan the new pass brought stands"
    );
    assert_ne!(
        saved_split(&rig)["asked"]["comment"].as_u64().unwrap(),
        first,
        "the new plan is asked again"
    );
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

// The drafts the pass a correction asks for answers with
fn three() -> [Value; 3] {
    [
        drawn("fix(a): one", BODY, &[], Value::Null),
        drawn("fix(b): two", BODY, &[0], Value::Null),
        drawn("fix(c): three", BODY, &[1], Value::Null),
    ]
}

// What the maintainer writes under the ask when the plan is not the split they meant
const CORRECTED: &str = "Not two. Split it three ways, one per language family.";

// What the representative says for the comment that plans the split again
const AGAIN: &str = "Kelpie planned the split again from your reply.";

// What the facts say so the comment never asks for a hand-filed sub-issue
const NEVER_FILE: &str = "Never ask the maintainer to file a sub-issue themselves.";

// Someone else's issue 30, its ask corrected by the maintainer, and the triager
// answering the pass that correction asks for with `found`. With it, the comment
// id the first ask went up under.
fn corrected(found: impl FnOnce(&Rig) -> Value) -> (Rig, Mutex<Runner>, u64) {
    let (rig, runner) = asked_to_split(PICK_ALL);
    let first = saved_split(&rig)["asked"]["comment"].as_u64().unwrap();
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(AGAIN)]);
    rig.claude.script_for(Role::Triager, [reply(found(&rig))]);
    wrote(&rig, MAINTAINER, CORRECTED);
    heard(&rig, &runner);
    (rig, runner, first)
}

// [`corrected`], up to the new comment that asks the maintainer about the new plan
fn replanned(found: impl FnOnce(&Rig) -> Value) -> (Rig, Mutex<Runner>, u64) {
    let (rig, runner, first) = corrected(found);
    step_until(&runner, || posted(&rig).len() == 4);
    (rig, runner, first)
}

// What the `n`th wording call was asked to word, which holds the facts
fn worded(rig: &Rig, n: usize) -> String {
    let mut asked = calls(rig, Role::Representative);
    assert_eq!(asked.len(), n + 1, "{asked:?}");
    asked.remove(n).call.prompt
}

// What the `n`th triager call was asked
fn researched(rig: &Rig, n: usize) -> String {
    let mut asked = calls(rig, Role::Triager);
    assert_eq!(asked.len(), n + 1, "{asked:?}");
    asked.remove(n).call.prompt
}

// The id of the last comment the App posted on issue 30
fn last_posted(rig: &Rig) -> u64 {
    rig.github
        .posted_on(30)
        .last()
        .map(|c| c.id)
        .expect("the App posted a comment")
}

#[test]
fn a_correction_under_the_ask_plans_the_split_again() {
    let (rig, _runner, _) = replanned(|_| judged(&three()));

    assert_eq!(
        rig.github.issues_created(),
        0,
        "the new plan waits on a yes of its own"
    );
    assert_eq!(rig.forge.sub_issues_of(30), Vec::<u64>::new());
    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(3),
        "the held plan gives way to the new one"
    );
    let asked = researched(&rig, 1);
    assert!(asked.contains(CORRECTED), "{asked}");
    assert!(
        asked.contains("fix(a): first"),
        "the plan it corrects: {asked}"
    );
    assert!(asked.contains("plan the split again"), "{asked}");
}

#[test]
fn the_new_plan_is_asked_in_a_new_comment() {
    let (rig, _runner, first) = replanned(|_| judged(&three()));

    let comments = posted(&rig);
    assert_eq!(comments.len(), 4, "{comments:?}");
    let ask = &comments[3];
    assert!(ask.contains(&format!("@{MAINTAINER}")), "{ask}");
    assert!(ask.contains("fix(a): one"), "{ask}");
    assert!(ask.contains("fix(b): two (after 1)"), "{ask}");
    assert!(ask.contains("fix(c): three (after 2)"), "{ask}");
    assert!(
        !ask.contains("fix(a): first"),
        "the old plan is gone: {ask}"
    );
    assert_eq!(saved_split(&rig)["asked"]["comment"], last_posted(&rig));
    assert_ne!(
        saved_split(&rig)["asked"]["comment"].as_u64().unwrap(),
        first,
        "the wait starts from the new ask"
    );
    assert_ne!(
        saved_split(&rig)["asked"]["comment"],
        saved_record(&rig)["comment"],
        "the ask is its own comment"
    );
    assert_eq!(saved_record(&rig)["step"], "done");
    assert_eq!(rig.github.issues_created(), 0);
}

#[test]
fn a_yes_under_the_old_ask_files_nothing() {
    let (rig, runner) = asked_to_split(PICK_ALL);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(AGAIN)]);
    rig.claude
        .script_for(Role::Triager, [reply(judged(&three()))]);
    wrote(&rig, MAINTAINER, CORRECTED);
    wrote(&rig, MAINTAINER, "yes");

    heard(&rig, &runner);
    step_until(&runner, || posted(&rig).len() == 4);

    assert_eq!(
        rig.github.issues_created(),
        0,
        "the yes answers an ask that no longer stands"
    );
    assert_eq!(rig.forge.sub_issues_of(30), Vec::<u64>::new());
    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(3),
        "the new plan still waits on its own ask"
    );
}

#[test]
fn a_yes_to_the_new_ask_files_the_new_drafts() {
    let (rig, runner, _) = replanned(|_| judged(&three()));

    yes(&rig, &runner, 3);

    assert_eq!(rig.forge.sub_issues_of(30), [900, 901, 902]);
    assert_eq!(rig.forge.links_added(), [(901, 900), (902, 901)]);
    assert_eq!(saved_split(&rig)["filed"], json!(["900", "901", "902"]));
    assert_eq!(saved_split(&rig)["asked"], Value::Null);
}

#[test]
fn a_new_pass_with_no_split_drops_the_plan_and_says_so() {
    let (rig, _runner, _) = corrected(|_| judged(&[]));

    assert_eq!(saved_split(&rig), Value::Null, "no plan is left held");
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(posted(&rig).len(), 3, "no ask follows no plan");
    let said = worded(&rig, 1);
    assert!(said.contains("planned the split again"), "{said}");
    assert!(said.contains("no split"), "{said}");
    assert!(said.contains(NEVER_FILE), "{said}");
    assert!(!said.contains("file it yourself"), "{said}");
}

// A new split the issue check refuses never reaches an ask: the comment says so,
// and never that the maintainer's words support no split
#[test]
fn a_new_split_the_check_refuses_is_dropped_and_not_called_no_split() {
    let failing = [
        drawn("fix(a): one", BODY, &[], Value::Null),
        drawn("fix(b): two", "Touches: `src/b.rs`\n", &[0], Value::Null),
    ];
    let (rig, runner) = asked_of_maintainer(PICK_MAINTAINER);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(AGAIN)]);
    rig.claude
        .script_for(Role::Triager, [reply(judged(&failing))]);

    wrote(&rig, MAINTAINER, CORRECTED);
    heard(&rig, &runner);
    step_until(&runner, || posted(&rig).len() == 3);

    assert_eq!(saved_split(&rig), Value::Null, "no plan is left held");
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(posted(&rig).len(), 3, "no ask follows a refused split");
    let said = worded(&rig, 1);
    assert!(said.contains("did not reach a new ask"), "{said}");
    assert!(
        said.contains("Do not say their words support no split"),
        "{said}"
    );
    assert!(!said.contains("found no split"), "{said}");
    assert!(said.contains(NEVER_FILE), "{said}");
}

// The maintainer's own issue: its body is rewritten again, and its comment names
// the new drafts
#[test]
fn a_correction_on_the_maintainers_own_issue_replaces_the_plan() {
    let (rig, runner) = asked_of_maintainer(PICK_MAINTAINER);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(AGAIN)]);
    rig.claude
        .script_for(Role::Triager, [reply(judged(&three()))]);
    let first = saved_split(&rig)["asked"]["comment"].as_u64().unwrap();

    wrote(&rig, MAINTAINER, CORRECTED);
    heard(&rig, &runner);
    step_until(&runner, || posted(&rig).len() == 4);

    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(3),
        "the held plan gives way to the new one"
    );
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(
        rig.github.issue_edits().len(),
        2,
        "the body goes over again"
    );
    assert_ne!(
        saved_split(&rig)["asked"]["comment"].as_u64().unwrap(),
        first,
        "the new plan is asked in a comment of its own"
    );
    let said = worded(&rig, 1);
    assert!(said.contains("planned the split again"), "{said}");
    assert!(said.contains("1. fix(a): one"), "{said}");
    assert!(said.contains("2. fix(b): two (after 1)"), "{said}");
    assert!(said.contains(NEVER_FILE), "{said}");

    yes(&rig, &runner, 3);

    assert_eq!(rig.forge.sub_issues_of(30), [900, 901, 902]);
    assert_eq!(saved_split(&rig)["filed"], json!(["900", "901", "902"]));
}

#[test]
fn a_strangers_correction_plans_nothing() {
    let (rig, runner) = asked_to_split(PICK_ALL);
    rig.claude
        .script_for(Role::Triager, [reply(judged(&three()))]);

    wrote(&rig, "passerby", CORRECTED);
    heard(&rig, &runner);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "no pass from a stranger"
    );
    still_asking(&rig);
    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(2),
        "the plan the ask holds stands"
    );
}
