//! How many review bot rounds a pull request gets: a fixed number when the
//! project sets one, or else ceil(changed / divisor) + 1, where changed is
//! added plus removed lines outside generated files

use std::num::NonZeroU32;
use std::path::Path;
use std::process::{Command, Stdio};

/// The cap for `changed` lines
pub(super) fn cap(changed: u64, divisor: NonZeroU32) -> u32 {
    let rounds = changed.div_ceil(u64::from(divisor.get())) + 1;
    u32::try_from(rounds).unwrap_or(u32::MAX)
}

/// Whether `rounds` spent a fixed number of rounds, `fixed`
pub(super) fn spent(rounds: u32, fixed: Option<NonZeroU32>) -> bool {
    fixed.is_some_and(|fixed| rounds >= fixed.get())
}

/// Lines added plus removed on `worktree`'s branch since it left
/// `origin/main`, outside every glob in `generated`
///
/// # Errors
///
/// A message when git cannot be run or fails.
pub(super) fn changed_lines(worktree: &Path, generated: &[String]) -> Result<u64, String> {
    let base = format!("origin/{}...HEAD", crate::worktree::BASE);
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--numstat", "--no-renames", &base])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git diff: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr.trim()));
    }
    Ok(counted(&String::from_utf8_lossy(&output.stdout), generated))
}

// A binary file's numstat is `-\t-\tpath`, and counts nothing.
fn counted(numstat: &str, generated: &[String]) -> u64 {
    numstat
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let (added, removed, path) = (fields.next()?, fields.next()?, fields.next()?);
            let generated = generated.iter().any(|glob| matches(glob, path));
            let lines = |n: &str| n.parse::<u64>().unwrap_or(0);
            (!generated).then(|| lines(added) + lines(removed))
        })
        .sum()
}

/// Whether `path` matches `glob`: `*` and `?` stay inside one folder,
/// `**` spans folders, and `**/` may match none
pub(super) fn matches(glob: &str, path: &str) -> bool {
    matches_bytes(glob.as_bytes(), path.as_bytes())
}

fn matches_bytes(glob: &[u8], path: &[u8]) -> bool {
    match glob {
        [] => path.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => {
            matches_bytes(rest, path)
                || path
                    .iter()
                    .enumerate()
                    .any(|(i, b)| *b == b'/' && matches_bytes(rest, &path[i + 1..]))
        }
        [b'*', b'*', rest @ ..] => (0..=path.len()).any(|i| matches_bytes(rest, &path[i..])),
        [b'*', rest @ ..] => {
            let folder = path.iter().position(|b| *b == b'/').unwrap_or(path.len());
            (0..=folder).any(|i| matches_bytes(rest, &path[i..]))
        }
        [b'?', rest @ ..] => path
            .first()
            .is_some_and(|b| *b != b'/' && matches_bytes(rest, &path[1..])),
        [c, rest @ ..] => path.first() == Some(c) && matches_bytes(rest, &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    #[test]
    fn the_cap_is_ceil_changed_over_the_divisor_plus_one() {
        assert_eq!(cap(0, nz(1000)), 1);
        assert_eq!(cap(1, nz(1000)), 2);
        assert_eq!(cap(1000, nz(1000)), 2);
        assert_eq!(cap(1001, nz(1000)), 3);
        assert_eq!(cap(2500, nz(1000)), 4);
    }

    // The example settings' globs, for shep.
    fn shep() -> Vec<String> {
        [
            "Cargo.lock",
            "**/snapshots/*.snap",
            "web/src/data/cli-reference.generated.txt",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn generated_files_are_left_out_of_the_count() {
        let numstat = "10\t2\tcrates/kelpie/src/lib.rs\n\
                       900\t850\tCargo.lock\n\
                       40\t0\tcrates/shep-cli/tests/snapshots/status.snap\n\
                       300\t300\tweb/src/data/cli-reference.generated.txt\n\
                       -\t-\tweb/public/logo.png\n\
                       3\t5\tsnapshots/top.snap\n";
        assert_eq!(counted(numstat, &shep()), 12);
        assert_eq!(counted(numstat, &[]), 12 + 1750 + 40 + 600 + 8);
    }

    #[test]
    fn a_glob_keeps_single_stars_inside_one_folder() {
        assert!(matches("*.lock", "Cargo.lock"));
        assert!(!matches("*.lock", "sub/Cargo.lock"));
        assert!(matches("**/*.lock", "sub/deeper/Cargo.lock"));
        assert!(matches("**/*.lock", "Cargo.lock"));
        assert!(matches("docs/**", "docs/a/b.md"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "a/c"));
        assert!(!matches("Cargo.lock", "Cargo.lockx"));
    }
}
