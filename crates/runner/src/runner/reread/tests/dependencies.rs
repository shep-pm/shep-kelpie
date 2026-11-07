use crate::ports::Checks;
use crate::runner::dependencies::tests::{bump, bumping, bumps, poll, settle};
use crate::runner::{CHECKS_SETTLE, Pass, advance, step};
use crate::test::{CALL_PATIENCE, Hold, Rig};

const ON: &str = "pick = \"on\"";
const OFF: &str = "pick = \"off\"";

#[test]
fn turning_pick_on_records_a_listed_bots_bump_without_a_restart() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);
    settle(&rig, &runner);
    assert!(bumps(&rig, &runner).is_empty());

    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.dependencies]\n{ON}\n"));
    let line = (runner.lock().unwrap()).reread(rig.settings(), rig.kelpie_settings());
    assert_eq!(
        line.unwrap().as_deref(),
        Some("settings changed: dependencies now in effect")
    );

    settle(&rig, &runner);
    assert_eq!(rig.forge.merges(), [(40, head)]);
}

#[test]
fn turning_pick_off_records_and_merges_no_bump() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));
    settle(&rig, &runner);
    assert_eq!(bumps(&rig, &runner).len(), 1);

    rig.edit_settings(|s| s.replace(ON, OFF));
    let line = (runner.lock().unwrap()).reread(rig.settings(), rig.kelpie_settings());
    assert_eq!(
        line.unwrap().as_deref(),
        Some("settings changed: dependencies now in effect")
    );
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);
    assert!(bumps(&rig, &runner).is_empty(), "the record is forgotten");
    assert!(rig.forge.merges().is_empty());
}

#[test]
fn a_changed_wait_hours_applies_to_a_bump_already_recorded() {
    let (rig, runner) = bumping("wait_hours = 24");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));
    settle(&rig, &runner);
    rig.clock.advance(2 * 3600);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.forge.pull_request_closings().is_empty(), "24 hours");

    rig.edit_settings(|s| s.replace("wait_hours = 24", "wait_hours = 2"));
    (runner.lock().unwrap())
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    poll(&rig, &runner);

    assert_eq!(rig.forge.pull_request_closings(), [40]);
    let comments = rig.forge.comments();
    assert!(comments[0].1.contains("2 hours"), "{comments:?}");
}

#[test]
fn turning_pick_off_forgets_a_red_bump_with_its_close_steps_left_as_a_start_does() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_pull_request_closes_down(true);
    settle(&rig, &runner);
    assert_eq!(rig.forge.comments().len(), 1);
    assert!(rig.forge.pull_request_closings().is_empty());

    rig.edit_settings(|s| s.replace(ON, OFF));
    (runner.lock().unwrap())
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    rig.forge.set_pull_request_closes_down(false);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert!(bumps(&rig, &runner).is_empty());
    assert!(rig.forge.pull_request_closings().is_empty());
    assert!(rig.forge.created().is_empty());
}

#[test]
fn an_e2e_run_in_flight_when_pick_goes_off_finishes_and_merges_nothing() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    let hold = Hold::default();
    rig.e2e.hold_next(hold.clone());
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the run never began");

    rig.edit_settings(|s| s.replace(ON, OFF));
    (runner.lock().unwrap())
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    poll(&rig, &runner);
    assert!(bumps(&rig, &runner).is_empty());

    // A drain reports when the last call has ended.
    rig.ask(&runner, "drain", None);
    hold.release();
    assert!(hold.answered(CALL_PATIENCE), "the run never ended");
    let by = std::time::Instant::now() + CALL_PATIENCE;
    while !rig.ask(&runner, "status", None)["draining"]["calls"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        assert!(std::time::Instant::now() < by, "the run never ended");
        std::thread::sleep(std::time::Duration::from_millis(10));
        step(&runner).unwrap();
    }
    poll(&rig, &runner);

    assert_eq!(
        rig.e2e.ran().len(),
        1,
        "the run finished and is not asked again"
    );
    assert!(rig.forge.merges().is_empty());
    assert!(bumps(&rig, &runner).is_empty());
}
