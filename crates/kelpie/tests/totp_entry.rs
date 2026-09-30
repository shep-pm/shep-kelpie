//! `shep kelpie totp`: shep starts the adopted kelpie with `totp` and its
//! flags, in the caller's folder, with `SHEP_HOME` and `SHEP_DOG_NAME` set,
//! and the command reaches the same code as `kelpie totp`.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const KELPIE: &str = env!("CARGO_BIN_EXE_kelpie");

// The environment shep gives an adopted dog, over a scratch kelpie home.
fn shep_kelpie(home: &Path, args: &[&str]) -> Output {
    Command::new(KELPIE)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("KELPIE_HOME", home.join("kelpie"))
        .env("SHEP_HOME", home.join("shep"))
        .env("SHEP_DOG_NAME", "kelpie")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn uri(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.starts_with("otpauth://"))
        .unwrap_or_else(|| panic!("no otpauth URI in {output:?}"))
        .to_owned()
}

#[test]
fn totp_prints_the_secret_and_rotate_replaces_it() {
    let home = tempfile::tempdir().unwrap();
    let first = uri(&shep_kelpie(home.path(), &["totp"]));
    assert_eq!(uri(&shep_kelpie(home.path(), &["totp"])), first);
    assert_ne!(uri(&shep_kelpie(home.path(), &["totp", "--rotate"])), first);
}

#[test]
fn totp_unlock_turns_answers_on() {
    let home = tempfile::tempdir().unwrap();
    let output = shep_kelpie(home.path(), &["totp", "--unlock"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("answers from ntfy are on again"),
        "{stdout}"
    );
}
