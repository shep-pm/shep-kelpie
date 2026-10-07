//! A bot that reviews every push and the heads nit-only fixes push, through
//! the runner's stand-ins (ADR 0007)

use std::sync::Mutex;

use crate::ports::Checks;
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{bot_reviewed, listing, reviewed, summoned};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

const NIT: &str = "P3: The name could be shorter.";
const OTHER_NIT: &str = "P3: The comment restates the code.";

fn phase(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["work_item"]["phase"].clone()
}

// cubic, listed after qwen and Claude, reads `head` and leaves `threads`.
fn cubic_reads(rig: &Rig, runner: &Mutex<Runner>, head: &str, round: u32, threads: &[&str]) {
    step(runner).unwrap(); // marks the draft ready
    assert_eq!(step(runner).unwrap(), summoned(head));
    rig.forge
        .coderabbit
        .cubic_review(71, head, now(rig) + 300, threads);
    rig.clock.advance(300);
    assert_eq!(
        rig.threads_read(runner),
        bot_reviewed(round, "cubic", threads.len())
    );
}

// cubic's nit goes to a fix turn, and cubic, which reviews every push,
// leaves another nit on the head the fix pushed. Nothing reads it in this
// pass, and when a rework that pushes nothing has cubic read that head
// again, its nit is left open: no second fix turn, and the pass ends.
#[test]
fn a_bot_that_reviews_every_push_cannot_loop_on_nits() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = reviewed(&rig);
    cubic_reads(&rig, &runner, &head, 3, &[NIT]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    rig.claude
        .script([Scripted::Push("shorter.txt", "shorter\n")]);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_eq!(
        phase(&rig, &runner)["state"],
        "ci",
        "cubic is not summoned again"
    );
    rig.forge
        .coderabbit
        .cubic_review(71, &fixed, now(&rig) + 60, &[OTHER_NIT]);

    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let worker_turns = rig.claude.calls().len();
    rig.ask(&runner, "rule", Some("1 rework check it once more"));
    rig.claude.script([
        Scripted::Text("Nothing to change."),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the noted turn: pushes nothing, and a pass begins
    step(&runner).unwrap(); // round 1, qwen: clean by default
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    step(&runner).unwrap(); // round 3, cubic: its review already covers the head
    // The nit is still open on the forge, and goes to no fix turn.
    assert_eq!(rig.threads_read(&runner), bot_reviewed(3, "cubic", 1));
    assert_eq!(phase(&rig, &runner)["state"], "ci");
    assert_eq!(
        rig.claude.calls().len(),
        worker_turns + 1,
        "the rework's turn, and no fix turn for the nit"
    );
}

// qwen's nit is fixed, and cubic's first read of the pass is of the head
// that fix pushed: its nit still goes to a fix turn, since cubic never had
// one sent.
#[test]
fn a_bots_first_read_of_a_nit_fixs_head_still_sends_its_nits() {
    let rig = listing("shep", &["cubic"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer
        .script([ScriptedRound::Findings(crate::ports::parse_findings(
            "LOW|work.txt:1|a name is misleading|a reader is confused\n",
        ))]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Push("renamed.txt", "renamed\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: one nit
    step(&runner).unwrap(); // the nit goes to a fix turn
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { .. })
    ));
    step(&runner).unwrap(); // round 2, claude: scripted clean above
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    cubic_reads(&rig, &runner, &fixed, 3, &[NIT]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
}
