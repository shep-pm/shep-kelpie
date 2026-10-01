//! What doctor asks of the machine itself, beyond the runner's ports

use std::fmt;

use crate::preview::Tools;

/// The machine kelpie runs on
pub trait Host: Send + Sync + fmt::Debug {
    /// The programs the sandbox runtime needs that are not on this machine,
    /// none when it can run
    fn sandbox_gaps(&self) -> Vec<&'static str>;

    /// The packages `npx playwright install-deps` would still install for
    /// `tools`' headless Chromium, checked without installing anything.
    /// Empty on a machine, such as macOS, that needs none. `Err` when the
    /// check itself could not run.
    fn playwright_gaps(&self, tools: &Tools) -> Result<Vec<String>, String>;
}

/// The programs the sandbox runtime needs on `os`, as `std::env::consts::OS` names it
pub const fn sandbox_needs(os: &str) -> &'static [&'static str] {
    match os.as_bytes() {
        b"macos" => &["sandbox-exec"],
        b"linux" => &["bwrap", "socat"],
        _ => &[],
    }
}

/// The programs in `needs` that `on_path` does not find
pub fn gaps(needs: &[&'static str], on_path: impl Fn(&str) -> bool) -> Vec<&'static str> {
    needs
        .iter()
        .copied()
        .filter(|program| !on_path(program))
        .collect()
}

/// The packages named in `install-deps --dry-run`'s own report
///
/// On a missing package it prints `Missing system dependencies (N):` then
/// one indented line per package; it prints nothing at all, and exits `0`,
/// on a machine such as macOS that needs none.
pub fn missing_playwright_packages(stdout: &str) -> Vec<String> {
    let Some(start) = stdout.find("Missing system dependencies") else {
        return Vec::new();
    };
    stdout[start..]
        .lines()
        .skip(1)
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_needs_its_own_sandbox_and_linux_needs_two_programs() {
        assert_eq!(sandbox_needs("macos"), ["sandbox-exec"]);
        assert_eq!(sandbox_needs("linux"), ["bwrap", "socat"]);
        assert!(sandbox_needs("windows").is_empty());
    }

    #[test]
    fn a_gap_is_a_needed_program_off_the_path() {
        let needs = sandbox_needs("linux");
        assert_eq!(gaps(needs, |_| true), Vec::<&str>::new());
        assert_eq!(gaps(needs, |p| p == "bwrap"), ["socat"]);
        assert_eq!(gaps(needs, |_| false), ["bwrap", "socat"]);
    }

    #[test]
    fn missing_packages_are_read_from_the_dry_runs_own_report() {
        let stdout = "Missing system dependencies (2):\n  libnspr4\n  libnss3\n";
        assert_eq!(missing_playwright_packages(stdout), ["libnspr4", "libnss3"]);
    }

    #[test]
    fn no_report_line_is_no_missing_package() {
        assert_eq!(missing_playwright_packages(""), Vec::<String>::new());
        assert_eq!(
            missing_playwright_packages("All system dependencies are installed.\n"),
            Vec::<String>::new()
        );
    }
}
