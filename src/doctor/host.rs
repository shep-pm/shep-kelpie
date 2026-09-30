//! What doctor asks of the machine itself, beyond the runner's ports

use std::fmt;

/// The machine kelpie runs on
pub trait Host: Send + Sync + fmt::Debug {
    /// The programs Claude Code's sandbox needs that are not on this machine,
    /// none when it can run
    fn sandbox_gaps(&self) -> Vec<&'static str>;
}

/// The programs Claude Code's sandbox needs on `os`, as `std::env::consts::OS` names it
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
}
