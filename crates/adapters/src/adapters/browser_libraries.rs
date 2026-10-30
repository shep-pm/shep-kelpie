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
        let text = String::from_utf8_lossy(&output.stdout);
        let missing: Vec<_> = text
            .lines()
            .filter_map(|line| line.trim().strip_suffix("=> not found"))
            .map(|name| name.trim().to_owned())
            .collect();
        if missing.is_empty() && !output.status.success() {
            return Err(format!(
                "ldd failed on {}: {}",
                shell.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        gaps.extend(missing);
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
    fn ldd_names_missing_libraries_from_the_recorded_wsl_output() {
        let dir = tempfile::tempdir().unwrap();
        let browsers = dir.path().join("browsers");
        let shell = browsers.join("chromium_headless_shell-1200/chrome-linux/headless_shell");
        std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
        std::fs::write(&shell, "").unwrap();
        let output = dir.path().join("ldd.txt");
        std::fs::write(
            &output,
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
