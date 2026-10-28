//! The harness folders already below a worktree, for a sandbox that cannot fence them by glob

use std::fs;
use std::path::{Path, PathBuf};

use super::{HARNESSES, folds_to};

// Deep enough for a monorepo's packages, shallow enough that a link loop or a
// vendored tree costs little.
const DEPTH: usize = 8;
const ENTRIES: usize = 20_000;

/// Every harness folder below `worktree`, in any case, as a path, sorted
/// (`.config/goose` is listed as that, and `.config` is entered otherwise)
///
/// The walk is best effort: a folder it cannot read, a link it does not follow
/// and whatever lies past [`DEPTH`] levels or [`ENTRIES`] entries are left out,
/// and so is `.git`. A folder not found is caught by the check against `main`
/// after the call. A folder that holds one is listed whole and not entered.
pub(super) fn existing(worktree: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![(worktree.to_owned(), 0)];
    let mut seen = 0;
    while let Some((folder, depth)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > ENTRIES {
                found.sort();
                return found;
            }
            let name = entry.file_name();
            let parent = folder.file_name();
            if (HARNESSES.iter().flat_map(|own| own.folders)).any(|own| folds_to(&name, own))
                || (HARNESSES.iter().flat_map(|own| own.nested)).any(|[up, own]| {
                    parent.is_some_and(|parent| folds_to(parent, up)) && folds_to(&name, own)
                })
            {
                found.push(entry.path());
            } else if depth < DEPTH
                && name != ".git"
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
            {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::super::deny_write_on;
    use super::*;

    fn found(worktree: &Path) -> Vec<String> {
        existing(worktree)
            .iter()
            .map(|p| p.strip_prefix(worktree).unwrap().display().to_string())
            .collect()
    }

    #[test]
    fn a_goose_settings_folder_is_listed_at_any_depth_in_any_case() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        for folder in [
            ".config/goose/hooks",
            "app/.Config/GOOSE",
            "a/b/c/.config/goose",
            ".config/other/nested",
            "config/goose",
            "src/.config",
        ] {
            fs::create_dir_all(wt.join(folder)).unwrap();
        }
        assert_eq!(
            found(wt),
            [".config/goose", "a/b/c/.config/goose", "app/.Config/GOOSE"]
        );
    }

    #[test]
    fn a_goose_settings_folder_below_another_harness_folder_is_left_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        fs::create_dir_all(wt.join(".claude/.config/goose")).unwrap();
        fs::create_dir_all(wt.join("pkg/.config/other/.pi")).unwrap();
        assert_eq!(found(wt), [".claude", "pkg/.config/other/.pi"]);
    }

    // srt-win expands a glob to every file that exists, which is more than the
    // sandbox can grant, so Windows is given the folders as paths.
    #[test]
    fn on_windows_a_nested_harness_folder_is_denied_by_path_and_no_glob_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        for folder in ["src/.claude/skills", "app/.config/goose", "src/plain"] {
            fs::create_dir_all(wt.join(folder)).unwrap();
        }
        let listed = deny_write_on(wt, "windows");
        for folder in ["src/.claude", "app/.config/goose", ".claude", ".mcp.json"] {
            assert!(listed.contains(&wt.join(folder)), "{folder}: {listed:?}");
        }
        assert!(!listed.contains(&wt.join("src/plain")));
        assert!(
            listed.iter().all(|p| !p.to_string_lossy().contains('*')),
            "{listed:?}"
        );
        // macOS and Linux keep the globs srt reads there.
        for os in ["macos", "linux"] {
            assert!(
                deny_write_on(wt, os).contains(&wt.join("**").join(".claude")),
                "{os}"
            );
        }
    }
}
