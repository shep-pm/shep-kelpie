//! A runner without `SHEP_HOME` or beside an old home that never moved, and
//! the adopted dog without its channel, against the real binary: each
//! refuses, and says what fixes it.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const KELPIE: &str = env!("CARGO_BIN_EXE_shep-kelpie");

// A scratch home, and no `SHEP_HOME` however the test is run.
fn kelpie(args: &[&str]) -> Output {
    kelpie_with(args, &[])
}

fn kelpie_with(args: &[&str], env: &[(&str, &str)]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new(KELPIE)
        .args(args)
        .env_clear()
        .env("HOME", home.path())
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn runner_under(home: &Path, shep: &Path) -> Output {
    Command::new(KELPIE)
        .args(["runner", "koji"])
        .env_clear()
        .env("HOME", home)
        .env("SHEP_HOME", shep)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_runner_without_shep_home_names_the_flockfile_line() {
    let output = kelpie(&["runner", "shep"]);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("SHEP_HOME is not set"), "{stderr}");
    assert!(stderr.contains("env = { SHEP_HOME ="), "{stderr}");
}

// Adopted before kelpie asked for the channel, shep starts it with none.
#[test]
fn the_adopted_dog_without_its_channel_names_the_adopt_again() {
    let dog = [
        ("SHEP_DOG_NAME", "kelpie"),
        ("SHEP_NAME", "kelpie"),
        ("SHEP_HOME", "/s"),
    ];
    let output = kelpie_with(&[], &dog);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("no shepherd channel"), "{stderr}");
    let adopt = format!("`shep adopt {KELPIE} --name kelpie`");
    assert!(stderr.contains(&adopt), "{stderr}");
}

// `shep kelpie` with no verb sets SHEP_DOG_NAME and no SHEP_NAME.
#[test]
fn a_bare_shep_kelpie_prints_the_usage_and_runs_no_dog() {
    let output = kelpie_with(&[], &[("SHEP_DOG_NAME", "kelpie"), ("SHEP_HOME", "/s")]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.starts_with("usage: "), "{stderr}");
    assert!(!stderr.contains("shepherd channel"), "{stderr}");
}

// A runner with a long `SHEP_HOME` stops before it opens anything.
#[test]
fn a_runner_whose_sockets_would_be_too_long_refuses() {
    let home = tempfile::tempdir().unwrap();
    let shep = home.path().join("s".repeat(90));
    let output = runner_under(home.path(), &shep);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("longer than the 103"), "{stderr}");
    assert!(!shep.join("kelpie").exists());
}

// Kelpie's files left in `~/.kelpie` by a home that never moved.
#[test]
fn a_runner_beside_an_unmoved_old_home_refuses_naming_it() {
    let home = tempfile::tempdir().unwrap();
    let old = home.path().join(".kelpie");
    std::fs::create_dir_all(old.join("projects/koji")).unwrap();
    std::fs::write(old.join("projects/koji/state.json"), "{}").unwrap();
    // Short enough for the socket check, and never made.
    let output = runner_under(home.path(), Path::new("/s"));

    // 78 is what the runner's flock entry lists in `stop_exit_codes`.
    assert_eq!(output.status.code(), Some(78));
    let stderr = stderr(&output);
    assert!(stderr.contains(&old.display().to_string()), "{stderr}");
    assert!(stderr.contains("move "), "{stderr}");
    assert!(old.join("projects/koji/state.json").is_file());
}
