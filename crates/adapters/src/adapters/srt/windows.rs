//! The path lists srt-win reads
//!
//! srt-win expands a glob to every file that exists, which for `~/.cargo/**`
//! is tens of thousands of denies, and refuses one on a file with two
//! hardlinks. A deny on a folder is one inheritable ACE, so a folder is
//! written as itself. Every list is `\`-separated and holds a path once.

use std::collections::HashSet;
use std::path::Path;

/// `list` as srt-win reads it: `<folder>/**` as `<folder>`, separators as `\`,
/// each path once (Windows ignores case), and no path that still holds a glob
pub(super) fn paths<P: AsRef<Path>>(list: impl IntoIterator<Item = P>) -> Vec<String> {
    let mut seen = HashSet::new();
    list.into_iter()
        .map(|path| path.as_ref().to_string_lossy().replace('/', r"\"))
        .map(|path| match path.strip_suffix(r"\**") {
            Some(folder) => folder.to_owned(),
            None => path,
        })
        .filter(|path| !has_glob(path))
        .filter(|path| seen.insert(path.to_lowercase()))
        .collect()
}

// Whether `path` holds `*` or `?`, as srt-win reads one: the `?` of a `\\?\`
// prefix is the namespace, and `[` is legal in a name.
fn has_glob(path: &str) -> bool {
    let body = path.strip_prefix(r"\\?\").unwrap_or(path);
    body.contains(['*', '?'])
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::{Platform, settings_on};
    use crate::ports::Policy;

    use std::path::PathBuf;

    fn policy() -> Policy {
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        Policy {
            read: paths(&[
                r"C:\Users\R\.kelpie\wt\22",
                r"C:\Users\R\.kelpie\skills",
                r"C:\Users\R\.kelpie\skills",
                r"c:\users\r\.kelpie\Skills",
                r"C:\Users\R\.kelpie/wt/22",
            ]),
            write: paths(&[
                r"C:\Users\R\.kelpie\wt\22",
                r"C:\Users\R\.kelpie\wt\22\.git\logs/refs/heads\kelpie/22",
            ]),
            no_read: vec![
                r"C:\Users\R/**".into(),
                "~/.ssh/**".into(),
                "~/.netrc".into(),
            ],
            no_write: paths(&[
                r"C:\Users\R\.kelpie\wt\22\.git\hooks",
                r"C:\Users\R\.kelpie\wt\22\.claude",
                r"C:\Users\R\.kelpie\wt\22/**/.claude",
            ]),
            ..Policy::default()
        }
    }

    fn files(platform: Platform) -> Value {
        settings_on(&policy(), platform, false)["filesystem"].clone()
    }

    #[test]
    fn on_windows_no_list_holds_a_glob() {
        let files = files(Platform::Windows);
        for list in ["denyRead", "denyWrite", "allowRead", "allowWrite"] {
            for path in files[list].as_array().unwrap() {
                assert!(!path.as_str().unwrap().contains('*'), "{list}: {path}");
            }
        }
    }

    #[test]
    fn on_windows_a_folder_deny_is_the_folder_and_elsewhere_it_keeps_its_glob() {
        assert_eq!(
            files(Platform::Windows)["denyRead"],
            json!([r"C:\Users\R", r"~\.ssh", r"~\.netrc"])
        );
        for platform in [Platform::Macos, Platform::Linux] {
            assert_eq!(
                files(platform)["denyRead"],
                json!([r"C:\Users\R/**", "~/.ssh/**", "~/.netrc"]),
                "{platform:?}"
            );
        }
    }

    #[test]
    fn on_windows_a_nested_harness_folder_that_is_listed_by_path_is_kept() {
        let denied = files(Platform::Windows)["denyWrite"].clone();
        assert_eq!(
            denied,
            json!([
                r"C:\Users\R\.kelpie\wt\22\.git\hooks",
                r"C:\Users\R\.kelpie\wt\22\.claude",
            ])
        );
    }

    #[test]
    fn on_windows_each_read_folder_is_listed_once_and_every_path_has_one_separator() {
        let files = files(Platform::Windows);
        assert_eq!(
            files["allowRead"],
            json!([r"C:\Users\R\.kelpie\wt\22", r"C:\Users\R\.kelpie\skills",])
        );
        assert_eq!(
            files["allowWrite"],
            json!([
                r"C:\Users\R\.kelpie\wt\22",
                r"C:\Users\R\.kelpie\wt\22\.git\logs\refs\heads\kelpie\22",
            ])
        );
    }

    #[test]
    fn a_verbatim_prefix_is_not_a_glob() {
        assert_eq!(
            super::paths([r"\\?\C:\Users\R\AppData\Roaming\npm", r"\\?\C:\Users\R/**"]),
            [r"\\?\C:\Users\R\AppData\Roaming\npm", r"\\?\C:\Users\R"]
        );
    }
}
