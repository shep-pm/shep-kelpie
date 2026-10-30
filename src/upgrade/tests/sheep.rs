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
    let new = rig.build("new", "0.3.0", "0.19.0");
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
// The upgrade probes stand-in builds as real child processes.
async fn the_wait_names_why_a_merged_item_has_not_cleaned_up() {
    let mut rig = Rig::new().await;
    let failing = r#"{"work_items":[{"issue":"7","phase":{"state":"done","merged":true},
        "failing":{"failures":8,"since":1790000000,
                   "reason":"cannot delete branch kelpie/7: checked out in another worktree"}}]}"#;
    rig.shepherd.says("koji", &[failing]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let action = Action::Install(Source::Binary(new));
    let (ran, said) = rig.upgrade_within(action, patience).await;
    let err = ran.unwrap_err();

    assert!(
        said.iter().any(|line| line
            == "waiting: `koji` is merging #7, whose cleanup has failed \
            8 times: cannot delete branch kelpie/7: checked out in another worktree"),
        "{said:?}"
    );
    assert!(
        err.contains(
            "`koji` is merging #7, whose cleanup has failed 8 times: \
            cannot delete branch kelpie/7: checked out in another worktree"
        ),
        "{err}"
    );
    assert!(err.contains("nothing was restarted"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
}

#[tokio::test]
async fn the_dog_goes_down_only_when_no_runner_is_merging() {
    let mut rig = Rig::new().await;
    rig.runs("aria", &rig.installed.clone(), true);
    // `koji` is free and `aria` merging for a while.
    rig.shepherd.says("aria", &[MERGING, MERGING, IDLE]);
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    let new = rig.build("new", "0.3.0", "0.19.0");
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

// `aria` sorts before `koji`, so it is restarted first and `koji` follows it.
// Its scripted looks: before the dog, the drain, the merge wait, then the restart's.
fn slow_aria(rig: &Rig, after_restart: &[Option<&str>]) {
    rig.runs("aria", &rig.installed.clone(), true);
    let mut looks = vec![Some(IDLE); 3];
    looks.extend_from_slice(after_restart);
    rig.shepherd.replies("aria", &looks);
}

// Two looks at a restarted sheep: the first sees it still opening its channel.
const TWO_LOOKS: Patience = Patience {
    start: FAST.poll,
    ..FAST
};

#[tokio::test]
async fn a_runner_slow_to_answer_after_its_restart_does_not_hold_back_the_runners_after_it() {
    let mut rig = Rig::new().await;
    slow_aria(&rig, &[None, Some(IDLE)]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let action = Action::Install(Source::Binary(new));
    let (ran, said) = rig.upgrade_within(action, TWO_LOOKS).await;
    ran.unwrap();

    assert_eq!(rig.restarts(), ["kelpie", "aria", "koji"]);
    let told = |what: &str| said.iter().position(|l| l.contains(what));
    assert!(
        told("restarted `koji`") < told("restarted `aria`"),
        "{said:?}"
    );
    assert!(told("restarted `aria`").is_some(), "{said:?}");
}

#[tokio::test]
async fn a_runner_that_never_answers_after_its_restart_is_named_once_the_others_are_restarted() {
    let mut rig = Rig::new().await;
    slow_aria(&rig, &[None]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, TWO_LOOKS).await.0.unwrap_err();

    assert!(
        err.contains("`aria` never answered after its restart"),
        "{err}"
    );
    assert!(err.contains("shep bleats aria"), "{err}");
    assert!(!err.contains("koji"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie", "aria", "koji"]);
}

#[tokio::test]
async fn a_failure_after_a_slow_runner_still_names_the_slow_runner() {
    let mut rig = Rig::new().await;
    slow_aria(&rig, &[None]);
    // `koji` is idle at the dog's look and its drain, then merges for good.
    rig.shepherd.says("koji", &[IDLE, IDLE, MERGING]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..TWO_LOOKS
    };
    let action = Action::Install(Source::Binary(new));
    let err = rig.upgrade_within(action, patience).await.0.unwrap_err();

    assert!(err.contains("`koji` is merging #7"), "{err}");
    assert!(
        err.contains("Not asked again: `aria` did not answer"),
        "{err}"
    );
    assert!(err.contains("shep bleats aria"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie", "aria"]);
}

#[tokio::test]
async fn a_slow_runner_that_stopped_before_it_was_asked_again_is_named() {
    let mut rig = Rig::new().await;
    slow_aria(&rig, &[None, Some(IDLE)]);
    let new = rig.build("new", "0.3.0", "0.19.0");
    let scene = rig.scene(TWO_LOOKS);
    let action = Action::Install(Source::Binary(new));
    let mut said = Vec::new();
    let ran = run(&scene, &action, &mut |line| {
        // The slow runner goes down once the one after it is up.
        if line.contains("restarted `koji`") {
            rig.shepherd.stops("aria");
        }
        said.push(line);
    })
    .await;
    let err = ran.unwrap_err();

    assert!(
        err.contains("`aria` is not running after its restart"),
        "{err}"
    );
    assert!(err.contains("shep bleats aria"), "{err}");
    assert_eq!(rig.restarts(), ["kelpie", "aria", "koji"]);
}

#[tokio::test]
async fn a_sheep_that_is_stopped_stays_stopped() {
    let mut rig = Rig::new().await;
    rig.runs("paused", &rig.installed.clone(), false);
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    let new = rig.build("new", "0.3.0", "0.19.0");
    // Stopped once it has been looked at a few times, not after a delay: the upgrade
    // checks the new build first, and how long that takes depends on the machine.
    let stopping = async {
        while rig.shepherd.answers_left("koji") > looks.len() - 5 {
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
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    let new = rig.build("new", "0.3.0", "0.19.0");
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
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.19.0");
    assert_eq!(rig.restarts(), Vec::<String>::new());

    // That command restarts without touching a file: the build is the one installed.
    rig.shepherd.says("koji", &[IDLE]);
    let finish = Action::Install(Source::Binary(rig.installed.clone()));
    let (ran, said) = rig.upgrade(finish).await;
    ran.unwrap();
    assert!(said[0].contains("installed already"), "{said:?}");
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert_eq!(says(&rig.previous()), "0.1.0 for shep 0.19.0");
}

#[tokio::test]
async fn a_failed_rollback_names_the_rolled_back_build_as_installed() {
    let mut rig = Rig::new().await;
    let new = rig.build("new", "0.3.0", "0.19.0");
    rig.install(&new).await.0.unwrap();
    rig.restarts();

    rig.shepherd.says("koji", &[MERGING]);
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let err = rig
        .upgrade_within(Action::Rollback, patience)
        .await
        .0
        .unwrap_err();
    let installed = rig.installed.display();
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.19.0");
    assert!(
        err.contains(&format!(
            "The rolled back build is installed at {installed}"
        )),
        "{err}"
    );
    assert!(!err.contains("The new build"), "{err}");
    assert!(
        err.contains(&format!("`shep kelpie upgrade --binary {installed}`")),
        "{err}"
    );
    assert!(
        err.contains("`--rollback` would swap the builds forward again"),
        "{err}"
    );
}

#[tokio::test]
async fn a_bare_upgrade_restarts_onto_the_installed_build() {
    let mut rig = Rig::new().await;
    // What `cargo install` leaves: the new build already at the installed path.
    rig.cargo_installs("0.3.0", "0.19.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.19.0");
    assert!(
        said.iter().any(|l| l.contains("restarting onto it")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|l| l.contains("restarted `koji`")),
        "{said:?}"
    );
    assert!(
        !rig.previous().exists(),
        "a bare upgrade stages and swaps nothing"
    );
}

#[tokio::test]
async fn a_bare_upgrade_with_nothing_changed_restarts_nothing_and_says_so() {
    let mut rig = Rig::new().await;
    // An upgrade leaves the installed file as the one restarted onto.
    let new = rig.build("new", "0.3.0", "0.19.0");
    rig.install(&new).await.0.unwrap();
    rig.restarts();

    let (ran, said) = rig.upgrade(Action::Installed).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert!(
        said.iter()
            .any(|l| l.contains("nothing changed since the last upgrade")),
        "{said:?}"
    );

    // A bare run records too: the next one finds the file it restarted onto.
    rig.cargo_installs("0.10.0", "0.19.0");
    rig.upgrade(Action::Installed).await.0.unwrap();
    rig.restarts();
    let (ran, said) = rig.upgrade(Action::Installed).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert!(
        said.iter()
            .any(|l| l.contains("nothing changed since the last upgrade")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_bare_upgrade_after_the_file_changed_says_rollback_has_no_copy() {
    let mut rig = Rig::new().await;
    let new = rig.build("new", "0.3.0", "0.19.0");
    rig.install(&new).await.0.unwrap();
    rig.restarts();

    // `cargo install` replaces the file the upgrade left, with no copy kept.
    rig.cargo_installs("0.10.0", "0.19.0");
    let (ran, said) = rig.upgrade(Action::Installed).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert_eq!(rig.installed_says(), "0.10.0 for shep 0.19.0");
    assert!(
        said.iter().any(|l| l.contains("`--rollback` has no copy")
            && l.contains("the previous build kept, kelpie 0.1.0, is the one before it")
            && l.contains("`--ref`, `--release` and `--binary` keep one")),
        "{said:?}"
    );
    assert_eq!(
        says(&rig.previous()),
        "0.1.0 for shep 0.19.0",
        "the kept previous build is the one before cargo's"
    );
}

#[tokio::test]
async fn a_bare_upgrade_with_no_record_counts_as_changed() {
    let mut rig = Rig::new().await;
    assert!(!rig.home.join("builds/installed.seen").exists());

    let (ran, said) = rig.upgrade(Action::Installed).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(
        said.iter().any(|l| l.contains("`--rollback` has no copy")
            && l.contains("no earlier build is kept either")),
        "{said:?}"
    );
    assert!(rig.home.join("builds/installed.seen").exists());
}

#[tokio::test]
async fn a_bare_upgrade_that_stops_in_its_restarts_does_not_offer_rollback_as_an_undo() {
    let mut rig = Rig::new().await;
    rig.shepherd.says("koji", &[MERGING]);
    rig.cargo_installs("0.3.0", "0.19.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let err = rig
        .upgrade_within(Action::Installed, patience)
        .await
        .0
        .unwrap_err();
    assert!(err.contains("To finish the restarts"), "{err}");
    assert!(!err.contains("--rollback"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
}

#[tokio::test]
async fn a_file_replaced_while_a_bare_upgrade_waits_is_not_recorded_as_restarted_onto() {
    let mut rig = Rig::new().await;
    // The restart waits on a merge, and `cargo install` runs meanwhile.
    let mut looks = vec![MERGING; 30];
    looks.push(IDLE);
    rig.shepherd.says("koji", &looks);
    let installing = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.cargo_installs("0.10.0", "0.19.0");
    };
    let ((ran, _), ()) = tokio::join!(rig.upgrade(Action::Installed), installing);
    ran.unwrap();
    rig.restarts();

    let (ran, said) = rig.upgrade(Action::Installed).await;
    ran.unwrap();
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(said.iter().any(|l| l.contains("kelpie 0.10.0")), "{said:?}");
}

#[tokio::test]
async fn a_build_that_does_not_fit_the_shepherd_stops_a_bare_upgrade_before_any_restart() {
    let mut rig = Rig::new().await;
    rig.cargo_installs("0.4.0", "0.20.0");

    let (ran, said) = rig.upgrade(Action::Installed).await;
    let err = ran.unwrap_err();
    assert!(err.contains("shep 0.20.0"), "{err}");
    assert!(err.contains("Nothing was installed or restarted"), "{err}");
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert!(!rig.home.join("builds/installed.seen").exists());
}
