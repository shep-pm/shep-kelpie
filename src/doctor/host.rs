//! What doctor asks of the machine itself, beyond the runner's ports

use std::fmt;
use std::path::Path;

/// The machine kelpie runs on
pub trait Host: Send + Sync + fmt::Debug {
    /// The programs Claude Code's sandbox needs that are not on this machine,
    /// none when it can run
    fn sandbox_gaps(&self) -> Vec<&'static str>;

    /// The shared libraries the headless Chromium under `browsers` cannot
    /// load, none when it can run or this machine needs no check
    fn browser_gaps(&self, browsers: &Path) -> Vec<String>;
}

/// The `apt` package that holds `program`, as Debian and Ubuntu name it
pub fn apt_package(program: &str) -> &str {
    match program {
        "bwrap" => "bubblewrap",
        other => other,
    }
}

/// The libraries `ldd` says it cannot find, from its output for one binary
///
/// A line reads `\tlibnss3.so => not found`.
pub fn unresolved(ldd: &str) -> Vec<String> {
    ldd.lines()
        .filter_map(|line| line.trim().strip_suffix("=> not found"))
        .map(|name| name.trim().to_owned())
        .collect()
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

    // `ldd` on a copy of `ls` whose `libselinux.so.1` was renamed, run on
    // WSL2 Ubuntu 24.04 (shep-pm/shep-kelpie#135).
    const LDD: &str = "\tlinux-vdso.so.1 (0x00007ffed48a4000)
\tlibselinuy.so.1 => not found
\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007fcccd935000)
\t/lib64/ld-linux-x86-64.so.2 (0x00007fcccdb74000)
";

    #[test]
    fn a_library_ldd_cannot_find_is_named_and_the_rest_are_not() {
        assert_eq!(unresolved(LDD), ["libselinuy.so.1"]);
        assert!(unresolved("\tlibc.so.6 => /lib/libc.so.6 (0x1)\n").is_empty());
    }

    #[test]
    fn bwrap_comes_from_the_bubblewrap_package() {
        assert_eq!(apt_package("bwrap"), "bubblewrap");
        assert_eq!(apt_package("socat"), "socat");
    }

    #[test]
    fn a_gap_is_a_needed_program_off_the_path() {
        let needs = sandbox_needs("linux");
        assert_eq!(gaps(needs, |_| true), Vec::<&str>::new());
        assert_eq!(gaps(needs, |p| p == "bwrap"), ["socat"]);
        assert_eq!(gaps(needs, |_| false), ["bwrap", "socat"]);
    }
}
