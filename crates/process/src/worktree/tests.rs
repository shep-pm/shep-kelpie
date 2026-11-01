#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use super::{git, has_ref, remove, status_names};

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

// A repo with nothing to remove but the build folder
fn remove_build(repo: &Path, build: &Path) -> Result<(), super::WorktreeError> {
    git(repo, ["init", "--quiet"]).unwrap();
    remove(repo, &repo.join("none"), "kelpie/7", build, None)
}

#[cfg(unix)]
#[test]
fn a_build_folder_holding_read_only_folders_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    let cache = build.join("gomod/cache/mod");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("go.mod"), "module x\n").unwrap();
    set_mode(&cache.join("go.mod"), 0o444);
    set_mode(&cache, 0o555);
    set_mode(&build.join("gomod"), 0o555);

    remove_build(dir.path(), &build).unwrap();
    assert!(!build.exists());
}

#[test]
fn removing_a_work_item_keeps_its_claude_config_folder() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build/7");
    std::fs::create_dir_all(&build).unwrap();
    let transcript = dir.path().join("claude/7/worker/projects/x/s.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();

    remove_build(dir.path(), &build).unwrap();
    assert!(!build.exists());
    assert!(transcript.is_file());
}

#[test]
fn a_build_folder_already_gone_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    remove_build(dir.path(), &dir.path().join("build")).unwrap();
}

#[cfg(unix)]
#[test]
fn a_link_in_the_build_folder_leaves_what_it_points_at_alone() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    set_mode(&outside, 0o555);
    let build = dir.path().join("build");
    let cache = build.join("mod");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("go.mod"), "module x\n").unwrap();
    set_mode(&cache, 0o555);
    symlink(&outside, build.join("link")).unwrap();

    let removed = remove_build(dir.path(), &build);
    let mode = mode_of(&outside);
    set_mode(&outside, 0o755);
    removed.unwrap();
    assert!(!build.exists());
    assert!(outside.exists());
    assert_eq!(mode, 0o555);
}

// Sleeps for a minute when a copy of this test binary runs it with
// `SLEEPER` set, and does nothing in a normal run. System binaries cannot
// be copied and run on macOS, so the copy that stands for a test binary in
// a build folder is this one.
#[cfg(unix)]
#[test]
fn sleeper_for_the_build_removal_tests() {
    if std::env::var_os("SLEEPER").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

// A copy of this test binary in `folder`, running the sleeper after
// `shell_prefix`, as a test binary would run from a build folder.
#[cfg(unix)]
fn sleep_from(folder: &Path, shell_prefix: &str) -> std::process::Child {
    std::fs::create_dir_all(folder).unwrap();
    let copy = folder.join("sleeper");
    std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{shell_prefix}exec \"$0\" --exact worktree::tests::sleeper_for_the_build_removal_tests"
        ))
        .arg(&copy)
        .env("SLEEPER", "1")
        .spawn()
        .unwrap()
}

// Waits for the child to end, which a running `sleep 60` does not in time.
#[cfg(unix)]
fn ended_within_ten_seconds(child: &mut std::process::Child) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[cfg(unix)]
#[test]
fn removing_a_build_ends_a_process_running_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    let mut child = sleep_from(&build.join("debug/deps"), "");

    remove_build(dir.path(), &build).unwrap();
    let ended = ended_within_ten_seconds(&mut child);
    let _ = child.kill();
    assert!(ended);
    assert!(!build.exists());
}

#[cfg(unix)]
#[test]
fn removing_a_build_kills_a_process_that_ignores_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    let mut child = sleep_from(&build.join("debug/deps"), "trap '' TERM; ");

    remove_build(dir.path(), &build).unwrap();
    let ended = ended_within_ten_seconds(&mut child);
    let _ = child.kill();
    assert!(ended);
}

#[cfg(unix)]
#[test]
fn removing_a_build_leaves_other_processes_running() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    std::fs::create_dir_all(&build).unwrap();
    // A sibling whose name starts like the build folder's does.
    let mut outside = sleep_from(&dir.path().join("build-other"), "");

    remove_build(dir.path(), &build).unwrap();
    let running = outside.try_wait().unwrap().is_none();
    let _ = outside.kill();
    assert!(running);
}

#[test]
fn a_missing_ref_is_absent_but_a_folder_git_cannot_read_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(has_ref(dir.path(), "refs/heads/kelpie/7").is_err());
    git(dir.path(), ["init", "--quiet"]).unwrap();
    assert!(!has_ref(dir.path(), "refs/heads/kelpie/7").unwrap());
}

#[test]
fn a_status_names_every_changed_staged_renamed_and_new_path() {
    let status = "1 .M N... 100644 100644 100644 aaaa bbbb src/a b.rs\0\
                  1 AD N... 000000 100644 000000 0000 cccc staged.txt\0\
                  2 R. N... 100644 100644 100644 dddd eeee R100 new.rs\0old.rs\0\
                  u UU N... 100644 100644 100644 100644 ffff gggg hhhh clash.rs\0\
                  ? left.txt\0";
    assert_eq!(
        status_names(status),
        [
            "src/a b.rs",
            "staged.txt",
            "old.rs",
            "new.rs",
            "clash.rs",
            "left.txt"
        ]
    );
}
