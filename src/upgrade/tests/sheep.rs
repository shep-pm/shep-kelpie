//! The sheep an upgrade restarts, in what order, and when

use super::*;

fn restarted(writes: &[Request]) -> Vec<&str> {
    writes
        .iter()
        .filter_map(|w| match w {
            Request::Restart {
                selector: SelectorSpec::Name(name),
            } => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

fn looks_at<'a>(writes: &'a [Request], sheep: &'a str) -> impl Iterator<Item = &'a Request> {
    writes.iter().filter(move |w| {
        matches!(w, Request::Trigger { selector: SelectorSpec::Name(n), action, .. }
            if n == sheep && action == "status")
    })
}

#[tokio::test]
async fn an_upgrade_waits_out_a_merge_in_flight_before_any_restart() {
    let rig = Rig::new().await;
    // Two looks at a merge under way, then it is over.
    rig.shepherd.says("koji", &[MERGING, MERGING, IDLE]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let mut rig = rig;
    let writes = rig.shepherd.writes();
    let first_restart = writes
        .iter()
        .position(|w| matches!(w, Request::Restart { .. }))
        .expect("a restart");
    assert_eq!(
        looks_at(&writes[..first_restart], "koji").count(),
        3,
        "the runner was looked at until its merge ended: {writes:?}"
    );
    assert!(
        said.iter().any(|l| l == "waiting: `koji` is merging #7"),
        "{said:?}"
    );
    assert!(
        said.iter().any(|l| l.contains("restarted `koji`")),
        "{said:?}"
    );
}

#[tokio::test]
async fn the_dog_goes_down_only_when_no_runner_is_merging() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // `koji` is free and `aria` merging for a while.
    rig.shepherd.says("aria", &[MERGING, MERGING, IDLE]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();

    let writes = rig.shepherd.writes();
    assert_eq!(restarted(&writes), ["kelpie", "aria", "koji"]);
    let dog = writes
        .iter()
        .position(|w| matches!(w, Request::Restart { .. }))
        .unwrap();
    assert_eq!(looks_at(&writes[..dog], "aria").count(), 3, "{writes:?}");
}

#[tokio::test]
async fn an_upgrade_gives_up_on_a_merge_that_does_not_end_and_restarts_nothing() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[MERGING]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();
    assert!(err.contains("`koji` is merging #7"), "{err}");
    assert!(err.contains("nothing was restarted"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
}

#[tokio::test]
async fn a_sheep_that_is_stopped_stays_stopped() {
    let mut rig = Rig::new().await;
    rig.runs("paused", &rig.installed.clone(), false);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(!rig.shepherd.sheep("paused").unwrap().1);
    assert!(
        said.iter().any(|l| l.contains("`paused` is not running")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_stopped_sheep_on_another_program_is_said_to_stay_behind() {
    let rig = Rig::new().await;
    rig.runs("old", Path::new("/opt/kelpie/bin/kelpie"), false);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert!(
        said.iter()
            .any(|l| l.contains("`old` is not running, and runs /opt/kelpie/bin/kelpie")),
        "{said:?}"
    );
}
