//! A bot the pass went on without that reviews the head late, read during
//! CI and while the merge ruling waits, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::late::LATE_READ;
use crate::ports::Checks;
use crate::runner::coderabbit::tests::now;
use crate::runner::review_bot::two_bots::{bot_reviewed, listing, reviewed, summoned};
use crate::runner::rework::HUMAN;
use crate::runner::slots_tests::{issue_of, slots};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

const FINDING: &str = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";

const LATE_FIX: &str =
    "This head is the worker's fix for a review bot's late review, and no reviewer read it.";

fn status(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)
}

// cubic, last after qwen and Claude, refuses its summon and is passed over,
// so the pass ends and the pull request goes to CI on the returned head.
fn passed_over(rig: &Rig) -> (Mutex<Runner>, String) {
    let (runner, head) = reviewed(rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    rig.forge.coderabbit.cubic_refuse(71, now(rig) + 20);
    rig.clock.advance(20);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    (runner, head)
}

// The late round's threads land, go to one fix turn, and the fix goes back
// to CI with no reviewer after it: the next verdict is the merge ruling's.
fn late_round_fixed(rig: &Rig, runner: &Mutex<Runner>) -> String {
    rig.clock.advance(super::SETTLE_LEAST);
    assert_eq!(step(runner).unwrap(), bot_reviewed(4, "cubic", 1));
    assert!(matches!(
        step(runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            round: 4,
            held: 1,
            ..
        })
    ));
    let reviewer_reads = rig.reviewer.seen().len();
    rig.claude
        .script([Scripted::Push("strip.txt", "stripped\n")]);
    step(runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(runner).unwrap(),
        Some(StepReport::FixPushed { round: 4, .. })
    ));
    assert_eq!(status(rig, runner)["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), reviewer_reads, "no new pass");
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    fixed
}

// shep-pm/shep#707: cubic was passed over for its window, the pass ended,
// and cubic reviewed the head on its own while CI ran. Its thread reaches
// the worker before any merge ruling, and the fix comes back to the ruling,
// which says no reviewer read it, and under `auto` is raised as under `ask`.
#[test]
fn a_bot_passed_over_that_reviews_during_ci_gets_a_fix_turn_before_the_merge_ruling() {
    for auto in [false, true] {
        let rig = listing("shep", &["cubic"]);
        if auto {
            rig.merge_auto();
        }
        let (runner, head) = passed_over(&rig);
        assert_eq!(status(&rig, &runner)["work_item"]["phase"]["state"], "ci");
        rig.forge
            .coderabbit
            .cubic_review(71, &head, now(&rig) + 60, &[FINDING]);
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(rig.verdict(&runner), None, "green CI reads cubic first");
        let status_now = status(&rig, &runner);
        assert_eq!(status_now["rulings"], json!([]));
        assert_eq!(status_now["work_item"]["bots_skipped"], json!(null));
        assert_eq!(
            status_now["work_item"]["phase"]["stage"]["stage"],
            "settling"
        );

        let fixed = late_round_fixed(&rig, &runner);
        let Some(StepReport::Ruling {
            id: 1, question, ..
        }) = rig.verdict(&runner)
        else {
            panic!("no merge ruling on the late round's fix, auto: {auto}");
        };
        assert!(question.contains(&fixed[..7]), "{question}");
        assert!(question.contains(LATE_FIX), "{question}");
        assert!(
            !question.contains("Review bot threads"),
            "the thread sent was resolved: {question}"
        );
        assert_eq!(rig.forge.merges(), [], "auto: {auto}");
    }
}

// A late review that lands while the merge ruling waits is read no sooner
// than LATE_READ after the ruling was raised. It withdraws the ruling, says
// so, and its fix comes back to a new merge ruling.
#[test]
fn a_bot_passed_over_that_reviews_while_the_merge_ruling_waits_withdraws_it() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = passed_over(&rig);
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { id: 1, .. }) = rig.verdict(&runner) else {
        panic!("no merge ruling");
    };
    assert!(
        rig.forge
            .pull_request_labels(71)
            .contains(&HUMAN.to_owned())
    );
    let logins = || rig.forge.coderabbit.logins().len();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let read_at_ruling = logins();

    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[FINDING]);
    rig.clock.advance(LATE_READ - 1);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(logins(), read_at_ruling, "no read before LATE_READ");
    assert_eq!(status(&rig, &runner)["rulings"][0]["id"], 1);

    rig.clock.advance(1);
    assert_eq!(step(&runner).unwrap(), None);
    let status_now = status(&rig, &runner);
    assert_eq!(status_now["rulings"], json!([]));
    assert_eq!(status_now["work_item"]["phase"]["state"], "review");
    assert!(
        !rig.forge
            .pull_request_labels(71)
            .contains(&HUMAN.to_owned())
    );
    let said = "Merge ruling 1 on pull request #71 is withdrawn: cubic reviewed its head \
                after the review pass went on without it, so the worker gets that review \
                first, and the merge ruling is raised again once CI is green after it. \
                Nothing to answer.";
    let notes = runner.lock().unwrap().take_notes();
    assert!(notes.iter().any(|n| n == said), "{notes:?}");
    let posts = rig.alerts.posts();
    let (_, alert) = posts.last().expect("posted to the webhook");
    assert_eq!(alert.title, "kelpie: shep merge ruling 1 withdrawn");
    assert_eq!(alert.text, said);

    let fixed = late_round_fixed(&rig, &runner);
    let Some(StepReport::Ruling {
        id: 2, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no merge ruling on the late round's fix");
    };
    assert!(question.contains(&fixed[..7]), "{question}");
    assert!(question.contains(LATE_FIX), "{question}");
}

// A clean late round pushes no fix, so when `main` moves on and kelpie
// catches the branch up, the head it pushes is not taken for a late fix:
// under `auto` it merges.
#[test]
fn a_catch_up_after_a_clean_late_round_is_no_late_fix() {
    let rig = listing("shep", &["cubic"]);
    rig.merge_auto();
    let (runner, head) = passed_over(&rig);
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[]);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(rig.verdict(&runner), None, "green CI reads cubic first");
    rig.clock.advance(super::SETTLE_LEAST);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(4, "cubic", 0));

    rig.land_on_origin("landed.txt");
    let Some(StepReport::Rebased { head: rebased, .. }) = step(&runner).unwrap() else {
        panic!("the branch was not caught up");
    };
    rig.forge.set_checks(&rebased, Checks::Passed);
    let merged = rig.verdict(&runner);
    assert!(
        matches!(merged, Some(StepReport::Finished { merged: true, .. })),
        "{merged:?}"
    );
    assert_eq!(rig.forge.merges(), [(71, rebased)]);
}

// A late read that finds nothing new costs no fix turn: while the ruling
// waits, a bot that has not reviewed leaves it standing, and one whose late
// review is clean goes back to CI and the merge ruling as before.
#[test]
fn a_late_read_with_nothing_new_costs_no_fix_turn() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = passed_over(&rig);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    let worker_turns = rig.claude.calls().len();
    let reads = rig.forge.coderabbit.logins().len();
    rig.clock.advance(LATE_READ);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.coderabbit.logins().len(), reads + 1, "cubic read");
    assert_eq!(status(&rig, &runner)["rulings"][0]["id"], 1);
    assert_eq!(rig.claude.calls().len(), worker_turns, "no fix turn");

    let rig = listing("shep", &["cubic"]);
    let (runner, head) = passed_over(&rig);
    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[]);
    rig.forge.set_checks(&head, Checks::Passed);
    let worker_turns = rig.claude.calls().len();
    assert_eq!(rig.verdict(&runner), None, "green CI reads cubic first");
    rig.clock.advance(super::SETTLE_LEAST);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(4, "cubic", 0));
    assert_eq!(status(&rig, &runner)["work_item"]["phase"]["state"], "ci");
    let Some(StepReport::Ruling {
        id: 1, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no merge ruling");
    };
    assert!(question.contains(&head[..7]), "{question}");
    assert!(!question.contains(LATE_FIX), "{question}");
    assert!(!question.contains("No reviewer read it"), "{question}");
    assert_eq!(rig.claude.calls().len(), worker_turns, "no fix turn");
}

// With one slot, #8 takes the slot #7's merge ruling freed. #7's late
// round, once its ruling is withdrawn, waits for the slot, and takes it when
// #8 parks on its own merge ruling.
#[test]
fn a_withdrawn_merge_ruling_waits_for_a_slot() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = passed_over(&rig);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    rig.forge.list_ready(8, false);
    // #7's branch touches a file, so #8 waits for the board to read its paths.
    assert_eq!(step(&runner).unwrap(), None);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 8, .. })
    ));
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    rig.claude.script([
        Scripted::Push("eight.txt", "eight\n"),
        Scripted::Text("CLEAN"),
    ]);
    // #8's turn, its qwen and Claude reads, and its draft marked ready.
    for _ in 0..4 {
        assert_eq!(issue_of(step(&runner).unwrap()), 8);
    }
    let eight = rig.forge.head_of("kelpie/8").unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { issue: 8, .. })
    ));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([]), json!([7])]);

    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[FINDING]);
    rig.clock.advance(LATE_READ);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(status(&rig, &runner)["rulings"], json!([]));
    assert_eq!(slots(&rig, &runner), [json!([8]), json!([7]), json!([])]);
    rig.clock.advance(super::SETTLE_LEAST);
    assert_eq!(step(&runner).unwrap(), None, "#7's round waits");

    // cubic reads #8 clean, and #8 parks on its own merge ruling.
    rig.forge
        .coderabbit
        .cubic_review(81, &eight, now(&rig) + 10, &[]);
    rig.clock.advance(10);
    assert_eq!(step(&runner).unwrap(), None, "#8's threads read once");
    rig.clock.advance(super::SETTLE_LEAST);
    assert_eq!(issue_of(step(&runner).unwrap()), 8, "cubic's read of #8");
    rig.forge.set_checks(&eight, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { issue: 8, .. })
    ));
    assert_eq!(slots(&rig, &runner), [json!([7]), json!([]), json!([8])]);
}

// Two bots passed over, and only cubic reviews late, at green CI. Once its
// late round's fix is pushed, CodeRabbit is read again before CI resumes,
// and no listed reviewer reads the fix.
#[test]
fn a_second_bot_passed_over_is_read_again_before_a_late_fix_goes_to_ci() {
    let rig = listing("shep", &["coderabbit", "cubic"]);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    rig.forge.coderabbit.refuse(71, now(&rig) + 20, 61);
    rig.clock.advance(30);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    rig.forge.coderabbit.cubic_refuse(71, now(&rig) + 20);
    rig.clock.advance(20);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 4, .. })
    ));
    step(&runner).unwrap(); // the pass's last round step: neither bot has reviewed
    assert_eq!(status(&rig, &runner)["work_item"]["phase"]["state"], "ci");

    rig.forge
        .coderabbit
        .cubic_review(71, &head, now(&rig) + 60, &[FINDING]);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(rig.verdict(&runner), None, "green CI reads both bots first");
    rig.clock.advance(super::SETTLE_LEAST);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(5, "cubic", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { round: 5, .. })
    ));
    let reviewer_reads = rig.reviewer.seen().len();
    let worker_turns = rig.claude.calls().len();
    rig.claude
        .script([Scripted::Push("strip.txt", "stripped\n")]);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { round: 5, .. })
    ));
    let coderabbit_reads = || {
        let logins = rig.forge.coderabbit.logins();
        logins.iter().filter(|l| l.contains("coderabbit")).count()
    };
    let before = coderabbit_reads();
    let status_now = status(&rig, &runner);
    assert_eq!(
        status_now["work_item"]["bots_skipped"][0]["reviewer"],
        "coderabbit"
    );
    assert_eq!(status_now["work_item"]["phase"]["state"], "review");

    step(&runner).unwrap(); // the pass's last round step reads CodeRabbit
    assert_eq!(coderabbit_reads(), before + 1, "CodeRabbit read again");
    assert_eq!(status(&rig, &runner)["work_item"]["phase"]["state"], "ci");
    assert_eq!(
        rig.reviewer.seen().len(),
        reviewer_reads,
        "qwen reads nothing"
    );
    assert_eq!(
        rig.claude.calls().len(),
        worker_turns + 1,
        "the fix turn, and no Claude read of it"
    );
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    let Some(StepReport::Ruling {
        id: 1, question, ..
    }) = rig.verdict(&runner)
    else {
        panic!("no merge ruling on the late round's fix");
    };
    assert!(question.contains(LATE_FIX), "{question}");
}
