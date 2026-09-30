//! The binary's usage text, against the real binary: it names `shep-kelpie`

use std::process::{Command, Stdio};

#[test]
fn the_usage_text_names_shep_kelpie() {
    let output = Command::new(env!("CARGO_BIN_EXE_shep-kelpie"))
        .arg("no-such-command")
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("usage: shep-kelpie add"), "{stderr}");
    assert!(stderr.contains("shep-kelpie lease status"), "{stderr}");
    assert!(stderr.contains("run as `shep kelpie <verb>`"), "{stderr}");
}

// `-p` before the verb reaches the verb, which then finds no ruling in an
// empty kelpie home, rather than printing the usage.
#[test]
fn the_project_can_come_before_the_verb() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_shep-kelpie"))
        .args(["-p", "koji", "rule", "14", "yes"])
        .env_clear()
        .env("HOME", home.path())
        .env("KELPIE_HOME", home.path().join("kelpie"))
        .env("SHEP_HOME", home.path().join("shep"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("shep kelpie rule: no ruling 14 is waiting"),
        "{stderr}"
    );
}
