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

// What the sleeper prints once it is running.
#[cfg(unix)]
const SLEEPER_STARTED: &str = "sleeper started";

#[cfg(unix)]
const SLEEPER_TEST: &str = "worktree::tests::sleeper_for_the_build_removal_tests";

// Says it started and sleeps for a minute when a copy of this test binary runs
// it with `SLEEPER` set, and does nothing in a normal run. System binaries
// cannot be copied and run on macOS, so the copy is of this one.
#[cfg(unix)]
#[test]
fn sleeper_for_the_build_removal_tests() {
    if std::env::var_os("SLEEPER").is_some() {
        println!("{SLEEPER_STARTED}");
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
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{shell_prefix}exec \"$0\" --exact --nocapture {SLEEPER_TEST}"
        ))
        .arg(&copy)
        .env("SLEEPER", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    started(child)
}

// The sleeper run straight from `program`, with no shell to start it: from
// `dir` under the name `./sleeper` when `relative`, and with `argv0` for its
// first argument when there is one.
#[cfg(unix)]
fn sleep_as(program: &Path, relative: bool, argv0: Option<&Path>) -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let folder = program.parent().unwrap();
    std::fs::create_dir_all(folder).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), program).unwrap();
    let mut command = if relative {
        let mut command = std::process::Command::new("./sleeper");
        command.current_dir(folder);
        command
    } else {
        std::process::Command::new(program)
    };
    if let Some(argv0) = argv0 {
        command.arg0(argv0);
    }
    command
        .args(["--exact", "--nocapture", SLEEPER_TEST])
        .env("SLEEPER", "1")
        .stdout(std::process::Stdio::piped());
    // On Linux another test's fork can hold the copy's write descriptor until
    // it execs, which makes the copy busy for that moment.
    let mut tries = 0;
    let child = loop {
        match command.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            spawned => break spawned.unwrap(),
        }
    };
    started(child)
}

// Waits for the child to say it is the sleeper. Until it does, it may still be
// the shell, which no folder holds.
#[cfg(unix)]
fn started(mut child: std::process::Child) -> std::process::Child {
    let mut lines = std::io::BufReader::new(child.stdout.as_mut().unwrap());
    let mut line = String::new();
    while line.trim() != SLEEPER_STARTED {
        line.clear();
        assert_ne!(
            std::io::BufRead::read_line(&mut lines, &mut line).unwrap(),
            0
        );
    }
    child
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
fn removing_a_build_ends_a_process_started_by_a_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    let mut child = sleep_as(&build.join("debug/sleeper"), true, None);

    remove_build(dir.path(), &build).unwrap();
    let ended = ended_within_ten_seconds(&mut child);
    let _ = child.kill();
    assert!(ended);
}

#[cfg(unix)]
#[test]
fn removing_a_build_ends_a_process_whose_argv0_was_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    let mut child = sleep_as(
        &build.join("debug/sleeper"),
        false,
        Some(Path::new("/elsewhere/not-the-sleeper")),
    );

    remove_build(dir.path(), &build).unwrap();
    let ended = ended_within_ten_seconds(&mut child);
    let _ = child.kill();
    assert!(ended);
}

#[cfg(unix)]
#[test]
fn removing_a_build_spares_a_process_that_only_names_a_path_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    std::fs::create_dir_all(&build).unwrap();
    let mut outside = sleep_as(
        &dir.path().join("elsewhere/sleeper"),
        false,
        Some(&build.join("debug/sleeper")),
    );

    remove_build(dir.path(), &build).unwrap();
    let running = outside.try_wait().unwrap().is_none();
    let _ = outside.kill();
    assert!(running);
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
