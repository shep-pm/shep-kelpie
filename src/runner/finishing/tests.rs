use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::runner::{CHECKS_SETTLE, step};
use crate::test::Rig;

// Issue 7 parked on merge ruling 1 in a project with two slots, so the
// board could open ready issue 9 beside it
fn parked_with_9_ready(project: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = Rig::parked_set(project, |rig| {
        rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    });
    rig.forge.list_ready(9, false);
    (rig, runner, head)
}

fn take_stop(runner: &Mutex<Runner>) -> bool {
    runner.lock().unwrap().take_stop()
}

fn saved(rig: &Rig) -> serde_json::Value {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn finish_lets_an_open_item_reach_its_merge_with_no_new_dispatch_then_stops() {
    let (rig, runner, head) = parked_with_9_ready("koji");
    let status = rig.ask(&runner, "finish", None);
    assert_eq!(
        (&status["run"], &status["parked"]),
        (&json!("finishing"), &json!([7]))
    );
    assert_eq!(saved(&rig)["finishing"], true);

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    // Parked on its ruling, #7 is still open, so the runner stays up for the answer.
    for _ in 0..3 {
        rig.clock.advance(3600);
        assert_eq!(step(&runner).unwrap(), None, "the board opened #9");
    }
    assert!(!take_stop(&runner));

    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { issue: 7, .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished {
            issue: 7,
            merged: true,
            ..
        })
    ));
    assert_eq!(rig.forge.merges(), [(71, head)]);
    assert!(
        !take_stop(&runner),
        "not before the pass that finds none open"
    );

    assert_eq!(step(&runner).unwrap(), Some(StepReport::RunFinished));
    assert!(take_stop(&runner));
    assert!(!take_stop(&runner), "the stop is taken once");
    let (_, alert) = rig.alerts.posts().pop().unwrap();
    assert_eq!(alert.title, "kelpie: koji finished");
    assert_eq!(
        alert.text,
        "koji's runner finished its open work items and picked nothing new, so it stops: \
         `shep kelpie start koji` runs it again. Nothing to answer."
    );
    assert_eq!(runner.lock().unwrap().take_notes(), [alert.text]);

    // Until the stop lands the board stays shut, and a restart picks again.
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["run"], &status["work_items"]),
        (&json!("finished"), &json!([]))
    );
    assert_eq!(saved(&rig).get("finishing"), None);
}

#[test]
fn finish_with_no_work_item_open_finishes_on_the_next_pass() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge.list_ready(4, false);
    assert_eq!(rig.ask(&runner, "finish", None)["run"], "finishing");
    assert_eq!(step(&runner).unwrap(), Some(StepReport::RunFinished));
    assert!(take_stop(&runner));
    assert_eq!(rig.forge.calls(), 0, "the board was never read");
}

#[test]
fn add_is_refused_while_the_runner_finishes() {
    let rig = Rig::new("rotom");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "finish", None);
    let refused = json!({
        "error": "the runner is finishing, so it opens no new work item: `shep kelpie start` \
                  lets it pick again"
    });
    assert_eq!(rig.ask(&runner, "add", Some("8")), refused);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["working"], json!([7]));
}

#[test]
fn start_while_finishing_lets_the_board_pick_again() {
    let (rig, runner, _) = parked_with_9_ready("golbat");
    rig.ask(&runner, "finish", None);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), None);

    let status = rig.ask(&runner, "start", None);
    assert_eq!(status.get("run"), None);
    assert_eq!(saved(&rig).get("finishing"), None);
    // The board reads #9's paths against the parked branch on its first pass.
    assert_eq!(step(&runner).unwrap(), None);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 9, .. })
    ));
}

#[test]
fn start_after_the_runner_finished_lets_the_board_pick_again() {
    let rig = Rig::new("eevee");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "finish", None);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::RunFinished));
    rig.forge.list_ready(4, false);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "start", None).get("run"), None);
    assert!(!take_stop(&runner), "a stop not taken goes with the start");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 4, .. })
    ));
}

#[test]
fn a_runner_restarted_while_finishing_comes_back_finishing() {
    let (rig, runner, _) = parked_with_9_ready("ditto");
    rig.ask(&runner, "finish", None);
    drop(runner);

    let runner = rig.open().unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["run"], &status["parked"]),
        (&json!("finishing"), &json!([7]))
    );
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None, "the board opened #9");
    }
    assert!(!take_stop(&runner));
}

#[test]
fn the_board_briefing_says_the_runner_is_finishing() {
    let rig = Rig::new("reactmap");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "drain", None);
    let board = || std::fs::read_to_string(rig.paths().board).unwrap();
    let finishing = "\nThe runner is finishing: the board opens nothing new, and the runner \
                     stops once the open work items end.\n";
    rig.ask(&runner, "finish", None);
    step(&runner).unwrap();
    assert!(board().contains(finishing), "{}", board());

    rig.ask(&runner, "drop", Some("7"));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::RunFinished));
    let finished = "\nThe runner finished its open work items and is stopping: the board \
                    opens nothing new.\n";
    assert!(board().contains(finished), "{}", board());

    rig.ask(&runner, "start", None);
    step(&runner).unwrap();
    assert!(!board().contains("The runner"), "{}", board());
}

#[test]
fn adopt_and_rework_are_refused_while_the_runner_finishes() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.ask(&runner, "finish", None);
    let refused = json!({
        "error": "the runner is finishing, so it takes on no pull request: `shep kelpie start` \
                  lets it pick again"
    });
    assert_eq!(rig.ask(&runner, "adopt", Some("71")), refused);
    assert_eq!(rig.ask(&runner, "rework", Some("71")), refused);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["adopted"], &status["work_items"]),
        (&json!([]), &json!([]))
    );
    assert_eq!(rig.forge.calls(), 0, "the pull request was never read");
}

#[test]
fn a_pause_while_finishing_leaves_a_runner_that_comes_back_picking() {
    let (rig, runner, _) = parked_with_9_ready("rotom");
    rig.ask(&runner, "finish", None);
    let status = rig.ask(&runner, "pausing", None);
    assert_eq!(
        status["run"], "finishing",
        "it goes on finishing until the stop"
    );
    assert_eq!(saved(&rig).get("finishing"), None);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), None);

    // A refused stop sends `finish` again, which saves it again.
    rig.ask(&runner, "finish", None);
    assert_eq!(saved(&rig)["finishing"], true);
    rig.ask(&runner, "pausing", None);
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "status", None).get("run"), None);
}

#[test]
fn a_notice_still_due_holds_the_finish_until_it_is_posted() {
    let rig = Rig::new("eevee");
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        crate::test::Scripted::Push("work.txt", "work\n"),
        crate::test::Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, crate::ports::Checks::Passed);
    rig.ask(&runner, "finish", None);
    rig.alerts.set_down(true);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { merged: true, .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::NoticeFailed { .. })
    ));
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "finished with the notice unsent"
    );
    assert!(!take_stop(&runner));

    rig.alerts.set_down(false);
    rig.clock.advance(60);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Noticed { .. })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::RunFinished));
    assert!(take_stop(&runner));
}

#[test]
fn finish_and_start_take_no_params() {
    let rig = Rig::new("chelone");
    let runner = rig.open().unwrap();
    for action in ["finish", "start"] {
        assert_eq!(
            rig.ask(&runner, action, Some("now")),
            json!({ "error": format!("`{action}` takes no params") })
        );
    }
    assert_eq!(rig.ask(&runner, "status", None).get("run"), None);
}
