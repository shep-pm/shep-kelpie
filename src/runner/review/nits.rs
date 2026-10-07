//! Nits get a fix turn that starts nothing new (ADR 0007)

use serde_json::json;

use crate::ports::{Checks, Finding, Severity};
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

fn nit() -> Finding {
    Finding {
        severity: Severity::Low,
        file: "src/lib.rs".into(),
        line: 3,
        what: "unused variable".into(),
        why: "dead code".into(),
    }
}

// The worker's first turn has pushed and the review's first round is due.
fn at_round_1() -> (Rig, std::sync::Mutex<crate::runner::Runner>) {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    (rig, runner)
}

// A nit fix that pushes nothing is the worker declining the nits: the pass
// goes on with no `fix-not-pushed` ruling.
#[test]
fn a_nit_fix_that_pushes_nothing_goes_on_to_the_next_reviewer_with_no_ruling() {
    let (rig, runner) = at_round_1();
    rig.reviewer.script([ScriptedRound::Findings(vec![nit()])]);
    rig.claude.script([
        Scripted::Text("The name reads fine as it is."),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // round 1, qwen: one nit
    step(&runner).unwrap(); // the nit goes to a fix turn
    step(&runner).unwrap(); // the worker's fix turn: pushes nothing
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::NitsDeclined {
            issue: 7,
            pull_request: 71,
            round: 1,
            nits: 1,
        })
    );
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(status["rulings"], json!([]));
}

// Someone else's push during a nit fix is not the worker's fix: its head is
// neither vouched for nor kept as a nit fix's.
#[test]
fn a_push_by_someone_else_during_a_nit_fix_is_not_a_nit_fixs_head() {
    let (rig, runner) = at_round_1();
    rig.reviewer.script([ScriptedRound::Findings(vec![nit()])]);
    step(&runner).unwrap(); // round 1, qwen: one nit
    step(&runner).unwrap(); // the nit goes to a fix turn
    let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
    rig.claude.script([Scripted::Text("Looked at it.")]);
    step(&runner).unwrap(); // the worker's fix turn: pushes nothing itself
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: 7,
            pull_request: 71,
            round: 1,
            head: Some(by_hand.clone()),
        })
    );
    let item = runner.lock().unwrap().state.work_items[0].clone();
    assert_eq!(item.nit_fix_heads, Vec::<String>::new());
    assert!(!item.vouches_for(&by_hand));
}

#[test]
fn a_round_of_nits_gets_one_fix_turn_and_the_pass_goes_on_to_the_next_reviewer() {
    let (rig, runner) = at_round_1();
    rig.reviewer.script([ScriptedRound::Findings(vec![nit()])]);
    step(&runner).unwrap(); // round 1, qwen: one nit
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 1,
            held: 1,
        })
    );
    let text = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(text.contains("LOW|src/lib.rs:3|unused variable"), "{text}");

    rig.claude.script([
        Scripted::Push("tidied.txt", "tidied\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's fix turn
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed {
            issue: 7,
            pull_request: 71,
            round: 1,
            head: Some(fixed.clone()),
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["ran"], json!(["qwen"]));

    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), 1, "qwen did not read again");
    assert_eq!(
        rig.claude.all_calls().len(),
        3,
        "first turn, fix turn, claude's read"
    );

    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let rulings = &rig.ask(&runner, "status", None)["rulings"];
    assert_eq!(
        rulings[0]["kind"],
        json!({ "kind": "merge", "head": fixed }),
        "the nit fix's head is vouched for"
    );
}
