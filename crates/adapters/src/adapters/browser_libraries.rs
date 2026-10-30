//! Shared libraries Playwright's headless Chromium needs on Linux

use std::path::Path;

pub(super) fn read(browsers: &Path, ldd: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(browsers)
        .map_err(|e| format!("cannot read {}: {e}", browsers.display()))?;
    let mut shells = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read a browser entry: {e}"))?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("chromium_headless_shell-")
        {
            continue;
        }
        for relative in [
            "chrome-linux/headless_shell",
            "chrome-headless-shell-linux64/chrome-headless-shell",
        ] {
            let shell = entry.path().join(relative);
            if shell.is_file() {
                shells.push(shell);
            }
        }
    }
    if shells.is_empty() {
        return Err(format!(
            "no Playwright headless shell under {}",
            browsers.display()
        ));
    }
    shells.sort();
    let mut gaps = Vec::new();
    for shell in shells {
        let output = crate::spawn::command(ldd)
            .arg(&shell)
            .output()
            .map_err(|e| format!("cannot run ldd on {}: {e}", shell.display()))?;
        if !output.status.success() {
            return Err(format!(
                "ldd failed on {} ({}): {}",
                shell.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        gaps.extend(
            text.lines()
                .filter_map(|line| line.trim().strip_suffix("=> not found"))
                .map(|name| name.trim().to_owned()),
        );
    }
    gaps.sort();
    gaps.dedup();
    Ok(gaps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::{Fake, write_fake};

    #[test]
    fn no_installed_headless_shell_is_an_incomplete_check() {
        let dir = tempfile::tempdir().unwrap();
        let browsers = dir.path().join("browsers");
        std::fs::create_dir(&browsers).unwrap();
        let why = read(&browsers, Path::new("unused-ldd")).unwrap_err();
        assert!(why.contains("no Playwright headless shell"), "{why}");
    }

    #[test]
    fn multiple_shells_include_the_alternate_path_and_duplicate_gaps_are_named_once() {
        let dir = tempfile::tempdir().unwrap();
        let browsers = dir.path().join("browsers");
        for relative in [
            "chromium_headless_shell-1200/chrome-linux/headless_shell",
            "chromium_headless_shell-1300/chrome-headless-shell-linux64/chrome-headless-shell",
        ] {
            let shell = browsers.join(relative);
            std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
            std::fs::write(shell, "").unwrap();
        }
        let calls = dir.path().join("calls");
        let ldd = write_fake(
            dir.path(),
            "ldd",
            &Fake::new()
                .records_args(&calls)
                .prints("libnss3.so => not found")
                .prints("libnss3.so => not found"),
        );
        assert_eq!(read(&browsers, &ldd), Ok(vec!["libnss3.so".into()]));
        let calls = std::fs::read_to_string(calls).unwrap();
        assert_eq!(calls.trim(), browsers.join("chromium_headless_shell-1300/chrome-headless-shell-linux64/chrome-headless-shell").to_str().unwrap());
    }

    #[test]
    fn resolved_libraries_leave_no_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let browsers = dir.path().join("browsers");
        let shell = browsers.join("chromium_headless_shell-1200/chrome-linux/headless_shell");
        std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
        std::fs::write(&shell, "").unwrap();
        let ldd = write_fake(
            dir.path(),
            "ldd",
            &Fake::new().prints("libc.so.6 => /lib/libc.so.6 (0x1)"),
        );
        assert_eq!(read(&browsers, &ldd), Ok(Vec::new()));

        let why = read(&browsers, &dir.path().join("missing-ldd")).unwrap_err();
        assert!(why.contains("cannot run ldd"), "{why}");
    }

    #[test]
    fn failed_ldd_does_not_report_partial_library_names_as_a_completed_check() {
        let dir = tempfile::tempdir().unwrap();
        let browsers = dir.path().join("browsers");
        let shell = browsers.join("chromium_headless_shell-1200/chrome-linux/headless_shell");
        std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
        std::fs::write(&shell, "").unwrap();
        let ldd = write_fake(
            dir.path(),
            "ldd",
            &Fake::new()
                .prints("libnss3.so => not found")
                .prints_to_stderr("cannot inspect the binary")
                .exits(1),
        );
        let why = read(&browsers, &ldd).expect_err("failed ldd must remain an incomplete check");
        assert!(why.contains("ldd failed"), "{why}");
        assert!(why.contains("cannot inspect the binary"), "{why}");
    }

    #[test]
    fn ldd_names_missing_libraries_from_the_recorded_wsl_output() {
        let dir = tempfile::tempdir().unwrap();
        let browsers = dir.path().join("browsers");
        let shell = browsers.join("chromium_headless_shell-1200/chrome-linux/headless_shell");
        std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
        std::fs::write(&shell, "").unwrap();
        let output = dir.path().join("ldd.txt");
        std::fs::write(
            &output,
            // The renamed dependency deliberately cannot resolve on the recorded WSL host.
            include_str!("../../../../fixtures/ldd-wsl-ubuntu-24.04.txt"),
        )
        .unwrap();
        let args = dir.path().join("args.txt");
        let ldd = write_fake(
            dir.path(),
            "ldd",
            &Fake::new().records_args(&args).prints_file(&output),
        );
        assert_eq!(read(&browsers, &ldd), Ok(vec!["libselinuy.so.1".into()]));
        assert_eq!(
            std::fs::read_to_string(args).unwrap().trim(),
            shell.to_str().unwrap()
        );
    }
}
