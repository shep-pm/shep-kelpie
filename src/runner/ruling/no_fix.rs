//! A merge ruling's `no`, whose fix goes to CI and back to the maintainer,
//! through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::ports::Checks;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

const NOTE_FIX: &str = "This head is the worker's fix for your note, and no reviewer read it.";

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

// A question the worker asks in a `no`'s fix turn does not turn the fix into
// a new pass: once answered, the fix still goes to CI.
#[test]
fn a_question_in_a_nos_fix_turn_still_sends_the_fix_to_ci() {
    let (rig, runner, _) = Rig::parked("rotom");
    let reviewer_reads = rig.reviewer.seen().len();
    rig.ask(&runner, "rule", Some("1 no rename the flag"));
    rig.claude.script([Scripted::Say(
        "Which name?\n\n<kelpie-question>\n--dry-run or --check?\n</kelpie-question>\n",
    )]);
    let report = step(&runner).unwrap(); // the noted turn asks
    assert!(
        matches!(report, Some(StepReport::Asked { id: 2, .. })),
        "{report:?}"
    );
    rig.ask(&runner, "rule", Some("2 answer --dry-run"));
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);
    step(&runner).unwrap(); // the answered turn pushes
    assert_eq!(phase(&rig, &runner)["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), reviewer_reads);
}

// Someone else's push during a `no`'s fix turn is not the worker's fix:
// the next ruling names the head unread, not as the note's fix, and a no on
// that ruling sends its fix through a pass, which is what clears it.
#[test]
fn a_push_by_someone_else_during_a_nos_fix_is_not_called_the_fix_and_its_no_starts_a_pass() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(&runner, "rule", Some("1 no rename the flag"));
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    rig.claude.script([Scripted::Text("Nothing to push.")]);
    step(&runner).unwrap(); // the noted turn pushes nothing itself
    rig.forge.set_checks(&by_hand, Checks::Passed);
    let Some(StepReport::Ruling {
        id: 2, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no second merge ruling");
    };
    assert!(!question.contains(NOTE_FIX), "{question}");
    assert!(
        question.contains("since only a new pass clears that"),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"][0]["kind"],
        json!({ "kind": "merge", "head": by_hand, "unread_head": true })
    );

    rig.ask(&runner, "rule", Some("2 no check that commit"));
    rig.claude.script([Scripted::Text("Looked at it.")]);
    step(&runner).unwrap(); // the noted turn ends, and a pass begins
    assert_eq!(phase(&rig, &runner)["state"], "review");
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(
        item.noted_from, None,
        "a no that starts a pass leaves nothing to say"
    );
}

// A hand-back the forge refuses fails the step before the ruling is saved,
// and the retry's ruling still names the note's fix.
#[test]
fn a_failed_hand_back_keeps_the_note_fix_for_the_retry() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(&runner, "rule", Some("1 no rename the flag"));
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);
    step(&runner).unwrap(); // the noted turn pushes, and goes to CI
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&pushed, Checks::Passed);
    rig.forge.set_labels_down(true);
    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::GateFailed { .. })),
        "{report:?}"
    );
    rig.forge.set_labels_down(false);
    let Some(StepReport::Ruling {
        id: 2, question, ..
    }) = step(&runner).unwrap()
    else {
        panic!("no second merge ruling");
    };
    assert!(question.contains(NOTE_FIX), "{question}");
}

// Under `auto` a `no`'s fix is never merged unread: it comes back to the
// maintainer as a merge ruling, as the question promised.
#[test]
fn under_auto_a_nos_fix_comes_back_as_a_ruling() {
    let (rig, runner, _) = Rig::parked("rotom");
    rig.ask(&runner, "rule", Some("1 no rename the flag"));
    rig.merge_auto();
    let reread = runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings());
    assert!(reread.is_ok(), "{reread:?}");
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);
    step(&runner).unwrap(); // the noted turn pushes, and goes to CI
    let pushed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&pushed, Checks::Passed);
    let mut reports = Vec::new();
    for _ in 0..4 {
        match rig.verdict(&runner) {
            Some(StepReport::Ruling { id, question, .. }) => {
                assert_eq!(id, 2);
                assert!(question.contains(NOTE_FIX), "{question}");
                assert_eq!(rig.forge.merges(), []);
                return;
            }
            other => reports.push(other),
        }
    }
    panic!(
        "no ruling under auto: {reports:?}, merges {:?}",
        rig.forge.merges()
    );
}
