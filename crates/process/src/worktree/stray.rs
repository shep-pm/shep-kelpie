//! Processes still running from a build folder
//!
//! A call's process group is killed when the call ends, but a test binary a
//! worker's `cargo test` left behind has the init process for its parent and
//! is in no group kelpie holds. Run from a build folder that is then deleted,
//! it spins until the machine restarts. Removing the build folder ends each
//! process whose program lies inside it first.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

// How long a process has to end on SIGTERM before it gets SIGKILL.
#[cfg(unix)]
const GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);

/// Ends every process whose program is inside `build`
///
/// Each gets SIGTERM, and SIGKILL if it is still there after a short grace. A
/// program is what the OS runs, read once from `/proc` or `lsof`; where neither
/// can be read the first argument stands in for it. A listing that fails is
/// logged and skipped, so the removal that calls this never fails because of
/// it. Does nothing off Unix.
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
        match still_there(&left) {
            Ok(now) => left.retain(|pid| now.contains(pid)),
            Err(why) => {
                eprintln!(
                    "cannot tell which processes running from {} ended, since {why}: killing the rest",
                    build.display()
                );
                break;
            }
        }
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
    let own = std::process::id();
    match programs() {
        Ok(programs) => Ok(programs_inside(&programs, prefixes, own)),
        Err(why) => {
            eprintln!(
                "cannot read each process's program, since {why}: matching the first argument instead"
            );
            let listing = ps(&["-A", "-o", "pid=,args="])?;
            if listing.trim().is_empty() {
                return Err("ps printed nothing".to_owned());
            }
            Ok(inside(&listing, prefixes, own))
        }
    }
}

// Every process's pid and the program it runs, from `/proc` where there is
// one and from `lsof` otherwise.
#[cfg(unix)]
fn programs() -> Result<Vec<(u32, PathBuf)>, String> {
    let proc = Path::new("/proc");
    if proc.join("self").exists() {
        return Ok(proc_programs(proc));
    }
    let out = Command::new("lsof")
        .args(["-d", "txt", "-Fpn"])
        .output()
        .map_err(|e| format!("lsof cannot start: {e}"))?;
    // lsof exits 1 when it could not look at some process, and prints the rest.
    let listing = String::from_utf8_lossy(&out.stdout);
    let programs = lsof_programs(&listing);
    if programs.is_empty() {
        return Err(format!("lsof listed no programs: {}", out.status));
    }
    Ok(programs)
}

// The pid and the target of `<pid>/exe` under `root`, for each folder named by
// a pid whose link can be read. A deleted program keeps its place.
#[cfg(unix)]
fn proc_programs(root: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let exe = std::fs::read_link(entry.path().join("exe")).ok()?;
            let exe = exe.to_string_lossy();
            Some((pid, PathBuf::from(exe.trim_end_matches(" (deleted)"))))
        })
        .collect()
}

// The pid and first text file of each process in `lsof -d txt -Fpn` output:
// the executable, which comes before `dyld` and the libraries.
#[cfg(unix)]
fn lsof_programs(listing: &str) -> Vec<(u32, PathBuf)> {
    let mut programs = Vec::new();
    let mut pid = None;
    let mut text = false;
    for line in listing.lines() {
        let Some((tag, rest)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => {
                pid = rest.parse().ok();
                text = false;
            }
            "f" => text = rest == "txt",
            "n" if text => {
                if let Some(pid) = pid.take() {
                    programs.push((pid, PathBuf::from(rest)));
                }
                text = false;
            }
            _ => {}
        }
    }
    programs
}

// The pids whose program lies in one of `prefixes`, `own` left out.
#[cfg(unix)]
fn programs_inside(programs: &[(u32, PathBuf)], prefixes: &[String], own: u32) -> Vec<u32> {
    programs
        .iter()
        .filter(|(pid, program)| {
            let program = program.to_string_lossy();
            *pid != own && prefixes.iter().any(|p| program.starts_with(p.as_str()))
        })
        .map(|(pid, _)| *pid)
        .collect()
}

// Which of `pids` are still there.
#[cfg(unix)]
fn still_there(pids: &[u32]) -> Result<Vec<u32>, String> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let listing = ps(&["-o", "pid=", "-p", &list])?;
    Ok(listing
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect())
}

// `ps` output. It exits 1 when `-p` names no process, so only a start that
// fails is an error.
#[cfg(unix)]
fn ps(args: &[&str]) -> Result<String, String> {
    let out = Command::new("ps")
        .args(args)
        .output()
        .map_err(|e| format!("ps cannot start: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
    use std::path::PathBuf;

    use super::{inside, lsof_programs, proc_programs, programs_inside};

    // `lsof -d txt -Fpn`: each process's executable, then `dyld` and the
    // libraries, all under the `ftxt` descriptor.
    const LSOF: &str = "\
p412
ftxt
n/Users/me/builds/7/debug/deps/shep_kelpie-0a1b2c
ftxt
n/usr/lib/dyld
ftxt
n/usr/lib/libSystem.B.dylib
p413
ftxt
n/bin/sleep
ftxt
n/usr/lib/dyld
p414
ftxt
n/Users/me/builds/7 files/app
ftxt
n/usr/lib/dyld
";

    #[test]
    fn an_lsof_listing_yields_each_processs_executable() {
        assert_eq!(
            lsof_programs(LSOF),
            [
                (
                    412,
                    PathBuf::from("/Users/me/builds/7/debug/deps/shep_kelpie-0a1b2c")
                ),
                (413, PathBuf::from("/bin/sleep")),
                (414, PathBuf::from("/Users/me/builds/7 files/app")),
            ]
        );
    }

    #[test]
    fn a_proc_folder_yields_the_program_each_pid_links_to() {
        let dir = tempfile::tempdir().unwrap();
        for (pid, program) in [
            ("412", "/Users/me/builds/7/app"),
            ("413", "/bin/sleep"),
            ("414", "/Users/me/builds/7/gone (deleted)"),
            ("self", "/bin/cat"),
        ] {
            std::fs::create_dir(dir.path().join(pid)).unwrap();
            std::os::unix::fs::symlink(program, dir.path().join(pid).join("exe")).unwrap();
        }
        // A process whose link cannot be read.
        std::fs::create_dir(dir.path().join("415")).unwrap();

        let mut programs = proc_programs(dir.path());
        programs.sort();
        assert_eq!(
            programs,
            [
                (412, PathBuf::from("/Users/me/builds/7/app")),
                (413, PathBuf::from("/bin/sleep")),
                (414, PathBuf::from("/Users/me/builds/7/gone")),
            ]
        );
    }

    #[test]
    fn programs_inside_the_folder_are_picked_and_the_calling_process_is_not() {
        let prefixes = ["/Users/me/builds/7/".to_owned()];
        let programs = lsof_programs(LSOF);
        assert_eq!(programs_inside(&programs, &prefixes, 999), [412]);
        assert_eq!(programs_inside(&programs, &prefixes, 412), [] as [u32; 0]);
    }

    const LISTING: &str = "\
    1 /sbin/launchd
  412 /Users/me/builds/7/debug/deps/shep_kelpie-0a1b2c --test-threads=1
  413 /Users/me/builds/70/debug/deps/other-9f
  414 cargo test --target-dir /Users/me/builds/7
  415 /Users/me/builds/7
  416 /Users/me/builds/7/release/app
  417 (sleeper)
";

    #[test]
    fn a_listing_yields_the_programs_inside_the_folder() {
        let prefixes = ["/Users/me/builds/7/".to_owned()];
        assert_eq!(inside(LISTING, &prefixes, 999), [412, 416]);
    }

    #[test]
    fn the_calling_process_is_never_picked() {
        let prefixes = ["/Users/me/builds/7/".to_owned()];
        assert_eq!(inside(LISTING, &prefixes, 412), [416]);
    }

    #[test]
    fn a_folder_known_by_two_paths_picks_the_programs_of_both() {
        let prefixes = [
            "/Users/me/builds/7/".to_owned(),
            "/Users/me/builds/70/".to_owned(),
        ];
        assert_eq!(inside(LISTING, &prefixes, 999), [412, 413, 416]);
    }
}
