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
