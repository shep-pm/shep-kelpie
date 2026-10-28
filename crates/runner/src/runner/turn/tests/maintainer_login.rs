//! A Windows worker with no App token is handed the maintainer's gh login as `GH_TOKEN`
//!
//! Its sandbox user cannot read the maintainer's Credential Manager, so kelpie
//! reads the token with `gh auth token` when it builds the call.

use super::*;
use crate::ports::ForgeError;
use crate::test::LOGIN_TOKEN;

// The work item exists once its first turn has run, on the platform's own setting.
fn started(rig: &Rig, runner: &Mutex<Runner>) -> WorkItem {
    rig.claude.script([Scripted::Say("done")]);
    step(runner).unwrap();
    runner.lock().unwrap().current().expect("an item").clone()
}

#[test]
fn a_worker_with_no_app_token_gets_the_login_gh_prints_as_gh_token() {
    let (rig, runner) = with_issue_7("shep");
    let item = started(&rig, &runner);
    runner.lock().unwrap().hand_login_as_variable(true);

    let reach = runner
        .lock()
        .unwrap()
        .worker_reach(&item, Start::Main, None)
        .unwrap();

    let fence = reach.fence.expect("a worker is fenced");
    assert_eq!(fence.env["GH_TOKEN"], LOGIN_TOKEN);
    assert!(!format!("{fence:?}").contains(LOGIN_TOKEN));
    drop(rig);
}

#[test]
fn a_worker_the_app_covers_holds_the_apps_token_and_none_of_the_maintainers() {
    let (rig, runner) = with_issue_7("koji");
    rig.with_app();
    runner.lock().unwrap().hand_login_as_variable(true);
    rig.claude.script([Scripted::Say("done")]);

    step(&runner).unwrap();

    let [seen] = rig.claude.seen().try_into().unwrap();
    let fence = seen.call.reach.fence.expect("a worker is fenced");
    assert!(
        !fence.env.contains_key("GH_TOKEN"),
        "{:?}",
        fence.env.keys()
    );
}

#[test]
fn a_gh_login_that_cannot_be_read_stops_the_worker_before_it_starts_and_says_what_to_run() {
    let (rig, runner) = with_issue_7("shep");
    let item = started(&rig, &runner);
    runner.lock().unwrap().hand_login_as_variable(true);
    // A turn with the login readable starts a call, so one that does not is the login's doing.
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let before = rig.claude.all_calls().len();
    assert_eq!(before, 2, "the second turn did not start");
    rig.forge
        .set_login_token_down(ForgeError::Failed("not logged in".to_owned()));

    let refused = runner
        .lock()
        .unwrap()
        .worker_reach(&item, Start::Main, None)
        .unwrap_err();

    assert!(refused.contains("not logged in"), "{refused}");
    assert!(refused.contains("gh auth login"), "{refused}");
    assert!(refused.contains("shep kelpie github setup"), "{refused}");
    assert!(!refused.contains(LOGIN_TOKEN), "{refused}");
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    assert_eq!(
        rig.claude.all_calls().len(),
        before,
        "the worker started anyway"
    );
}
