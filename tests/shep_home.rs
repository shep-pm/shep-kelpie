//! A runner and the relay's commands without `SHEP_HOME`, and the adopted
//! dog without its channel, against the real binary: each refuses, and says
//! what fixes it.

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
    let output = kelpie_with(&[], &[("SHEP_DOG_NAME", "kelpie"), ("SHEP_HOME", "/s")]);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("no shepherd channel"), "{stderr}");
    let adopt = format!("`shep adopt {KELPIE} --name kelpie`");
    assert!(stderr.contains(&adopt), "{stderr}");
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
