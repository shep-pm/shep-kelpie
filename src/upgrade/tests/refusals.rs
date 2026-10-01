//! What an upgrade refuses, and that it refuses before it changes anything

use std::os::unix::fs::PermissionsExt;

use super::*;

#[tokio::test]
async fn a_build_for_another_shep_minor_stops_before_anything_changes() {
    let mut rig = Rig::new().await;
    let new = rig.build("new", "0.4.0", "0.13.0");

    let (ran, said) = rig.install(&new).await;
    let err = ran.unwrap_err();
    assert!(err.contains("shep 0.13.0"), "{err}");
    assert!(err.contains("runs shep 0.12.0"), "{err}");
    assert!(err.contains("Nothing was installed or restarted"), "{err}");
    assert!(
        err.contains("1. upgrade shep and reload its shepherd"),
        "a newer build says to move the shepherd first: {err}"
    );
    assert!(
        err.contains(&format!(
            "2. run the new build's own upgrade: `{0} upgrade --binary {0}`",
            new.display()
        )),
        "{err}"
    );
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
    assert!(!rig.previous().exists());
    let beside: Vec<_> = std::fs::read_dir(rig.installed.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(beside, ["shep-kelpie"], "no staged copy is left behind");
}

#[tokio::test]
async fn a_build_for_an_older_minor_than_the_shepherd_says_to_install_another_kelpie() {
    let rig = Rig::on("0.13.1", false).await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    let err = rig.install(&new).await.0.unwrap_err();
    assert!(err.contains("install a kelpie built for"), "{err}");
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_newer_patch_of_the_same_minor_is_taken() {
    let rig = Rig::on("0.12.3", false).await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_rollback_to_a_build_the_shepherd_would_refuse_stops_there() {
    // The shepherd moved to 0.13 before the upgrade to a 0.13 build, so the
    // build kept as the previous is made for the shep line it left.
    let mut rig = Rig::on("0.13.0", false).await;
    let new = rig.build("new", "0.4.0", "0.13.0");
    rig.install(&new).await.0.unwrap();
    rig.restarts();

    let err = rig.upgrade(Action::Rollback).await.0.unwrap_err();
    assert!(err.contains("shep 0.12.0"), "{err}");
    assert!(err.contains("runs shep 0.13.0"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert_eq!(rig.installed_says(), "0.4.0 for shep 0.13.0");
}

#[tokio::test]
async fn a_binary_that_does_not_say_its_versions_is_refused_and_not_installed() {
    let rig = Rig::new().await;
    let liar = rig.builds.join("liar");
    write_script(&liar, "#!/bin/sh\necho hello\n");
    let err = rig.install(&liar).await.0.unwrap_err();
    assert!(err.contains("did not answer `version --json`"), "{err}");
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");

    let err = rig
        .install(&rig.builds.join("missing"))
        .await
        .0
        .unwrap_err();
    assert!(err.contains("is not a file"), "{err}");
}

#[tokio::test]
async fn a_binary_that_exits_non_zero_is_refused_and_not_installed() {
    let rig = Rig::new().await;
    let sick = rig.builds.join("sick");
    write_script(&sick, &format!("{}exit 3\n", answering("0.3.0", "0.12.0")));
    let err = rig.install(&sick).await.0.unwrap_err();
    assert!(err.contains("exited"), "{err}");
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_binary_without_its_executable_bits_is_refused_naming_chmod() {
    let mut rig = Rig::new().await;
    let flat = rig.build("flat", "0.3.0", "0.12.0");
    std::fs::set_permissions(&flat, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = rig.install(&flat).await.0.unwrap_err();
    assert!(err.contains("not executable"), "{err}");
    assert!(
        err.contains(&format!("chmod +x {}", flat.display())),
        "{err}"
    );
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_sheep_that_runs_another_program_stops_the_upgrade_before_anything_changes() {
    let mut rig = Rig::new().await;
    rig.runs("old", Path::new("/opt/kelpie/bin/kelpie"), true);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let err = rig.install(&new).await.0.unwrap_err();
    assert!(
        err.contains("`old` runs /opt/kelpie/bin/kelpie"),
        "the sheep and its path are named: {err}"
    );
    assert!(
        err.contains(&rig.installed.display().to_string()),
        "the dog's path is named: {err}"
    );
    assert!(err.contains("Nothing was installed or restarted"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
    assert!(!rig.previous().exists());
}

#[tokio::test]
async fn a_flock_with_no_adopted_kelpie_has_nothing_to_upgrade() {
    let shepherd = FakeShepherd::new().await;
    let home = shepherd.scratch("kelpie");
    let scene = Scene {
        kelpie_home: &home,
        shep_home: shepherd.home(),
        repo: "/nowhere",
        patience: FAST,
    };
    let err = run(&scene, &Action::Rollback, &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.contains("no adopted kelpie"), "{err}");
}

#[tokio::test]
async fn no_shepherd_means_no_upgrade() {
    let home = tempfile::tempdir().unwrap();
    let scene = Scene {
        kelpie_home: home.path(),
        shep_home: Path::new("/nowhere/shep"),
        repo: "/nowhere",
        patience: FAST,
    };
    let err = run(&scene, &Action::Rollback, &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.contains("cannot reach kelpie's shepherd"), "{err}");
}

#[tokio::test]
async fn a_second_upgrade_at_once_is_refused_and_the_first_finishes() {
    let mut rig = Rig::new().await;
    // The first upgrade waits out a merge long enough for the second to start.
    let mut looks = vec![MERGING; 40];
    looks.push(IDLE);
    rig.shepherd.says("koji", &looks);
    let new = rig.build("new", "0.3.0", "0.12.0");
    let other = rig.build("other", "0.4.0", "0.12.0");

    let first = rig.install(&new);
    let second = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.install(&other).await
    };
    let ((first, _), (second, _)) = tokio::join!(first, second);
    first.unwrap();
    let err = second.unwrap_err();
    assert!(err.contains("another upgrade is running"), "{err}");
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.12.0");
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    // The lock goes with the upgrade that held it.
    rig.install(&other).await.0.unwrap();
}
