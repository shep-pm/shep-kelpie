//! This machine, as doctor reads it

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::doctor::host::{Host, gaps, missing_playwright_packages, sandbox_needs};
use crate::preview::Tools;

/// The machine kelpie is running on
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl Host for SystemHost {
    fn sandbox_gaps(&self) -> Vec<&'static str> {
        gaps(sandbox_needs(env::consts::OS), on_path)
    }

    fn playwright_gaps(&self, tools: &Tools) -> Result<Vec<String>, String> {
        let cli = tools.playwright_cli();
        if !cli.is_file() {
            // `missing()` already reports the tools themselves as absent.
            return Ok(Vec::new());
        }
        let output = Command::new("node")
            .arg(cli)
            .args(["install-deps", "--dry-run", "chromium-headless-shell"])
            .env("PLAYWRIGHT_BROWSERS_PATH", tools.browsers())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run `node`: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let packages = missing_playwright_packages(&stdout);
        if !packages.is_empty() || output.status.success() {
            return Ok(packages);
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let first = said
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("install-deps --dry-run failed with no message");
        Err(first.to_owned())
    }
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
