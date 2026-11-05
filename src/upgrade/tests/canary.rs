//! The canary: who goes first, what the rest wait for, and what fails it

use super::*;

// A runner that is healthy, with #7 going on and a worker turn running.
const WORKING: &str = r#"{"build":"{build}","passes":1,"working":["7"],
    "work_items":[{"issue":"7","phase":{"state":"implement"},"turn":{"state":"running","since":1}}]}"#;

// A runner that has not yet finished a full pass of its loop.
const NO_COUNT: &str = r#"{"build":"{build}","work_items":[]}"#;
const NO_PASS: &str = r#"{"build":"{build}","passes":0,"work_items":[]}"#;

// Another build than the installed one.
const ANOTHER_BUILD: &str = r#"{"build":"1 0.000000000","passes":1,"work_items":[]}"#;

const REFUSED: &str = r#"{"build":"{build}","passes":0,"work_items":[],
    "settings_refused":{"reason":"unknown key `colour`","at":1}}"#;

const FAILING: &str = r#"{"build":"{build}","passes":1,
    "work_items":[{"issue":"7","phase":{"state":"implement"},
    "failing":{"failures":3,"since":1,"reason":"the forge refused the push"}}]}"#;

const TURN_FAILED: &str = r#"{"build":"{build}","passes":1,
    "work_items":[{"issue":"7","phase":{"state":"implement"},
    "turn":{"state":"failed","at":1,"reason":"the worker's session was lost"}}]}"#;

const ROLLBACK: &str = "shep kelpie upgrade --rollback";

fn restarted(writes: &[Request]) -> Vec<&str> {
    (writes.iter())
        .filter_map(|w| match w {
            Request::Restart {
                selector: SelectorSpec::Name(name),
            } => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

fn triggers<'a>(writes: &'a [Request], sheep: &str) -> Vec<&'a str> {
    (writes.iter())
        .filter_map(|w| match w {
            Request::Trigger {
                selector: SelectorSpec::Name(n),
                action,
                ..
            } if n == sheep => Some(action.as_str()),
            _ => None,
        })
        .collect()
}

/// How many `status` looks the canary `aria` had between its restart and `koji`'s
fn looks_before_the_rest(writes: &[Request]) -> usize {
    let restart = |name: &str| {
        writes
            .iter()
            .position(
                |w| matches!(w, Request::Restart { selector: SelectorSpec::Name(n) } if n == name),
            )
            .unwrap()
    };
    writes[restart("aria")..restart("koji")]
        .iter()
        .filter(|w| {
            matches!(w, Request::Trigger { selector: SelectorSpec::Name(n), action, .. }
                if n == "aria" && action == "status")
        })
        .count()
}

fn told(said: &[String], what: &str) -> Option<usize> {
    said.iter().position(|l| l.contains(what))
}

// `aria` sorts first, so it is the canary. Its scripted looks: before the dog,
// then `picked` for the pick, the drain and the merge wait (the last look before
// the restart), then `after` from the restart on.
fn canary_says(rig: &Rig, picked: &str, after: &str) {
    rig.runs("aria", &rig.installed.clone(), true);
    rig.shepherd
        .says("aria", &[IDLE, picked, picked, picked, after]);
}

// What a failed canary leaves: only the dog and the canary restarted, no other
// runner asked for more than its two looks, and nothing recorded as restarted onto.
fn assert_stopped_at_the_canary(rig: &mut Rig, err: &str, failed: &str) {
    assert!(err.contains(failed), "{err}");
    assert!(err.contains(ROLLBACK), "{err}");
    assert!(!err.contains("To finish the restarts"), "{err}");
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "aria"]);
    assert_eq!(
        triggers(&writes, "koji"),
        ["status", "status"],
        "no other runner was drained, handed over or restarted: {writes:?}"
    );
    assert!(!rig.home.join("builds/installed.seen").exists());
}

#[tokio::test]
async fn the_dog_restarts_first_then_the_canary_and_no_other_runner_before_its_pass() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    rig.runs("zed", &rig.installed.clone(), true);
    // `aria` shows no pass for a few looks after its restart.
    rig.shepherd.says(
        "aria",
        &[IDLE, IDLE, IDLE, IDLE, NO_PASS, NO_PASS, NO_PASS, IDLE],
    );
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "aria", "koji", "zed"]);
    let at = |name: &str| {
        writes.iter().position(
            |w| matches!(w, Request::Restart { selector: SelectorSpec::Name(n) } if n == name),
        )
    };
    let others_before_the_canary_restarts =
        |name: &str| triggers(&writes[..at("aria").unwrap()], name);
    assert_eq!(
        others_before_the_canary_restarts("koji"),
        ["status", "status"]
    );
    assert_eq!(
        others_before_the_canary_restarts("zed"),
        ["status", "status"]
    );
    assert!(told(&said, "restarted `kelpie`") < told(&said, "restarted `aria`"));
    assert!(told(&said, "restarted `aria`") < told(&said, "restarted `koji`"));
}

#[tokio::test]
async fn the_canary_is_the_first_runner_to_show_nothing_in_flight() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // `aria` sorts first and is quiet at the dog's look, then merging at the pick.
    rig.shepherd.says("aria", &[IDLE, MERGING, IDLE]);
    // `koji` has a worker turn going, which does not hold it back.
    rig.shepherd.says("koji", &[WORKING]);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    assert_eq!(rig.restarts(), ["kelpie", "koji", "aria"]);
    assert!(
        told(
            &said,
            "`koji` shows no merge in flight, so it goes first, as the canary"
        )
        .is_some(),
        "{said:?}"
    );
    assert!(told(&said, "`aria` shows no merge in flight").is_none());
}

#[tokio::test]
async fn two_quiet_runners_at_one_look_pick_the_first_by_name() {
    let mut rig = Rig::new().await;
    // Added after `koji` and `zed`, so insertion order does not decide it.
    rig.runs("zed", &rig.installed.clone(), true);
    rig.runs("aria", &rig.installed.clone(), true);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes)[..2], ["kelpie", "aria"]);
    assert!(
        told(
            &said,
            "`aria` shows no merge in flight, so it goes first, as the canary"
        )
        .is_some(),
        "{said:?}"
    );
}

#[tokio::test]
async fn the_other_runners_are_not_drained_or_handed_over_while_the_canary_is_checked() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    rig.runs("zed", &rig.installed.clone(), true);
    // `aria` never shows a first pass, so the check goes on until Ctrl-C.
    rig.shepherd
        .says("aria", &[IDLE, IDLE, IDLE, IDLE, NO_PASS]);
    let new = rig.build("new", "0.3.0", "0.24.2");
    let scene = Scene {
        interrupt: Interrupt::After(Duration::from_millis(300)),
        ..rig.scene(FAST)
    };

    let (ran, _) = rig
        .upgrade_in(scene, Action::Install(Source::Binary(new)))
        .await;

    let err = ran.unwrap_err();
    assert!(
        err.contains("stopped by Ctrl-C while the canary `aria` was checked"),
        "{err}"
    );
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "aria"]);
    for other in ["koji", "zed"] {
        assert_eq!(triggers(&writes, other), ["status", "status"], "{writes:?}");
    }
}

#[tokio::test]
async fn the_rest_wait_for_the_canarys_first_pass() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // No pass for the looks the steps before the restart take and for a few
    // after it, then the first.
    let mut looks = vec![NO_PASS; 12];
    looks.push(IDLE);
    rig.shepherd.says("aria", &looks);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, _) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "aria", "koji"]);
    let restart = |name: &str| {
        writes
            .iter()
            .position(
                |w| matches!(w, Request::Restart { selector: SelectorSpec::Name(n) } if n == name),
            )
            .unwrap()
    };
    let (canary, rest) = (restart("aria"), restart("koji"));
    let looks_between = writes[canary..rest]
        .iter()
        .filter(|w| {
            matches!(w, Request::Trigger { selector: SelectorSpec::Name(n), action, .. }
                if n == "aria" && action == "status")
        })
        .count();
    assert!(
        looks_between >= 4,
        "`koji` waited for the canary's first pass: {looks_between} looks"
    );
    let last_look = writes[..rest]
        .iter()
        .rposition(|w| {
            matches!(w, Request::Trigger { selector: SelectorSpec::Name(n), action, .. }
                if n == "aria" && action == "status")
        })
        .unwrap();
    assert_eq!(
        triggers(&writes[..last_look], "koji"),
        ["status", "status"],
        "`koji` was neither handed over nor drained before the pass: {writes:?}"
    );
}

#[tokio::test]
async fn a_runner_waiting_on_a_merge_does_not_hold_back_the_runners_after_it() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    rig.runs("zed", &rig.installed.clone(), true);
    // `aria` is quiet at the dog's look and then merges for good.
    rig.shepherd.says("aria", &[IDLE, MERGING]);
    let new = rig.build("new", "0.3.0", "0.24.2");
    let patience = Patience {
        merge: Duration::from_millis(300),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));

    let (ran, said) = rig.upgrade_within(action, patience).await;

    let err = ran.unwrap_err();
    assert!(err.contains("`aria` is merging #7"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie", "koji", "zed"]);
    assert!(told(&said, "waiting: `aria` is merging #7").is_some());
    assert!(told(&said, "restarted `zed`").is_some(), "{said:?}");
}

#[tokio::test]
async fn no_runner_going_quiet_within_the_merge_wait_restarts_nothing_and_names_each() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // Both are quiet at the dog's look and merging from the pick on.
    for name in ["aria", "koji"] {
        rig.shepherd.says(name, &[IDLE, MERGING]);
    }
    let new = rig.build("new", "0.3.0", "0.24.2");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));

    let (ran, _) = rig.upgrade_within(action, patience).await;

    let err = ran.unwrap_err();
    assert!(err.contains("`aria` is merging #7"), "{err}");
    assert!(err.contains("`koji` is merging #7"), "{err}");
    assert!(err.contains("so no runner was restarted"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie"]);
}

#[tokio::test]
async fn a_pick_stopped_meanwhile_stays_stopped_and_picking_goes_on() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    let new = rig.build("new", "0.3.0", "0.24.2");
    let scene = rig.scene(FAST);
    let action = Action::Install(Source::Binary(new));
    let mut said = Vec::new();

    let ran = run(&scene, &action, &mut |line| {
        // `aria` is stopped once it has been picked, before its turn comes.
        if line.contains("`aria` shows no merge in flight") {
            rig.shepherd.stops("aria");
        }
        said.push(line);
    })
    .await;

    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(!rig.shepherd.sheep("aria").unwrap().1);
    assert!(
        told(&said, "`aria` was stopped meanwhile, so it stays stopped").is_some(),
        "{said:?}"
    );
    assert!(
        told(
            &said,
            "`koji` shows no merge in flight, so it goes first, as the canary"
        )
        .is_some(),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_canary_showing_another_build_fails_and_no_other_runner_is_touched() {
    let mut rig = Rig::new().await;
    canary_says(&rig, IDLE, ANOTHER_BUILD);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let err = rig.install(&new).await.0.unwrap_err();

    assert_stopped_at_the_canary(
        &mut rig,
        &err,
        "the canary `aria` shows build 1 0.000000000, not the installed",
    );
}

#[tokio::test]
async fn a_canary_showing_another_commit_fails_and_no_other_runner_is_touched() {
    let mut rig = Rig::new().await;
    canary_says(&rig, IDLE, &on("1f2e3a4"));
    let new = rig.built_from("new", "0.3.0", "9c8b7a6");

    let err = rig.install(&new).await.0.unwrap_err();

    assert_stopped_at_the_canary(
        &mut rig,
        &err,
        "the canary `aria` shows commit 1f2e3a4, not the installed 9c8b7a6",
    );
}

#[tokio::test]
async fn a_canary_refusing_its_settings_fails_and_no_other_runner_is_touched() {
    let mut rig = Rig::new().await;
    canary_says(&rig, IDLE, REFUSED);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let err = rig.install(&new).await.0.unwrap_err();

    assert_stopped_at_the_canary(
        &mut rig,
        &err,
        "the canary `aria` refuses its settings: unknown key `colour`",
    );
}

#[tokio::test]
async fn a_canary_whose_step_newly_fails_fails_and_no_other_runner_is_touched() {
    let mut rig = Rig::new().await;
    canary_says(&rig, IDLE, FAILING);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let err = rig.install(&new).await.0.unwrap_err();

    assert_stopped_at_the_canary(
        &mut rig,
        &err,
        "the canary `aria` shows #7's step failing, which it did not before: the forge refused the push",
    );
}

#[tokio::test]
async fn a_canary_whose_working_ticket_is_now_failed_fails_and_no_other_runner_is_touched() {
    let mut rig = Rig::new().await;
    canary_says(&rig, WORKING, TURN_FAILED);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let err = rig.install(&new).await.0.unwrap_err();

    assert_stopped_at_the_canary(
        &mut rig,
        &err,
        "the canary `aria` shows #7's worker turn failed, which was going on before: the worker's session was lost",
    );
}

#[tokio::test]
async fn a_canary_that_answers_no_status_within_the_start_wait_fails_and_no_other_runner_is_touched()
 {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // Four looks before the restart, then silence.
    rig.shepherd.replies(
        "aria",
        &[Some(IDLE), Some(IDLE), Some(IDLE), Some(IDLE), None],
    );
    let new = rig.build("new", "0.3.0", "0.24.2");
    let patience = Patience {
        start: Duration::from_millis(50),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));

    let (ran, said) = rig.upgrade_within(action, patience).await;
    let err = ran.unwrap_err();

    assert_stopped_at_the_canary(
        &mut rig,
        &err,
        "the canary `aria` did not answer in 0s after its restart",
    );
    // No other runner goes on first: the line must not say they do.
    assert!(told(&said, "go on first").is_none(), "{said:?}");
}

#[tokio::test]
async fn a_step_failing_before_and_after_the_restart_does_not_fail_the_canary() {
    let mut rig = Rig::new().await;
    canary_says(&rig, FAILING, FAILING);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, _) = rig.install(&new).await;

    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "aria", "koji"]);
}

#[tokio::test]
async fn a_step_that_began_failing_while_the_canary_drained_does_not_fail_it() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // Clean at the pick, failing at the merge wait just before the restart,
    // and still failing after it: the old build's failure.
    rig.shepherd
        .says("aria", &[IDLE, IDLE, IDLE, FAILING, FAILING]);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, _) = rig.install(&new).await;

    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "aria", "koji"]);
}

#[tokio::test]
async fn a_canary_that_counts_no_passes_yet_waits_for_its_first_instead_of_passing() {
    // A current runner leaves `passes` out until its first pass ends.
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    let mut looks = vec![IDLE; 4];
    looks.extend([NO_COUNT; 12]);
    looks.push(IDLE);
    rig.shepherd.says("aria", &looks);
    let new = rig.build("new", "0.3.0", "0.24.2");

    let (ran, _) = rig.install(&new).await;

    ran.unwrap();
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "aria", "koji"]);
    let waited = looks_before_the_rest(&writes);
    assert!(
        (12..100).contains(&waited),
        "`koji` waited for the pass, not for the start wait: {waited} looks"
    );
}

#[tokio::test]
async fn a_canary_from_before_passes_passes_once_the_start_wait_has_gone_by_without_a_count() {
    // A rollback onto such a build must not wait for a count it never sends.
    for after in [NO_COUNT, r#"{"work_items":[]}"#] {
        let mut rig = Rig::new().await;
        canary_says(&rig, IDLE, after);
        let new = rig.build("new", "0.3.0", "0.24.2");

        let (ran, said) = rig.install(&new).await;

        ran.unwrap();
        // The start wait is 10s, a look about every 10ms.
        let writes = rig.shepherd.writes();
        let waited = looks_before_the_rest(&writes);
        assert!(waited >= 500, "{after}: {waited} looks");
        assert_eq!(restarted(&writes), ["kelpie", "aria", "koji"], "{after}");
        assert!(
            told(&said, "counts no passes").is_some(),
            "{after}: {said:?}"
        );
    }
}

#[tokio::test]
async fn a_bare_upgrade_whose_canary_fails_offers_no_rollback() {
    let mut rig = Rig::new().await;
    canary_says(&rig, IDLE, ANOTHER_BUILD);
    rig.cargo_installs("0.3.0", "0.24.2");

    let err = rig.upgrade(Action::Installed).await.0.unwrap_err();

    assert!(err.contains("the canary `aria` shows build"), "{err}");
    assert!(
        err.contains("The dog and the canary run the new build"),
        "{err}"
    );
    assert!(!err.contains("--rollback"), "{err}");
    assert!(!err.contains("To finish the restarts"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie", "aria"]);
    assert!(!rig.home.join("builds/installed.seen").exists());
}

#[tokio::test]
async fn a_rollbacks_failed_canary_says_a_second_rollback_swaps_forward_again() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    let first = rig.build("first", "0.2.0", "0.24.2");
    let second = rig.build("second", "0.3.0", "0.24.2");
    rig.install(&first).await.0.unwrap();
    rig.install(&second).await.0.unwrap();
    rig.restarts();
    rig.shepherd
        .says("aria", &[IDLE, IDLE, IDLE, IDLE, ANOTHER_BUILD]);

    let err = rig.upgrade(Action::Rollback).await.0.unwrap_err();

    assert!(err.contains("the canary `aria` shows build"), "{err}");
    assert!(
        err.contains("`shep kelpie upgrade --rollback` swaps the builds forward again"),
        "{err}"
    );
    assert!(
        err.contains("The dog and the canary run the rolled back build"),
        "{err}"
    );
    assert_eq!(rig.restarts(), ["kelpie", "aria"]);
}

// A healthy runner that shows `commit`.
fn on(commit: &str) -> String {
    format!(r#"{{"build":"{{build}}","passes":1,"work_items":[],"commit":"{commit}"}}"#)
}
