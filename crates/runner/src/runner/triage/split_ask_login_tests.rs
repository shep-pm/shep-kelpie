//! Naming the login a split ask mentions, through the runner's stand-ins: what
//! a saved plan does when the forge cannot name one just now, and when it never
//! can at all.

use std::sync::Mutex;

use serde_json::{Value, json};

use super::answers_tests::step_until;
use super::ask_tests::{posted, wrote};
use super::pick_tests::MAINTAINER;
use super::split_ask_tests::saved_record;
use super::split_tests::{BODY, app_writes, draft, heard, planned_asking, saved_split};
use crate::ports::ForgeError;
use crate::runner::{Runner, step};
use crate::test::{Rig, VIEWER};

// A saved plan whose ask needs a maintainer login, on a project that names none
// in its settings and whose `gh` cannot be asked just now: the plan waits, it
// does not go over a call that failed
fn asking_with_no_login_to_name() -> (Rig, Mutex<Runner>) {
    let held = json!({ "comment": 101, "at": 4_000_000_000_u64 });
    let drafts = [
        draft("fix(a): first", BODY, &[]),
        draft("fix(b): second", BODY, &[0]),
    ];
    let (rig, runner) = planned_asking(&drafts, &[], held);
    drop(runner);
    rig.edit_settings(|s| s.replace(&format!("maintainer = \"{MAINTAINER}\"\n"), ""));
    rig.forge
        .set_viewer_down(ForgeError::Failed("no login".into()));
    let runner = rig.open().unwrap();
    (rig, runner)
}

#[test]
fn a_split_asks_again_when_no_maintainer_login_can_be_read() {
    let (rig, runner) = asking_with_no_login_to_name();
    let start = rig.github.writes().len();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        saved_split(&rig)["drafts"].as_array().map(Vec::len),
        Some(2),
        "the plan waits for a login to name, it does not go"
    );
    assert_eq!(
        saved_record(&rig)["step"],
        "body-written",
        "the pass is not ended"
    );
    assert_eq!(posted(&rig).len(), 0, "nothing is asked of nobody");
    assert_eq!(rig.github.issues_created(), 0);
    assert_eq!(
        app_writes(&rig, start),
        ["label #30 needs-approval"],
        "the parent waits at `needs-approval`"
    );
    let told: Vec<String> = runner.lock().unwrap().take_notes();
    assert!(
        told.iter().any(|n| n.contains("tried at the next poll")),
        "the failure is said: {told:?}"
    );

    rig.forge.set_viewer_up();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        posted(&rig).len(),
        1,
        "the ask goes up once a login can be named: {:?}",
        posted(&rig)
    );
    assert_ne!(
        saved_split(&rig)["asked"]["comment"].as_u64().unwrap(),
        saved_record(&rig)["comment"].as_u64().unwrap(),
        "the wait is dated from the ask"
    );
    assert_eq!(saved_record(&rig)["step"], "done");

    wrote(&rig, VIEWER, "yes");
    heard(&rig, &runner);
    step_until(&runner, || rig.github.issues_created() == 2);
    assert_eq!(rig.forge.sub_issues_of(30), [900, 901]);
}

// A project with no maintainer to ask at all: the plan is asked for as many
// times as a triage issue gets, then goes, saying so
#[test]
fn a_split_with_no_maintainer_to_ask_goes_when_its_tries_run_out() {
    let (rig, runner) = asking_with_no_login_to_name();

    for _ in 0..5 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }

    assert_eq!(
        saved_split(&rig),
        Value::Null,
        "no login across every try is no maintainer to ask"
    );
    assert_eq!(saved_record(&rig)["step"], "done");
    assert_eq!(posted(&rig).len(), 0, "nothing was ever asked");
    assert_eq!(rig.github.issues_created(), 0);
    let told: Vec<String> = runner.lock().unwrap().take_notes();
    assert!(
        told.iter()
            .any(|n| n.contains("none is named, so it is dropped")),
        "the plan going is said: {told:?}"
    );
}
