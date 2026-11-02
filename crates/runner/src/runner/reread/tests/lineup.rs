//! What a start refuses of the agents a project names, which a reread refuses too

use std::sync::Mutex;

use crate::runner::Runner;
use crate::runner::contributions::tests::picking;
use crate::settings::Settings;
use crate::test::Rig;

fn refused(rig: &Rig, runner: &Mutex<Runner>, next: Settings) -> String {
    let mut runner = runner.lock().unwrap();
    let err = runner.reread(next, rig.kelpie_settings()).unwrap_err();
    err.to_string()
}

// Deletes every implementer file the rig wrote, as a project that never ran `agents init` has.
fn no_implementers(rig: &Rig) {
    for name in ["sonnet-high", "opus-high"] {
        std::fs::remove_file(rig.paths().agents.join(format!("{name}.md"))).unwrap();
    }
}

#[test]
fn a_representative_deleted_while_contributions_are_on_refuses_the_reread() {
    let (rig, runner) = picking("all");
    let before = rig.settings();
    let mut next = rig.settings();
    next.agents.representative = None;

    let err = refused(&rig, &runner, next);

    assert!(err.contains("`agents.representative` is not set"), "{err}");
    assert!(err.contains("agents init"), "{err}");
    assert_eq!(runner.lock().unwrap().settings(), &before);
}

#[test]
fn a_representative_deleted_while_contributions_and_triage_are_off_is_not_refused() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let mut next = rig.settings();
    next.agents.representative = None;

    let mut runner = runner.lock().unwrap();
    runner.reread(next, rig.kelpie_settings()).unwrap();
}

#[test]
fn a_project_with_no_implementer_refuses_to_start_naming_agents_init() {
    let rig = Rig::new("shep");
    no_implementers(&rig);

    let err = rig.open().expect_err("the runner refuses to start");

    let said = err.to_string();
    assert!(said.contains("no implementer"), "{said}");
    assert!(
        said.contains("`shep kelpie agents init <harness>`"),
        "{said}"
    );
}

#[test]
fn the_last_implementer_deleted_refuses_the_reread_and_the_old_settings_stay() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let before = rig.settings();
    no_implementers(&rig);

    let err = refused(&rig, &runner, rig.settings());

    assert!(err.contains("`shep kelpie agents init <harness>`"), "{err}");
    assert_eq!(runner.lock().unwrap().settings(), &before);
}
