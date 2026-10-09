//! Slots and parked work items, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::board::Skip;
use crate::ports::{Checks, Cost, Usage};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

// A worker's reply that ends on a question, which parks its work item
pub(super) const ASKS: &str = "<kelpie-question>\nWhich flag?\n</kelpie-question>\n";

pub(super) fn running(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    (rig, runner)
}

pub(super) fn dispatched(report: Option<StepReport>) -> (u64, Vec<Skip>) {
    match report {
        Some(StepReport::Dispatched { issue, skipped, .. }) => (issue, skipped),
        other => panic!("nothing was dispatched: {other:?}"),
    }
}

// The issue of the work item a step's report is about
pub(super) fn issue_of(report: Option<StepReport>) -> u64 {
    let logged = serde_json::to_value(&report).unwrap();
    logged["issue"]
        .as_u64()
        .unwrap_or_else(|| panic!("no work item acted: {logged}"))
}

// The status's working, waiting and parked issues
pub(super) fn slots(rig: &Rig, runner: &Mutex<Runner>) -> [Value; 3] {
    let status = rig.ask(runner, "status", None);
    ["working", "waiting_for_slot", "parked"].map(|key| status[key].clone())
}

// One slot, and #7 opened and parked on its question as ruling 1, alerted
fn seven_asks(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = running(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's first turn");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    (rig, runner)
}

#[test]
fn an_item_parked_on_a_ruling_frees_its_slot_for_the_next_ready_issue() {
    let (rig, runner) = seven_asks("acme");
    rig.forge.list_ready(8, false);
    assert_eq!(dispatched(step(&runner).unwrap()), (8, vec![]));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([]), json!([7])]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_items"].as_array().unwrap().len(), 2);
    assert_eq!(status["work_item"]["issue"], 7, "the oldest, as before");
}

#[test]
fn a_full_pending_rulings_opens_nothing_and_its_alert_counts_the_rulings() {
    let (rig, runner) = seven_asks("acme");
    let [(_, first)] = rig.alerts.posts().try_into().unwrap();
    assert!(!first.text.contains("pending_rulings"), "{}", first.text);
    for issue in [8, 9] {
        rig.forge.list_ready(issue, false);
    }
    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(dispatched(step(&runner).unwrap()), (8, vec![]));
    assert_eq!(issue_of(step(&runner).unwrap()), 8, "#8's first turn");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    let (_, filled) = rig.alerts.posts().pop().unwrap();
    assert!(
        filled.text.ends_with(
            "\n\n2 rulings are waiting, and `concurrency.pending_rulings` is 2, so no new work item opens \
             until one is answered."
        ),
        "{}",
        filled.text
    );

    // The one slot is free, and still #9 waits.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(slots(&rig, &runner), [json!([]), json!([]), json!([7, 8])]);

    rig.ask(&runner, "rule", Some("1 answer the short one"));
    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7 takes the slot");
}

#[test]
fn pending_rulings_0_opens_nothing_while_any_ruling_waits() {
    let rig = Rig::new("acme");
    rig.edit_settings(|s| s.replace("pending_rulings = 2", "pending_rulings = 0"));
    let runner = rig.open().unwrap();
    // With no ruling waiting, the board opens work as ever.
    rig.forge.list_ready(7, false);
    assert_eq!(dispatched(step(&runner).unwrap()), (7, vec![]));
    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's first turn");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let (_, alert) = rig.alerts.posts().pop().unwrap();
    assert!(
        alert
            .text
            .contains("1 ruling is waiting, and `concurrency.pending_rulings` is 0")
    );
    rig.forge.list_ready(8, false);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(slots(&rig, &runner), [json!([]), json!([]), json!([7])]);
}

#[test]
fn a_larger_active_items_gives_a_waiting_item_its_slot() {
    let (rig, runner) = seven_asks("acme");
    rig.forge.list_ready(8, false);
    assert_eq!(dispatched(step(&runner).unwrap()), (8, vec![]));
    rig.ask(&runner, "rule", Some("1 answer the short one"));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([7]), json!([])]);

    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let next = rig.settings();
    runner
        .lock()
        .unwrap()
        .reread(next, rig.kelpie_settings())
        .unwrap();
    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's answered turn");
}

#[test]
fn an_answered_ruling_waits_for_the_slot_and_goes_before_a_new_issue() {
    let (rig, runner) = seven_asks("acme");
    rig.forge.list_ready(8, false);
    assert_eq!(dispatched(step(&runner).unwrap()), (8, vec![]));
    rig.forge.list_ready(9, false);

    // #8 holds the one slot, so #7's answer waits for it.
    rig.ask(&runner, "rule", Some("1 answer the short one"));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([7]), json!([])]);
    // Its session would be a model call past `concurrency.active_items` too.
    let me = std::process::id();
    let refused = rig.ask(&runner, "attach", Some(&format!("7 {me}")));
    let why = refused["error"].as_str().unwrap();
    assert!(
        why.starts_with("the work item for #7 holds no slot under `concurrency.active_items`"),
        "{why}"
    );
    assert!(rig.ask(&runner, "status", None)["work_item"]["attached"].is_null());
    rig.claude
        .script([Scripted::Say(ASKS), Scripted::Say(ASKS)]);
    assert_eq!(issue_of(step(&runner).unwrap()), 8, "#8's first turn");
    let calls = rig.claude.calls();
    assert_eq!(calls.len(), 2, "#7's turn did not run");
    assert!(calls[1].prompt.contains("issue #8"), "{}", calls[1].prompt);

    // #8 parked on its question, so #7 takes the slot, ahead of #9.
    assert_eq!(slots(&rig, &runner), [json!([7]), json!([]), json!([8])]);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's answered turn");
    let call = rig.claude.calls().pop().unwrap();
    assert!(call.prompt.contains("the short one"), "{}", call.prompt);
    assert_eq!(
        rig.ask(&runner, "status", None)["work_items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// One slot, #7 parked on merge ruling 1 about the returned head and alerted,
// and #8 opened in the slot it freed
fn seven_on_its_merge_ruling_and_eight_working(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = Rig::parked(project);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    rig.forge.list_ready(8, false);
    // #7's branch touches a file, so #8 waits for the board to read its paths.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(dispatched(step(&runner).unwrap()), (8, vec![]));
    (rig, runner, head)
}

#[test]
fn a_merge_yes_merges_at_once_with_every_slot_taken() {
    let (rig, runner, head) = seven_on_its_merge_ruling_and_eight_working("acme");
    rig.ask(&runner, "rule", Some("1 yes"));
    // A merge runs no model call, so #7 goes on beside #8 in the one slot.
    assert_eq!(slots(&rig, &runner), [json!([7, 8]), json!([]), json!([])]);
    rig.claude.script([Scripted::Say(ASKS)]);
    let mut merged = false;
    for _ in 0..4 {
        match step(&runner).unwrap() {
            Some(StepReport::Finished {
                issue: 7,
                merged: true,
                ..
            }) => {
                merged = true;
                break;
            }
            _ => rig.clock.advance(crate::runner::CHECKS_SETTLE),
        }
    }
    assert!(merged, "#7 never merged");
    assert_eq!(rig.forge.merges(), [(71, head)]);
}

#[test]
fn a_merge_no_is_a_fix_turn_and_waits_for_the_slot() {
    let (rig, runner, _) = seven_on_its_merge_ruling_and_eight_working("acme");
    rig.ask(&runner, "rule", Some("1 no rename the flag"));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([7]), json!([])]);
    rig.claude.script([Scripted::Say(ASKS)]);
    assert_eq!(
        issue_of(step(&runner).unwrap()),
        8,
        "#8's turn, not #7's fix"
    );
    let calls = rig.claude.calls();
    assert!(calls.iter().all(|c| !c.prompt.contains("rename the flag")));
}

// #8 opened in the slot a parked #7 freed, through its first turn and its
// review to CI, where it waits with no checks reported. Returns #8's head.
pub(super) fn eight_reaches_ci(rig: &Rig, runner: &Mutex<Runner>) -> String {
    rig.forge.list_ready(8, false);
    // #7's branch touches a file, so #8 waits for the board to read its paths.
    assert_eq!(step(runner).unwrap(), None);
    assert_eq!(dispatched(step(runner).unwrap()), (8, vec![]));
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    rig.claude.script([
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        assert_eq!(
            issue_of(step(runner).unwrap()),
            8,
            "a turn, then its review"
        );
    }
    rig.forge.head_of("kelpie/8").unwrap()
}

#[test]
fn a_merge_yes_sent_back_to_a_fix_turn_waits_for_the_slot() {
    let (rig, runner, _) = Rig::parked("acme");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    rig.land_on_origin("landed.txt");
    let eight = eight_reaches_ci(&rig, &runner);

    // The yes is withdrawn for a `main` that moved, and the caught-up head goes red.
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::YesWithdrawn { issue: 7, .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Rebased { issue: 7, .. })
    ));
    let seven = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge
        .set_checks(&seven, Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { issue: 7, .. })
    ));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([7]), json!([])]);
    let calls = rig.claude.calls().len();
    assert_eq!(step(&runner).unwrap(), None, "#7's fix turn waits");
    assert_eq!(rig.claude.calls().len(), calls);

    // #8 parks on its merge ruling, which frees the slot for #7's fix.
    rig.forge.set_checks(&eight, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling {
            issue: 8,
            id: 2,
            ..
        })
    ));
    assert_eq!(slots(&rig, &runner), [json!([7]), json!([]), json!([8])]);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 2 }));
    rig.claude.script([Scripted::Push("fix.txt", "fixed\n")]);
    assert_eq!(issue_of(step(&runner).unwrap()), 7, "#7's fix turn");
    let fix = rig.claude.calls().pop().unwrap();
    assert!(fix.prompt.contains("CI failed"), "{}", fix.prompt);
}

// One slot, #7 parked on `still-red` ruling 1 after a fix that pushed
// nothing, and #8 opened in its slot and waiting on CI
fn seven_still_red_and_eight_in_ci(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request(project);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".into()]));
    rig.verdict(&runner);
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling {
            issue: 7,
            id: 1,
            ..
        })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    eight_reaches_ci(&rig, &runner);
    (rig, runner)
}

#[test]
fn a_still_red_yes_goes_to_ci_with_every_slot_taken() {
    let (rig, runner) = seven_still_red_and_eight_in_ci("acme");

    // CI calls no model, so the yes goes on beside #8 in the one slot.
    let fixed = rig.push_by_hand("kelpie/7", "lint.txt");
    rig.ask(&runner, "rule", Some("1 yes"));
    assert_eq!(slots(&rig, &runner), [json!([7, 8]), json!([]), json!([])]);
    // An attach would start a session, which needs the slot #8 holds.
    let me = std::process::id();
    let refused = rig.ask(&runner, "attach", Some(&format!("7 {me}")));
    let why = refused["error"].as_str().unwrap();
    assert!(
        why.contains("holds no slot under `concurrency.active_items`"),
        "{why}"
    );
    rig.forge.set_checks(&fixed, Checks::Passed);
    let Some(StepReport::Ruling {
        issue: 7, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("#7 never reached its merge ruling");
    };
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_bots_pass_owed_after_green_ci_waits_for_the_slot() {
    let (rig, runner) = seven_still_red_and_eight_in_ci("acme");
    // A state file from before review bots read in the pass owes #7 one.
    rig.coderabbit_on();
    drop(runner);
    let path = rig.paths().state;
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["work_items"][0]["issue"], 7);
    saved["work_items"][0]["bots_after_ci"] = json!(true);
    std::fs::write(&path, saved.to_string()).unwrap();
    let runner = rig.open().unwrap();

    let fixed = rig.push_by_hand("kelpie/7", "lint.txt");
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.forge.set_checks(&fixed, Checks::Passed);
    // Green CI sends #7 to the bots' pass, which waits for #8's slot.
    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(crate::runner::CHECKS_SETTLE);
    }
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([7]), json!([])]);
    assert_eq!(
        rig.forge.readied(),
        Vec::<u64>::new(),
        "no bot was summoned"
    );
}

#[test]
fn an_attach_takes_a_free_slot_for_an_item_going_on_without_one() {
    let (rig, runner, head) = Rig::with_pull_request("acme");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["lint".into()]));
    rig.verdict(&runner);
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling {
            issue: 7,
            id: 1,
            ..
        })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    // The yes sends #7 to CI without a slot, so the one slot is free.
    rig.push_by_hand("kelpie/7", "lint.txt");
    rig.ask(&runner, "rule", Some("1 yes"));

    let me = std::process::id();
    let answer = rig.ask(&runner, "attach", Some(&format!("7 {me}")));
    assert_eq!(answer["attach"], "ready", "{answer}");
    // The session holds the slot, so the board opens nothing beside it.
    rig.forge.list_ready(8, false);
    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert_eq!(slots(&rig, &runner), [json!([7]), json!([]), json!([])]);
}
