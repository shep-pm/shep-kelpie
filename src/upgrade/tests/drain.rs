//! Draining each runner before its restart, and `--now`, which does not

use super::*;
use crate::upgrade::drain::{Drained, drained};

// A runner draining with #7's worker turn still running, whose calls are
// ended within the ceiling it gives
const RUNNING_7: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"}}],
    "draining":{"calls":[{"issue":7,"role":"worker"}],"ceiling":0}}"#;

// A runner draining with no call left
const DRAINED: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"}}],
    "draining":{"calls":[],"ceiling":3600}}"#;

// The triggers sent to `sheep` since the last look, by action
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
async fn a_runner_is_drained_and_restarted_once_no_call_runs() {
    let mut rig = Rig::new().await;
    // The dog's look, then two drains with the turn running, then none.
    rig.shepherd
        .says("koji", &[IDLE, RUNNING_7, RUNNING_7, DRAINED]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "koji"]);
    let koji = writes
        .iter()
        .position(
            |w| matches!(w, Request::Restart { selector: SelectorSpec::Name(n) } if n == "koji"),
        )
        .unwrap();
    assert_eq!(
        triggers(&writes[..koji], "koji"),
        ["status", "drain", "drain", "drain", "status"],
        "drained until no call ran, then the merge wait: {writes:?}"
    );
    assert!(
        !triggers(&writes, "kelpie").contains(&"drain".to_owned()),
        "the dog is not drained"
    );
    assert!(!triggers(&writes, "koji").contains(&"undrain".to_owned()));
    assert!(
        said.iter()
            .any(|l| l == "waiting: `koji` is running #7's worker turn"),
        "{said:?}"
    );
    let undo = "draining `koji`, which starts no new call until its restart: if this upgrade \
                stops first, `shep kelpie undrain -p koji` lets it start them again";
    assert_eq!(said.iter().filter(|l| *l == undo).count(), 1, "{said:?}");
}

#[tokio::test]
async fn a_ctrl_c_while_a_runner_drains_undrains_it() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[IDLE, RUNNING_7]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let scene = Scene {
        interrupt: Interrupt::After(Duration::from_millis(100)),
        ..rig.scene(FAST)
    };
    let (ran, _) = rig
        .upgrade_in(scene, Action::Install(Source::Binary(new)))
        .await;
    let err = ran.unwrap_err();
    assert!(
        err.contains("stopped by Ctrl-C while `koji` was draining"),
        "{err}"
    );
    assert!(err.contains("it was sent `undrain`"), "{err}");
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie"]);
    assert_eq!(triggers(&writes, "koji").last().unwrap(), "undrain");
}

#[tokio::test]
async fn a_runner_that_answers_drain_on_its_second_ask_is_drained() {
    let mut rig = Rig::new().await;
    // Still opening at the first `drain`, then answering both actions.
    rig.shepherd.replies(
        "koji",
        &[
            Some(IDLE),
            Some("unknown action: drain"),
            Some(IDLE),
            Some(DRAINED),
        ],
    );
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "koji"]);
    let koji = writes
        .iter()
        .position(
            |w| matches!(w, Request::Restart { selector: SelectorSpec::Name(n) } if n == "koji"),
        )
        .unwrap();
    assert_eq!(
        triggers(&writes[..koji], "koji"),
        ["status", "drain", "status", "drain", "status"]
    );
    assert!(
        !said.iter().any(|l| l.contains("from before `drain`")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|l| l.starts_with("draining `koji`")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_call_that_outlasts_its_ceiling_leaves_the_runner_undrained_on_the_old_build() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[IDLE, RUNNING_7]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let patience = Patience {
        margin: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();
    assert!(
        err.contains("`koji` is running #7's worker turn after 0s"),
        "{err}"
    );
    assert!(err.contains("nothing was restarted for it"), "{err}");
    assert!(err.contains("it was sent `undrain`"), "{err}");
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie"]);
    assert_eq!(triggers(&writes, "koji").last().unwrap(), "undrain");
}

#[tokio::test]
async fn a_merge_that_outlasts_its_wait_after_the_drain_undrains_the_runner() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[IDLE, DRAINED, MERGING]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();
    assert!(err.contains("`koji` is merging #7"), "{err}");
    assert!(err.contains("it was sent `undrain`"), "{err}");
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie"]);
    assert_eq!(triggers(&writes, "koji").last().unwrap(), "undrain");
}

#[tokio::test]
async fn now_restarts_without_draining() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[IDLE, RUNNING_7]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let scene = Scene {
        now: true,
        ..rig.scene(FAST)
    };
    let (ran, _) = rig
        .upgrade_in(scene, Action::Install(Source::Binary(new)))
        .await;
    ran.unwrap();
    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "koji"]);
    assert!(
        !triggers(&writes, "koji").contains(&"drain".to_owned()),
        "{writes:?}"
    );
}

#[tokio::test]
async fn a_runner_from_before_drain_is_restarted_as_before() {
    let mut rig = Rig::new().await;
    // An old runner answers `drain` as an action nobody took, twice, and `status` as ever.
    let old = Some("unknown action: drain");
    rig.shepherd
        .replies("koji", &[Some(IDLE), old, Some(IDLE), old, Some(IDLE)]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(
        said.iter().any(|l| l
            == "`koji` runs a kelpie from before `drain`, so its restart cuts short any call \
                it has running"),
        "{said:?}"
    );
}

#[test]
fn now_is_read_first_or_last_and_once() {
    let args = |a: &[&str]| a.iter().map(|&s| s.to_owned()).collect::<Vec<_>>();
    let rest = |a: &[&str]| {
        let a = args(a);
        let (now, rest) = now(&a);
        (now, parse(rest))
    };
    let main = Ok(Action::Install(Source::Ref("main".into())));
    assert_eq!(rest(&["--now", "--ref", "main"]), (true, main.clone()));
    assert_eq!(rest(&["--ref", "main", "--now"]), (true, main.clone()));
    assert_eq!(rest(&["--ref", "main"]), (false, main));
    assert_eq!(rest(&["--rollback", "--now"]), (true, Ok(Action::Rollback)));
    for bad in [
        &["--now"][..],
        &["--now", "--rollback", "--now"],
        &["--ref", "--now", "main"],
    ] {
        assert_eq!(rest(bad).1, Err(USAGE.to_owned()), "{bad:?}");
    }
}

#[test]
fn a_draining_answer_names_each_call() {
    let body = r#"{"draining":{"calls":[{"issue":7,"role":"worker"},
        {"issue":8,"role":"reviewer"},{"role":"pm"}],"ceiling":3600}}"#;
    assert_eq!(
        drained(body),
        Some(Drained {
            calls: vec![
                "#7's worker turn".into(),
                "#8's review".into(),
                "the project manager's wake".into(),
            ],
            ceiling: 3600,
        })
    );
    for not_draining in [IDLE, r#"{"error":"unknown action `drain`"}"#, "", "[]"] {
        assert_eq!(drained(not_draining), None, "{not_draining}");
    }
}
