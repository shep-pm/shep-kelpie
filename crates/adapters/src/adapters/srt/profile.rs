//! The profile read grant's exclusions, and the read grants cut around them
//!
//! crabpot, like upstream, refuses a session whose read grant is an exclusion
//! of the profile read grant, lies inside one, or lies above one. A toolchain
//! home such as `~\.cargo` holds `credentials`, so it is granted as its
//! entries instead, less the excluded one. The rules are crabpot's
//! (`profile_exclusion_conflicts`): a `*` stands for one name and a trailing
//! `*` for a prefix, and a grant above a wildcard exclusion conflicts only
//! when something beneath it matches.

use std::cmp::Ordering;

use super::windows;
use crate::adapters::host::ProfileExclusions;

/// What the Windows profile read grant keeps out of a session, and how to list a folder
pub(super) struct Profile<'a> {
    pub exclusions: &'a ProfileExclusions,
    /// The names in the folder at a path, None when it cannot be listed
    pub entries: &'a dyn Fn(&str) -> Option<Vec<String>>,
}

// One exclusion, its path in lower case components
struct Rule<'a> {
    exclusion: &'a str,
    parts: Vec<String>,
    // A directory the grant leaves ungranted without denying it: a grant inside
    // it is fine
    ungranted_only: bool,
}

impl<'a> Rule<'a> {
    fn of(exclusion: &'a str, ungranted_only: bool) -> Self {
        Self {
            exclusion,
            parts: parts(exclusion),
            ungranted_only,
        }
    }
}

// How a grant stands to an exclusion
enum Standing {
    Equal,
    Inside,
    Above,
}

impl Profile<'_> {
    /// `folders` as read grants clear of every exclusion: a folder above one as
    /// its entries, down to the folder that holds it, and a folder equal to or
    /// inside one left out, with a line in `notes` naming it
    pub fn grants(&self, folders: Vec<String>, notes: &mut Vec<String>) -> Vec<String> {
        let rules = self.rules();
        let mut granted = Vec::new();
        for folder in folders {
            self.place(&rules, folder, true, &mut granted, notes);
        }
        // An entry may be a grant of its own too.
        windows::paths(granted)
    }

    fn rules(&self) -> Vec<Rule<'_>> {
        let protected = (self.exclusions.exclusions.iter()).map(|path| Rule::of(path, false));
        let ungranted = (self.exclusions.ungranted_only.iter()).map(|path| Rule::of(path, true));
        protected.chain(ungranted).collect()
    }

    fn place(
        &self,
        rules: &[Rule<'_>],
        path: String,
        named: bool,
        granted: &mut Vec<String>,
        notes: &mut Vec<String>,
    ) {
        let key = parts(&path);
        let mut holds = None;
        for rule in rules {
            match standing(&key, &rule.parts) {
                Some(Standing::Equal) => return left_out(&path, rule, named, notes),
                Some(Standing::Inside) if !rule.ungranted_only => {
                    return left_out(&path, rule, named, notes);
                }
                Some(Standing::Above) if holds.is_none() && self.reaches(rule, &path, &key) => {
                    holds = Some(rule);
                }
                _ => {}
            }
        }
        let Some(rule) = holds else {
            return granted.push(path);
        };
        match (self.entries)(&path) {
            Some(names) => {
                for name in names {
                    let entry = format!(r"{}\{name}", path.trim_end_matches('\\'));
                    self.place(rules, entry, false, granted, notes);
                }
            }
            None => notes.push(format!(
                "the read grant {path} is left out, since it holds {} and cannot be listed",
                rule.exclusion
            )),
        }
    }

    // Whether `rule` lies beneath the folder at `path`: always for a plain
    // exclusion, and for a wildcard one when something there matches.
    fn reaches(&self, rule: &Rule<'_>, path: &str, key: &[String]) -> bool {
        !rule.exclusion.contains('*') || self.matches_beneath(path, &rule.parts[key.len()..])
    }

    fn matches_beneath(&self, folder: &str, rest: &[String]) -> bool {
        let Some((next, after)) = rest.split_first() else {
            return true;
        };
        let names = (self.entries)(folder).unwrap_or_default();
        (names.iter())
            .filter(|name| component_matches(next, &name.to_lowercase()))
            .any(|name| {
                self.matches_beneath(&format!(r"{}\{name}", folder.trim_end_matches('\\')), after)
            })
    }
}

// An entry of a split folder that is excluded is how the split works, so only
// a grant the policy named is noted.
fn left_out(path: &str, rule: &Rule<'_>, named: bool, notes: &mut Vec<String>) {
    if !named {
        return;
    }
    notes.push(format!(
        "the read grant {path} is left out, since the profile read grant excludes {}",
        rule.exclusion
    ));
}

// A path's components in lower case, as Windows compares them
fn parts(path: &str) -> Vec<String> {
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('/', r"\");
    (path.trim_matches('\\').to_lowercase())
        .split('\\')
        .map(str::to_owned)
        .collect()
}

// One component of an exclusion against one of a path, both in lower case
fn component_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

fn standing(path: &[String], rule: &[String]) -> Option<Standing> {
    let shared = path.len().min(rule.len());
    if !(0..shared).all(|i| component_matches(&rule[i], &path[i])) {
        return None;
    }
    Some(match path.len().cmp(&rule.len()) {
        Ordering::Equal => Standing::Equal,
        Ordering::Greater => Standing::Inside,
        Ordering::Less => Standing::Above,
    })
}

/// The names in the folder at `path`, sorted, None when it cannot be read
pub(super) fn list(path: &str) -> Option<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    Some(names)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::super::{Platform, settings_in};
    use super::*;
    use crate::ports::Policy;

    // A Windows machine's folders, as `(folder, names in it)`, and the folders
    // `settings_in` asked to list
    struct Disk {
        folders: Vec<(&'static str, Vec<&'static str>)>,
        listed: RefCell<Vec<String>>,
    }

    impl Disk {
        fn new(folders: &[(&'static str, &[&'static str])]) -> Self {
            Self {
                folders: folders
                    .iter()
                    .map(|(f, names)| (*f, names.to_vec()))
                    .collect(),
                listed: RefCell::default(),
            }
        }

        fn list(&self, path: &str) -> Option<Vec<String>> {
            self.listed.borrow_mut().push(path.to_owned());
            let (_, names) = self
                .folders
                .iter()
                .find(|(f, _)| f.eq_ignore_ascii_case(path))?;
            let mut names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
            names.sort_by_key(|name| name.to_lowercase());
            Some(names)
        }
    }

    fn exclusions(protected: &[&str], ungranted_only: &[&str]) -> ProfileExclusions {
        let owned = |list: &[&str]| list.iter().map(|path| (*path).to_owned()).collect();
        ProfileExclusions {
            exclusions: owned(protected),
            ungranted_only: owned(ungranted_only),
        }
    }

    // The Windows settings for a policy that reads `read` and denies `no_read`,
    // with the notes they came with
    fn windows(
        read: &[&str],
        no_read: &[&str],
        excluding: &ProfileExclusions,
        disk: &Disk,
    ) -> (Value, Vec<String>) {
        let policy = Policy {
            read: read.iter().map(PathBuf::from).collect(),
            no_read: no_read.iter().map(|path| (*path).to_owned()).collect(),
            ..Policy::default()
        };
        let profile = Profile {
            exclusions: excluding,
            entries: &|path| disk.list(path),
        };
        let (settings, notes) = settings_in(
            &policy,
            Platform::Windows,
            false,
            None,
            &|_: &Path| true,
            &profile,
        );
        (settings["filesystem"].clone(), notes)
    }

    const CARGO: &str = r"C:\Users\R\.cargo";

    fn cargo_disk() -> Disk {
        Disk::new(&[(
            CARGO,
            &[
                "bin",
                "registry",
                "git",
                ".crates.toml",
                ".crates2.json",
                "env",
                "credentials",
                "credentials.toml",
            ],
        )])
    }

    #[test]
    fn a_read_folder_above_an_exclusion_is_granted_as_its_entries_and_the_deny_stays() {
        let kept_out = exclusions(
            &[
                r"\\?\C:\Users\R\.cargo\credentials",
                r"\\?\C:\Users\R\.cargo\credentials.toml",
            ],
            &[],
        );
        let denied = [
            r"C:\Users\R\.cargo\credentials",
            r"C:\Users\R\.cargo\credentials.toml",
        ];
        let (files, notes) = windows(&[CARGO], &denied, &kept_out, &cargo_disk());
        assert_eq!(
            files["allowRead"],
            json!([
                r"C:\Users\R\.cargo\.crates.toml",
                r"C:\Users\R\.cargo\.crates2.json",
                r"C:\Users\R\.cargo\bin",
                r"C:\Users\R\.cargo\env",
                r"C:\Users\R\.cargo\git",
                r"C:\Users\R\.cargo\registry",
            ])
        );
        assert_eq!(files["denyRead"], json!(denied));
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn an_exclusion_two_levels_down_splits_only_the_folders_above_it() {
        let disk = Disk::new(&[
            (r"C:\Users\R\tools", &["a", "c.txt", "d"]),
            (r"C:\Users\R\tools\a", &["b", "x"]),
            (r"C:\Users\R\tools\a\b", &["secret", "ok"]),
        ]);
        let kept_out = exclusions(&[r"C:\Users\R\tools\a\b\secret"], &[]);
        let (files, notes) = windows(&[r"C:\Users\R\tools"], &[], &kept_out, &disk);
        assert_eq!(
            files["allowRead"],
            json!([
                r"C:\Users\R\tools\a\b\ok",
                r"C:\Users\R\tools\a\x",
                r"C:\Users\R\tools\c.txt",
                r"C:\Users\R\tools\d",
            ])
        );
        assert!(notes.is_empty(), "{notes:?}");
        // `d` and `ok` hold no exclusion, so neither was opened.
        assert_eq!(
            *disk.listed.borrow(),
            [
                r"C:\Users\R\tools",
                r"C:\Users\R\tools\a",
                r"C:\Users\R\tools\a\b"
            ]
        );
    }

    #[test]
    fn a_wildcard_exclusion_splits_a_folder_only_where_an_entry_beneath_matches() {
        let disk = Disk::new(&[
            (r"C:\Users\R\apps", &["one", "two", "plain.txt"]),
            (r"C:\Users\R\apps\one", &["EBWebView", "cache"]),
            (r"C:\Users\R\apps\two", &["cache"]),
        ]);
        let kept_out = exclusions(&[r"C:\Users\R\apps\*\EBWebView"], &[]);
        let (files, notes) = windows(&[r"C:\Users\R\apps"], &[], &kept_out, &disk);
        assert_eq!(
            files["allowRead"],
            json!([
                r"C:\Users\R\apps\one\cache",
                r"C:\Users\R\apps\plain.txt",
                r"C:\Users\R\apps\two",
            ])
        );
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_wildcard_exclusion_with_nothing_beneath_that_matches_leaves_the_folder_whole() {
        let disk = Disk::new(&[
            (r"C:\Users\R\apps", &["two"]),
            (r"C:\Users\R\apps\two", &["cache"]),
        ]);
        let kept_out = exclusions(&[r"C:\Users\R\apps\*\EBWebView"], &[]);
        let (files, _) = windows(&[r"C:\Users\R\apps"], &[], &kept_out, &disk);
        assert_eq!(files["allowRead"], json!([r"C:\Users\R\apps"]));
    }

    #[test]
    fn a_grant_equal_to_or_inside_an_exclusion_is_left_out_with_a_note_naming_it() {
        let kept_out = exclusions(
            &[r"\\?\C:\Users\R\.ssh", r"C:\Users\R\NTUSER.DAT*"],
            &[r"C:\Users\R\AppData\Local\Temp"],
        );
        let read = [
            r"C:\Users\R\.ssh",
            r"C:\Users\R\.ssh\keys",
            r"C:\Users\R\ntuser.dat.LOG1",
            r"C:\Users\R\AppData\Local\Temp",
            // Inside an ungranted-only directory is fine.
            r"C:\Users\R\AppData\Local\Temp\build",
            r"C:\Users\R\src",
        ];
        let (files, notes) = windows(&read, &[], &kept_out, &Disk::new(&[]));
        assert_eq!(
            files["allowRead"],
            json!([r"C:\Users\R\AppData\Local\Temp\build", r"C:\Users\R\src"])
        );
        let said = |path: &str, exclusion: &str| {
            let found = notes
                .iter()
                .filter(|n| n.contains(path) && n.contains(exclusion));
            assert_eq!(found.count(), 1, "{path}: {notes:?}");
        };
        said(r"C:\Users\R\.ssh is", r"C:\Users\R\.ssh");
        said(r"C:\Users\R\.ssh\keys", r"C:\Users\R\.ssh");
        said(r"ntuser.dat.LOG1", r"NTUSER.DAT*");
        said(r"AppData\Local\Temp is", r"AppData\Local\Temp");
        assert_eq!(notes.len(), 4, "{notes:?}");
    }

    #[test]
    fn a_grant_above_an_ungranted_only_directory_is_split_around_it() {
        let disk = Disk::new(&[(r"C:\Users\R\AppData\Local", &["Temp", "Programs"])]);
        let kept_out = exclusions(&[], &[r"C:\Users\R\AppData\Local\Temp"]);
        let (files, _) = windows(&[r"C:\Users\R\AppData\Local"], &[], &kept_out, &disk);
        assert_eq!(
            files["allowRead"],
            json!([r"C:\Users\R\AppData\Local\Programs"])
        );
    }

    #[test]
    fn a_read_folder_above_no_exclusion_stays_whole_and_is_not_opened() {
        let disk = cargo_disk();
        let kept_out = exclusions(&[r"\\?\C:\Users\R\.ssh"], &[]);
        let (files, notes) = windows(&[CARGO, r"C:\work\wt"], &[], &kept_out, &disk);
        assert_eq!(files["allowRead"], json!([CARGO, r"C:\work\wt"]));
        assert!(notes.is_empty(), "{notes:?}");
        assert!(disk.listed.borrow().is_empty());
    }

    #[test]
    fn a_folder_that_cannot_be_listed_is_left_out_with_a_note() {
        let kept_out = exclusions(&[r"C:\Users\R\.cargo\credentials"], &[]);
        let (files, notes) = windows(&[CARGO], &[], &kept_out, &Disk::new(&[]));
        assert_eq!(files["allowRead"], json!([]));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("cannot be listed"), "{notes:?}");
    }

    #[test]
    fn macos_and_linux_keep_the_folder_whole_with_the_deny_beside_it() {
        let kept_out = exclusions(&[r"\\?\C:\Users\R\.cargo\credentials"], &[]);
        let policy = Policy {
            read: vec![PathBuf::from("/Users/me/.cargo")],
            no_read: vec!["/Users/me/.cargo/credentials".into()],
            ..Policy::default()
        };
        let disk = cargo_disk();
        let profile = Profile {
            exclusions: &kept_out,
            entries: &|path| disk.list(path),
        };
        for platform in [Platform::Macos, Platform::Linux] {
            let (settings, notes) =
                settings_in(&policy, platform, false, None, &|_: &Path| true, &profile);
            let files = &settings["filesystem"];
            assert_eq!(
                files["allowRead"],
                json!(["/Users/me/.cargo"]),
                "{platform:?}"
            );
            assert_eq!(
                files["denyRead"],
                json!(["/Users/me/.cargo/credentials"]),
                "{platform:?}"
            );
            assert!(notes.is_empty(), "{notes:?}");
        }
        assert!(disk.listed.borrow().is_empty());
    }

    // The wrapped command's settings are written from the install's list, so a
    // list that drifted from crabpot's is the install's to correct. A Unix test
    // cannot list `C:\...`, so the folder above the exclusion is left out.
    #[test]
    fn the_installs_exclusions_are_what_the_written_settings_are_cut_around() {
        use super::super::SandboxRuntime;
        use crate::adapters::host::WindowsInstall;
        use crate::ports::Sandbox;

        let dir = tempfile::tempdir().unwrap();
        let policy = Policy {
            read: vec![PathBuf::from(CARGO), PathBuf::from(r"C:\work\wt")],
            ..Policy::default()
        };
        let settings = dir.path().join("call.srt.json");
        let install = |kept_out: ProfileExclusions| {
            SandboxRuntime::new()
                .on(Platform::Windows)
                .with_install(WindowsInstall::provisioned(false).excluding(kept_out))
        };
        let command = crate::spawn::command("claude");

        let none = install(ProfileExclusions::default());
        none.wrap(&policy, &settings, &command).unwrap();
        let written = |settings: &Path| -> Value {
            serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap()
        };
        assert_eq!(
            written(&settings)["filesystem"]["allowRead"],
            json!([CARGO, r"C:\work\wt"])
        );

        let credentials = exclusions(&[r"\\?\C:\Users\R\.cargo\credentials"], &[]);
        install(credentials)
            .wrap(&policy, &settings, &command)
            .unwrap();
        assert_eq!(
            written(&settings)["filesystem"]["allowRead"],
            json!([r"C:\work\wt"])
        );
    }
}
