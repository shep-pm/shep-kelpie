//! A stand-in script that is safe to run at once
//!
//! Shared by path with the integration tests, so it uses std alone.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Writes an executable stand-in script that is safe to run at once.
///
/// On Linux a child forked while the file is still open for writing holds
/// it, and `exec` of it fails with ETXTBSY. Other tests fork all the time,
/// so this waits until the script has been executed once, with the probe
/// variable set, before handing it over. The script's first line after the
/// shebang exits on the probe, so the probe run does nothing.
pub(crate) fn write_script(path: &Path, contents: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let (shebang, rest) = contents.split_once('\n').expect("a script has a shebang");
    assert!(shebang.starts_with("#!"), "{shebang:?}");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(path)
        .unwrap();
    write!(
        file,
        "{shebang}\n[ -n \"$KELPIE_TEST_PROBE\" ] && exit 0\n{rest}"
    )
    .unwrap();
    drop(file);

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match Command::new(path).env("KELPIE_TEST_PROBE", "1").status() {
            Ok(_) => return,
            Err(e) if e.raw_os_error() == Some(26) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("cannot run the stand-in script {}: {e}", path.display()),
        }
    }
}
