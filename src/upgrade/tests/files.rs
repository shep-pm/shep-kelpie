//! The files an upgrade moves: the installed kelpie and the build kept beside it

use std::os::unix::fs::MetadataExt;

use super::*;

#[tokio::test]
async fn an_upgrade_swaps_the_build_in_at_the_path_the_dog_runs() {
    let mut rig = Rig::new().await;
    let inode = std::fs::metadata(&rig.installed).unwrap().ino();
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.12.0");
    assert_ne!(
        std::fs::metadata(&rig.installed).unwrap().ino(),
        inode,
        "the running file's bytes were not edited in place"
    );
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(
        said.iter().any(|l| l.contains("restarted `koji`")),
        "{said:?}"
    );
    assert!(
        !rig.home.join("bin").exists(),
        "nothing is installed under kelpie's home"
    );
}

#[tokio::test]
async fn the_build_an_upgrade_replaces_is_copied_into_builds_as_the_previous() {
    let rig = Rig::new().await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert_eq!(says(&rig.previous()), "0.1.0 for shep 0.12.0");
    assert!(rig.previous().starts_with(rig.home.join("builds")));
    assert!(said[0].contains("kelpie 0.1.0"), "{said:?}");
    assert!(said[0].contains("--rollback"), "{said:?}");
}

#[tokio::test]
async fn running_the_same_build_again_keeps_the_real_previous_build() {
    let rig = Rig::new().await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();
    let (ran, said) = rig.install(&new).await;
    ran.unwrap();
    assert!(said[0].contains("installed already"), "{said:?}");
    assert_eq!(says(&rig.previous()), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn installing_the_same_build_again_leaves_a_rollback_going_back_to_the_first() {
    let rig = Rig::new().await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();
    rig.install(&new).await.0.unwrap();
    rig.upgrade(Action::Rollback).await.0.unwrap();
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn an_installed_symlink_stays_a_link_and_the_copy_kept_is_its_target() {
    let rig = Rig::on(crate::shepherd::SHEP_VERSION, true).await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();
    assert!(
        std::fs::symlink_metadata(&rig.installed)
            .unwrap()
            .is_symlink(),
        "the link is still a link"
    );
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.12.0");
    let kept = std::fs::symlink_metadata(rig.previous()).unwrap();
    assert!(kept.is_file(), "the copy is a file, not a link");
    assert_eq!(says(&rig.previous()), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_rollback_restores_the_previous_build_and_restarts_onto_it() {
    let mut rig = Rig::new().await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();
    rig.restarts();

    let (ran, said) = rig.upgrade(Action::Rollback).await;
    ran.unwrap();
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
    assert_eq!(rig.restarts(), ["kelpie", "koji"]);
    assert!(said.iter().any(|l| l.contains("kelpie 0.1.0")), "{said:?}");

    // A second rollback puts the upgrade back.
    rig.upgrade(Action::Rollback).await.0.unwrap();
    assert_eq!(rig.installed_says(), "0.3.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_rollback_through_a_symlink_puts_the_build_back_in_the_target() {
    let rig = Rig::on(crate::shepherd::SHEP_VERSION, true).await;
    let new = rig.build("new", "0.3.0", "0.12.0");
    rig.install(&new).await.0.unwrap();
    rig.upgrade(Action::Rollback).await.0.unwrap();
    assert!(
        std::fs::symlink_metadata(&rig.installed)
            .unwrap()
            .is_symlink()
    );
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_rollback_with_no_previous_build_changes_nothing() {
    let mut rig = Rig::new().await;
    let err = rig.upgrade(Action::Rollback).await.0.unwrap_err();
    assert!(err.contains("no previous build"), "{err}");
    assert_eq!(rig.restarts(), Vec::<String>::new());
    assert_eq!(rig.installed_says(), "0.1.0 for shep 0.12.0");
}
