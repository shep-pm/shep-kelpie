//! Handing each runner over to the new build, and draining it when it cannot

use super::*;

// A runner still on the old build, and one that reports the installed file
const OLD: &str = r#"{"build":"1 1.000000000","work_items":[]}"#;
const NEW: &str = r#"{"build":"{build}","work_items":[]}"#;

const REFUSED: &str = r#"{"error":"handover refused: a call has just ended and is being read"}"#;

// A runner draining with no call left
const DRAINED: &str = r#"{"work_items":[],"draining":{"calls":[],"ceiling":3600}}"#;

// The triggers sent to `sheep`, by action
fn triggers(writes: &[Request], sheep: &str) -> Vec<String> {
    (writes.iter())
        .filter_map(|w| match w {
            Request::Trigger {
                selector: SelectorSpec::Name(n),
                action,
                ..
            } if n == sheep => Some(action.clone()),
            _ => None,
        })
        .collect()
}

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

#[tokio::test]
async fn upgrade_hands_over_a_runner_that_can_and_drains_none() {
    let mut rig = Rig::new().await;
    // The dog's look, then the old build until the handover lands. The old
    // image never answers `handover`: it is replaced as it takes it.
    rig.shepherd.says("koji", &[IDLE, OLD, NEW]);
    rig.shepherd.hands("koji", &[None]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie"], "only the dog restarts");
    assert_eq!(
        triggers(&writes, "koji"),
        ["status", "handover", "status", "status"]
    );
    assert!(
        said.iter()
            .any(|l| l == "`koji` handed over to the new build"),
        "{said:?}"
    );
}

#[tokio::test]
async fn upgrade_drains_a_runner_that_predates_the_handover() {
    let mut rig = Rig::new().await;
    // An old build answers `status` and takes no `handover`, twice.
    rig.shepherd.says("koji", &[IDLE, IDLE, DRAINED]);
    rig.shepherd.hands(
        "koji",
        &[
            Some("unknown action: handover"),
            Some("unknown action: handover"),
        ],
    );
    let new = rig.build("new", "0.3.0", "0.19.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "koji"]);
    assert_eq!(
        triggers(&writes, "koji"),
        [
            "status", "handover", "status", "handover", "drain", "status", "status", "status"
        ]
    );
    assert!(
        said.iter().any(|l| l
            == "`koji` runs a kelpie from before `handover`, so it is drained and restarted as before"),
        "{said:?}"
    );
}

#[tokio::test]
async fn upgrade_drains_a_runner_that_refuses_and_names_the_reason() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[IDLE, DRAINED]);
    rig.shepherd.hands("koji", &[Some(REFUSED)]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "koji"]);
    assert_eq!(
        triggers(&writes, "koji"),
        ["status", "handover", "drain", "status", "status", "status"]
    );
    assert!(
        said.iter().any(|l| l
            == "`koji` refused to hand over: a call has just ended and is being read, so it is \
                drained and restarted as before"),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_handover_that_never_shows_the_new_build_is_drained_and_named() {
    let mut rig = Rig::new().await;
    rig.shepherd.says(
        "koji",
        &[IDLE, OLD, OLD, OLD, OLD, OLD, OLD, OLD, OLD, DRAINED],
    );
    rig.shepherd.hands(
        "koji",
        &[Some(
            r#"{"handover":"waiting for the runner's quiet point"}"#,
        )],
    );
    let new = rig.build("new", "0.3.0", "0.19.0");
    let patience = Patience {
        start: Duration::from_millis(30),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let (ran, said) = rig.upgrade_within(action, patience).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "koji"]);
    let koji = triggers(&writes, "koji");
    let withdrawn = koji.iter().position(|a| a == "withdraw_handover");
    let drained = koji.iter().position(|a| a == "drain");
    assert!(
        withdrawn.is_some() && withdrawn < drained,
        "the queued handover is withdrawn before the drain: {koji:?}"
    );
    assert!(
        said.iter().any(|l| l
            == "`koji` still showed the old build within 30ms of its handover, so it is drained \
                and restarted as before"),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_stopped_while_it_hands_over_stays_stopped() {
    let mut rig = Rig::new().await;
    let looks = [IDLE, OLD, OLD, OLD, OLD, OLD, OLD, OLD, OLD, OLD];
    rig.shepherd.says("koji", &looks);
    rig.shepherd.hands("koji", &[None]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    // Stopped once the upgrade has looked at it a few times.
    let stopping = async {
        while rig.shepherd.answers_left("koji") > looks.len() - 4 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        rig.shepherd.stops("koji");
    };
    let ((ran, said), ()) = tokio::join!(rig.install(&new), stopping);
    ran.unwrap();

    assert_eq!(rig.restarts(), ["kelpie"]);
    assert!(!rig.shepherd.sheep("koji").unwrap().1);
    assert!(
        said.iter()
            .any(|l| l == "`koji` was stopped meanwhile, so it stays stopped"),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_that_shows_no_build_is_said_to_show_none() {
    let rig = Rig::new().await;
    rig.shepherd
        .says("koji", &[IDLE, r#"{"work_items":[]}"#, DRAINED]);
    rig.shepherd.hands("koji", &[None]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let patience = Patience {
        start: Duration::from_millis(30),
        ..FAST
    };
    let (ran, said) = rig
        .upgrade_within(Action::Install(Source::Binary(new)), patience)
        .await;
    ran.unwrap();
    assert!(
        said.iter()
            .any(|l| l.contains("`koji` showed no build in its `status` within 30ms")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_still_opening_is_not_taken_for_a_handover_in_progress() {
    let rig = Rig::new().await;
    // Neither `handover` nor `status` is taken yet, and never is.
    let opening = Some("unknown action: status");
    rig.shepherd.replies("koji", &[Some(IDLE), opening]);
    rig.shepherd
        .hands("koji", &[Some("unknown action: handover")]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let patience = Patience {
        start: Duration::from_millis(30),
        merge: Duration::from_millis(100),
        ..FAST
    };
    let (ran, said) = rig
        .upgrade_within(Action::Install(Source::Binary(new)), patience)
        .await;
    assert!(ran.is_err(), "the drain waits on a runner that never opens");
    assert!(
        said.iter().any(|l| l.contains(
            "`koji` did not take the handover within 30ms, so it is drained and restarted"
        )),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|l| l.contains("is handing over")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_ctrl_c_while_a_runner_hands_over_stops_the_upgrade() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[IDLE, OLD]);
    rig.shepherd.hands("koji", &[None]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let scene = Scene {
        interrupt: Interrupt::After(Duration::from_millis(100)),
        ..rig.scene(FAST)
    };
    let (ran, _) = rig
        .upgrade_in(scene, Action::Install(Source::Binary(new)))
        .await;
    let err = ran.unwrap_err();
    assert!(
        err.contains("stopped by Ctrl-C while `koji` was handing over"),
        "{err}"
    );
    assert_eq!(rig.restarts(), ["kelpie"]);
}
