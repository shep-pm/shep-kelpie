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
async fn a_runner_whose_status_times_out_is_waited_on_not_taken_for_idle() {
    let mut rig = Rig::new().await;
    // Three triggers delivered and unanswered, then an idle runner.
    rig.shepherd
        .replies("koji", &[None, None, None, Some(IDLE)]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();

    let writes = rig.shepherd.writes();
    let first_restart = writes
        .iter()
        .position(|w| matches!(w, Request::Restart { .. }))
        .expect("a restart");
    assert_eq!(
        looks_at(&writes[..first_restart], "koji").count(),
        4,
        "{writes:?}"
    );
    assert!(
        said.iter()
            .any(|l| l == "waiting: `koji` did not answer `status`"),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_that_never_answers_status_is_named_and_nothing_restarts() {
    let mut rig = Rig::new().await;
    rig.shepherd.replies("koji", &[None]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();
    assert!(err.contains("`koji` did not answer `status`"), "{err}");
    assert!(err.contains("nothing was restarted"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
}

#[tokio::test]
async fn a_restarted_sheep_that_never_answers_again_is_named() {
    let mut rig = Rig::new().await;
    // Two idle looks (before the dog, before the runner), then silence.
    rig.shepherd
        .replies("koji", &[Some(IDLE), Some(IDLE), None]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let patience = Patience {
        start: Duration::from_secs(1),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();
    assert!(
        err.contains("`koji` did not answer in 1s after its restart"),
        "{err}"
    );
    assert!(err.contains("shep bleats koji"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
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
        said.iter().any(|l| l.contains("`paused` is stopped")),
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
            .any(|l| l.contains("`old` is stopped, and runs /opt/kelpie/bin/kelpie")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_stopped_while_the_upgrade_waits_stays_stopped() {
    let mut rig = Rig::new().await;
    // The upgrade waits on a merge, and the maintainer stops the runner meanwhile.
    let mut looks = vec![MERGING; 30];
    looks.push(IDLE);
    rig.shepherd.says("koji", &looks);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let stopping = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.shepherd.stops("koji");
    };
    let ((ran, said), ()) = tokio::join!(rig.install(&new), stopping);
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie"]);
    assert!(!rig.shepherd.sheep("koji").unwrap().1);
    assert!(
        said.iter()
            .any(|l| l.contains("`koji` was stopped meanwhile")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_runner_still_opening_its_channel_is_waited_on() {
    let rig = Rig::new().await;
    // Plain text is what a runner answers before it takes its actions.
    rig.shepherd
        .replies("koji", &[Some("unknown action: status"), Some(IDLE)]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert!(
        said.iter().any(|l| l == "waiting: `koji` is starting"),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_failure_after_the_swap_names_what_is_installed_and_the_command_that_finishes() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[MERGING]);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();
    let installed = rig.installed.display();
    assert!(err.contains(&format!("installed at {installed}")), "{err}");
    assert!(
        err.contains(&format!("`shep kelpie upgrade --binary {installed}`")),
        "{err}"
    );
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.12.0");
    assert_eq!(rig.restarts(), Vec::<String>::new());

    // That command restarts without touching a file: the build is the one installed.
    rig.shepherd.says("koji", &[IDLE]);
    let finish = Action::Install(Source::Binary(rig.installed.clone()));
    let (ran, said) = rig.upgrade(finish).await;
    ran.unwrap();
    assert!(said[0].contains("installed already"), "{said:?}");
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert_eq!(says(&rig.previous()), "0.1.0 for shep 0.12.0");
}
