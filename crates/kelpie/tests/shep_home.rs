//! A runner, the dog and the relay's commands without `SHEP_HOME`, against
//! the real binary: each refuses, and says what sets it.

use std::process::{Command, Output, Stdio};

const KELPIE: &str = env!("CARGO_BIN_EXE_kelpie");

// A scratch home, and no `SHEP_HOME` however the test is run.
fn kelpie(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new(KELPIE)
        .args(args)
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_runner_without_shep_home_names_the_flockfile_line() {
    let output = kelpie(&["runner", "shep"]);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("SHEP_HOME is not set"), "{stderr}");
    assert!(stderr.contains("env = { SHEP_HOME ="), "{stderr}");
}

#[test]
fn the_dog_without_shep_home_names_the_flockfile_line() {
    let output = kelpie(&["dog"]);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("SHEP_HOME is not set"), "{stderr}");
    assert!(stderr.contains("env = { SHEP_HOME ="), "{stderr}");
}

#[test]
fn the_relay_commands_without_shep_home_refuse() {
    for args in [
        &["relay-yes", "shep", "1"][..],
        &["relay-answer", "shep", "1 no rename it"][..],
    ] {
        let output = kelpie(args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = stderr(&output);
        assert!(
            stderr.contains("SHEP_HOME is not set"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains("the relay's settings"),
            "{args:?}: {stderr}"
        );
    }
}
