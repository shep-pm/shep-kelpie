//! This machine, as doctor reads it

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::doctor::host::{Host, gaps, sandbox_needs, unresolved};

/// The machine kelpie is running on
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl Host for SystemHost {
    fn sandbox_gaps(&self) -> Vec<&'static str> {
        gaps(sandbox_needs(env::consts::OS), on_path)
    }

    fn browser_gaps(&self, browsers: &Path) -> Vec<String> {
        if env::consts::OS != "linux" {
            return Vec::new();
        }
        let Some(shell) = headless_shell(browsers, 3) else {
            return Vec::new();
        };
        Command::new("ldd")
            .arg(shell)
            .output()
            .map(|out| unresolved(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or_default()
    }
}

// Playwright keeps it at `chromium_headless_shell-<build>/chrome-linux/headless_shell`.
fn headless_shell(dir: &Path, depth: u8) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            if let Some(found) = headless_shell(&path, depth - 1) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == "headless_shell") {
            return Some(path);
        }
    }
    None
}

// A program is on the path when a folder of `PATH` holds an executable file
// of its name. macOS keeps `sandbox-exec` in `/usr/bin`, which `PATH` names.
fn on_path(program: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|folder| {
        std::fs::metadata(Path::new(&folder).join(program))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_every_machine_has_is_on_the_path_and_a_made_up_one_is_not() {
        assert!(on_path("sh"));
        assert!(!on_path("kelpie-no-such-program"));
    }
}
