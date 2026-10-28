use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

// A build folder, and beside it a folder that is none of the worker's
fn folders() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (build, theirs) = (dir.path().join("build"), dir.path().join("theirs"));
    fs::create_dir(&build).unwrap();
    fs::create_dir(&theirs).unwrap();
    fs::write(theirs.join("hosts.yml"), "theirs\n").unwrap();
    (dir, build, theirs)
}

// A junction needs no privilege, and is a reparse point as a symlink is.
fn junction(at: &Path, target: &Path) {
    let made = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(at)
        .arg(target)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
}

#[test]
fn a_write_then_a_read_round_trips_and_a_missing_file_is_none() {
    let (_dir, build, _) = folders();
    let file = Path::new("gh/deep/a.md");

    write(&build, file, "first\n").unwrap();
    write(&build, file, "second\n").unwrap();

    assert_eq!(read(&build, file), Ok(Some("second\n".into())));
    assert_eq!(read(&build, Path::new("gh/b.md")), Ok(None));
    assert_eq!(read(&build, Path::new("gone/b.md")), Ok(None));
    assert_eq!(read(&build.join("gone"), Path::new("b.md")), Ok(None));
    assert!(!build.join("gh/deep/a.md.tmp").exists());
}

#[test]
fn a_folder_that_is_a_junction_is_not_followed_to_read_or_write() {
    let (_dir, build, theirs) = folders();
    junction(&build.join("gh"), &theirs);
    let file = Path::new("gh/hosts.yml");

    assert_eq!(read(&build, file), Err(Error::NotFolder));
    assert_eq!(write(&build, file, "mine\n"), Err(Error::NotFolder));
    assert_eq!(write_private(&build, file, "mine\n"), Err(Error::NotFolder));

    assert_eq!(
        fs::read_to_string(theirs.join("hosts.yml")).unwrap(),
        "theirs\n"
    );
    assert_eq!(fs::read_dir(&theirs).unwrap().count(), 1);
}

// Windows makes a symlink only with Developer Mode or as an administrator (#1271).
#[test]
fn a_folder_that_is_a_symlink_is_not_followed_to_read_or_write() {
    let (_dir, build, theirs) = folders();
    std::os::windows::fs::symlink_dir(&theirs, build.join("gh")).unwrap();
    let file = Path::new("gh/hosts.yml");

    assert_eq!(read(&build, file), Err(Error::NotFolder));
    assert_eq!(write(&build, file, "mine\n"), Err(Error::NotFolder));
    assert_eq!(
        fs::read_to_string(theirs.join("hosts.yml")).unwrap(),
        "theirs\n"
    );
}

#[test]
fn a_file_that_is_a_symlink_is_refused_without_its_content() {
    let (_dir, build, theirs) = folders();
    std::os::windows::fs::symlink_file(theirs.join("hosts.yml"), build.join("a.md")).unwrap();

    let refused = read(&build, Path::new("a.md"));

    assert_eq!(refused, Err(Error::NotRegular));
    assert!(!refused.unwrap_err().to_string().contains("theirs"));
}

#[test]
fn a_write_replaces_a_symlink_and_leaves_what_it_names_alone() {
    let (_dir, build, theirs) = folders();
    std::os::windows::fs::symlink_file(theirs.join("hosts.yml"), build.join("a.md")).unwrap();

    write(&build, Path::new("a.md"), "mine\n").unwrap();

    assert_eq!(
        fs::read_to_string(theirs.join("hosts.yml")).unwrap(),
        "theirs\n"
    );
    assert!(
        !fs::symlink_metadata(build.join("a.md"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(build.join("a.md")).unwrap(), "mine\n");
}

#[test]
fn a_write_replaces_a_hard_link_and_leaves_what_it_names_alone() {
    let (_dir, build, theirs) = folders();
    fs::hard_link(theirs.join("hosts.yml"), build.join("a.md")).unwrap();

    write(&build, Path::new("a.md"), "mine\n").unwrap();

    assert_eq!(
        fs::read_to_string(theirs.join("hosts.yml")).unwrap(),
        "theirs\n"
    );
    assert_eq!(fs::read_to_string(build.join("a.md")).unwrap(), "mine\n");
}

#[test]
fn a_staged_file_a_crash_left_does_not_stop_the_next_write() {
    let (_dir, build, _) = folders();
    fs::write(build.join("a.md.tmp"), "half").unwrap();

    write(&build, Path::new("a.md"), "whole\n").unwrap();

    assert_eq!(fs::read_to_string(build.join("a.md")).unwrap(), "whole\n");
}

#[test]
fn a_folder_is_refused_as_a_file_and_a_file_as_a_folder() {
    let (_dir, build, _) = folders();
    fs::create_dir(build.join("a.md")).unwrap();
    fs::write(build.join("b"), "x").unwrap();

    assert_eq!(read(&build, Path::new("a.md")), Err(Error::NotRegular));
    assert_eq!(read(&build, Path::new("b/c.md")), Err(Error::NotFolder));
    assert_eq!(
        write(&build, Path::new("b/c.md"), "x"),
        Err(Error::NotFolder)
    );
}

#[test]
fn a_file_over_the_cap_is_refused_and_one_at_it_is_read() {
    let (_dir, build, _) = folders();
    let at = "x".repeat(usize::try_from(MAX).unwrap());
    fs::write(build.join("at.md"), &at).unwrap();
    fs::write(build.join("over.md"), format!("{at}x")).unwrap();

    assert_eq!(read(&build, Path::new("at.md")), Ok(Some(at)));
    assert_eq!(read(&build, Path::new("over.md")), Err(Error::TooBig));
}

#[test]
fn a_path_that_climbs_out_of_the_folder_is_refused() {
    let (_dir, build, _) = folders();

    assert!(read(&build, Path::new("../theirs/hosts.yml")).is_err());
    assert!(write(&build, Path::new("C:\\x"), "").is_err());
    assert!(write(&build, Path::new(""), "").is_err());
}

#[test]
fn a_held_walk_refuses_a_junction_and_makes_its_folders_and_files_in_place() {
    let (_dir, build, theirs) = folders();
    junction(&build.join("out"), &theirs);
    let mut held = Held::open(&build).unwrap();

    assert_eq!(held.enter(OsStr::new("out")), Err(Error::NotFolder));
    assert_eq!(
        held.make_and_enter(OsStr::new("out")),
        Err(Error::NotFolder)
    );
    assert_eq!(held.enter(OsStr::new("gone")), Ok(false));
    held.make_and_enter(OsStr::new("in")).unwrap();
    drop(held.create(OsStr::new("a.txt")).unwrap());

    assert!(build.join("in/a.txt").exists());
    assert_eq!(fs::read_dir(&theirs).unwrap().count(), 1);
}
