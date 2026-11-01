//! Processes still running from a build folder
//!
//! A call's process group is killed when the call ends, but a test binary a
//! worker's `cargo test` left behind has the init process for its parent and
//! is in no group kelpie holds. Run from a build folder that is then deleted,
//! it spins until the machine restarts. Removing the build folder ends each
//! process whose program lies inside it first.

use std::path::Path;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

// How long a process has to end on SIGTERM before it gets SIGKILL.
#[cfg(unix)]
const GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);

/// Ends every process whose first argument is a path inside `build`
///
/// Each gets SIGTERM, and SIGKILL if it is still there after a short grace. A
/// process listing that fails or prints nothing is logged and skipped, so the
/// removal that calls this never fails because of it. Does nothing off Unix.
#[cfg(unix)]
pub(super) fn end_running_from(build: &Path) {
    let mut prefixes = vec![prefix_of(build)];
    if let Ok(real) = build.canonicalize() {
        let real = prefix_of(&real);
        if !prefixes.contains(&real) {
            prefixes.push(real);
        }
    }
    let found = match running_from(&prefixes) {
        Ok(found) => found,
        Err(why) => {
            eprintln!(
                "not ending the processes running from {}, since {why}",
                build.display()
            );
            return;
        }
    };
    if found.is_empty() {
        return;
    }
    signal("TERM", &found);
    let deadline = Instant::now() + GRACE;
    let mut left = found;
    while Instant::now() < deadline {
        std::thread::sleep(POLL);
        let Ok(now) = running_from(&prefixes) else {
            return;
        };
        left.retain(|pid| now.contains(pid));
        if left.is_empty() {
            return;
        }
    }
    signal("KILL", &left);
}

/// Does nothing: only Unix lists processes this way
#[cfg(not(unix))]
pub(super) fn end_running_from(_build: &Path) {}

// The build folder's path with the separator that ends it, so a sibling named
// like it with more after is not inside it.
#[cfg(unix)]
fn prefix_of(build: &Path) -> String {
    let text = build.to_string_lossy();
    format!("{}/", text.trim_end_matches('/'))
}

#[cfg(unix)]
fn running_from(prefixes: &[String]) -> Result<Vec<u32>, String> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,args="])
        .output()
        .map_err(|e| format!("ps cannot start: {e}"))?;
    if !out.status.success() {
        return Err(format!("ps failed: {}", out.status));
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    if listing.trim().is_empty() {
        return Err("ps printed nothing".to_owned());
    }
    Ok(inside(&listing, prefixes, std::process::id()))
}

// The pids in a `ps -o pid=,args=` listing whose command starts with one of
// `prefixes`, `own` left out.
#[cfg(unix)]
fn inside(listing: &str, prefixes: &[String], own: u32) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|line| {
            let (pid, args) = line.trim_start().split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let args = args.trim_start();
            (pid != own && prefixes.iter().any(|p| args.starts_with(p.as_str()))).then_some(pid)
        })
        .collect()
}

// A process that is already gone makes `kill` fail, which is what was wanted.
#[cfg(unix)]
fn signal(name: &str, pids: &[u32]) {
    let _ = Command::new("kill")
        .arg(format!("-{name}"))
        .args(pids.iter().map(u32::to_string))
        .output();
}

#[cfg(all(test, unix))]
mod tests {
    use super::inside;

    const LISTING: &str = "\
    1 /sbin/launchd
  412 /Users/x/builds/7/debug/deps/shep_kelpie-0a1b2c --test-threads=1
  413 /Users/x/builds/70/debug/deps/other-9f
  414 cargo test --target-dir /Users/x/builds/7
  415 /Users/x/builds/7
  416 /Users/x/builds/7/release/app
  417 (sleeper)
";

    #[test]
    fn a_listing_yields_the_programs_inside_the_folder() {
        let prefixes = ["/Users/x/builds/7/".to_owned()];
        assert_eq!(inside(LISTING, &prefixes, 999), [412, 416]);
    }

    #[test]
    fn the_calling_process_is_never_picked() {
        let prefixes = ["/Users/x/builds/7/".to_owned()];
        assert_eq!(inside(LISTING, &prefixes, 412), [416]);
    }

    #[test]
    fn a_folder_known_by_two_paths_picks_the_programs_of_both() {
        let prefixes = [
            "/Users/x/builds/7/".to_owned(),
            "/Users/x/builds/70/".to_owned(),
        ];
        assert_eq!(inside(LISTING, &prefixes, 999), [412, 413, 416]);
    }
}
