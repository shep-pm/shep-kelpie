//! Kelpie's home: `$SHEP_HOME/kelpie`, or the folder `KELPIE_HOME` names
//!
//! Kelpie is a plugin of shep, so what it keeps lives under the shepherd's
//! home: its own files at the top, and each project's in a folder named for
//! it, with the project's worktrees and build folders inside. The dog always
//! keeps its book and door under `$SHEP_HOME/kelpie/dog`, since shep starts
//! the adopted dog with `SHEP_HOME` and without `KELPIE_HOME`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::runner::ProjectName;

/// The variable that names kelpie's home in place of the shepherd's
pub const KELPIE_VAR: &str = "KELPIE_HOME";

/// Kelpie's folder under the shepherd's home
const UNDER_SHEP: &str = "kelpie";

// shep's own default when `SHEP_HOME` is unset (shep-core's `DEFAULT_HOME_DIR`).
const SHEP_DEFAULT: &str = ".shep";

/// The folder kelpie kept everything in before it moved under shep's home
pub const OLD: &str = ".kelpie";

/// Kelpie's own entries in its home, which no project may be named for
pub const OWN: [&str; 11] = [
    "agents",
    "builds",
    "codex",
    "dog",
    "relay",
    "rulings",
    "settings.toml",
    "tools",
    "totp",
    "upgrade",
    "upgrade.lock",
];

/// Kelpie's shared files in the old home, which the first runner to start
/// on a build that moved homes took, leaving `.moved-to` behind
const OLD_SHARED: [&str; 6] = [
    "codex",
    "relay",
    "rulings",
    "settings.toml",
    "tools",
    "totp",
];

/// The folders of the old home that held a project's files, each in a
/// folder named for it, until its own runner's start moved them
const OLD_PROJECT: [&str; 5] = ["playwright", "projects", "shots", "targets", "wt"];

/// The dog's book in the old home, until the dog's start moved it
const OLD_BOOK: &str = "dog/book.json";

/// The file the old home keeps once a runner moved its shared files
const MOVED_TO: &str = ".moved-to";

/// What a shepherd keeps at the top of its home, so a folder holding one
/// is a shepherd's home and not kelpie's old one
const SHEPHERDS: [&str; 3] = ["flock.json", "shep.toml", "run"];

/// The longest path a Unix socket may have, one byte short of macOS's 104
pub const LONGEST_SOCKET: usize = 103;

/// Kelpie's home under the shepherd at `shep_home`
pub fn under(shep_home: &Path) -> PathBuf {
    shep_home.join(UNDER_SHEP)
}

/// The dog's folder, holding its book and its door, under the shepherd at `shep_home`
pub fn dog_folder(shep_home: &Path) -> PathBuf {
    under(shep_home).join("dog")
}

/// The shepherd's home: `SHEP_HOME`, or shep's own default `~/.shep`
///
/// # Errors
///
/// A message when `SHEP_HOME` is not an absolute path, or neither it nor `HOME` is set.
pub fn shep_home() -> Result<PathBuf, String> {
    shep_home_from(std::env::var_os("SHEP_HOME"), std::env::var_os("HOME"))
}

/// Kelpie's home: `KELPIE_HOME`, or `kelpie` under [`shep_home`]
///
/// # Errors
///
/// A message when neither can be worked out, as [`shep_home`] says.
pub fn kelpie_home() -> Result<PathBuf, String> {
    kelpie_home_from(
        std::env::var_os(KELPIE_VAR),
        std::env::var_os("SHEP_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Kelpie's home for the shepherd at `shep_home`: `KELPIE_HOME`, or `kelpie` under it
pub fn kelpie_home_of(shep_home: &Path) -> PathBuf {
    std::env::var_os(KELPIE_VAR)
        .filter(|v| !v.is_empty())
        .map_or_else(|| under(shep_home), PathBuf::from)
}

/// The old home a runner's files would still be in, `~/.kelpie`, when
/// `KELPIE_HOME` does not name its home instead
pub fn old_home() -> Option<PathBuf> {
    if std::env::var_os(KELPIE_VAR).is_some_and(|v| !v.is_empty()) {
        return None;
    }
    old_home_of(std::env::var_os("HOME"))
}

/// The old home the dog's book would still be in, `~/.kelpie`, since shep
/// never hands the adopted dog `KELPIE_HOME`
pub fn old_dog_home() -> Option<PathBuf> {
    old_home_of(std::env::var_os("HOME"))
}

/// Refuses project `name`'s runner a start in `home` while the old home
/// `old` still holds the project's files, or kelpie's shared files with
/// no runner having moved them
///
/// # Errors
///
/// A message naming the old home and what it holds, and how to move it.
pub fn runner_may_start(old: &Path, home: &Path, name: &ProjectName) -> Result<(), String> {
    if shepherds(old) {
        return Ok(());
    }
    let own = OLD_PROJECT
        .iter()
        .map(|folder| Path::new(folder).join(name.as_str()))
        .find(|path| old.join(path).exists());
    let shared = || {
        let claimed = old.join(MOVED_TO).exists();
        let found = OLD_SHARED.iter().find(|file| kept(&old.join(file)));
        found.filter(|_| !claimed).map(PathBuf::from)
    };
    own.or_else(shared)
        .map_or(Ok(()), |found| Err(unmoved(old, &found, home)))
}

/// Refuses the dog a start in its folder `dog` while the old home `old`
/// still holds its book
///
/// # Errors
///
/// A message naming the old home and the book, and how to move it.
pub fn dog_may_start(old: &Path, dog: &Path) -> Result<(), String> {
    if shepherds(old) || !kept(&old.join(OLD_BOOK)) {
        return Ok(());
    }
    Err(unmoved(old, Path::new(OLD_BOOK), dog))
}

fn old_home_of(home: Option<OsString>) -> Option<PathBuf> {
    let home = home.filter(|v| !v.is_empty())?;
    Some(Path::new(&home).join(OLD))
}

// Whether `old` is a shepherd's home, which kelpie never kept its files in.
fn shepherds(old: &Path) -> bool {
    SHEPHERDS.iter().any(|file| old.join(file).exists())
}

// Whether `path` is there and is not a link a move left.
fn kept(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_symlink())
}

fn unmoved(old: &Path, found: &Path, home: &Path) -> String {
    format!(
        "{} still holds {}, from before kelpie's home moved to {}: run the previous \
         shep-kelpie release once, which moves it, or move it there by hand",
        old.display(),
        found.display(),
        home.display()
    )
}

fn shep_home_from(shep: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
    match (
        shep.filter(|v| !v.is_empty()),
        home.filter(|v| !v.is_empty()),
    ) {
        (Some(shep), _) if Path::new(&shep).is_absolute() => Ok(shep.into()),
        (Some(shep), _) => Err(format!(
            "SHEP_HOME is {} and must be an absolute path",
            Path::new(&shep).display()
        )),
        (None, Some(home)) => Ok(Path::new(&home).join(SHEP_DEFAULT)),
        (None, None) => Err("neither SHEP_HOME nor HOME is set".into()),
    }
}

fn kelpie_home_from(
    kelpie: Option<OsString>,
    shep: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, String> {
    match kelpie.filter(|v| !v.is_empty()) {
        Some(kelpie) => Ok(kelpie.into()),
        None => shep_home_from(shep, home).map(|shep| under(&shep)),
    }
}

/// Whether `socket` is short enough to bind, naming it and the fix when not
///
/// # Errors
///
/// A message naming the path, its length and the limit.
pub fn socket_fits(socket: &Path) -> Result<(), String> {
    let length = socket.as_os_str().len();
    if length <= LONGEST_SOCKET {
        return Ok(());
    }
    Err(format!(
        "{} is {length} bytes, longer than the {LONGEST_SOCKET} a socket's path may be: \
         give the shepherd a shorter SHEP_HOME, or kelpie a shorter {KELPIE_VAR}",
        socket.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(s.into())
    }

    #[test]
    fn the_default_home_follows_shep_home() {
        let home = kelpie_home_from(None, os("/srv/shep"), os("/Users/me"));
        assert_eq!(home, Ok(PathBuf::from("/srv/shep/kelpie")));
        assert_eq!(
            dog_folder(Path::new("/srv/shep")),
            Path::new("/srv/shep/kelpie/dog")
        );
    }

    #[test]
    fn kelpie_home_overrides_the_shepherds() {
        let home = kelpie_home_from(os("/k"), os("/srv/shep"), os("/Users/me"));
        assert_eq!(home, Ok(PathBuf::from("/k")));
    }

    #[test]
    fn without_shep_home_it_is_under_sheps_own_default() {
        let home = kelpie_home_from(None, None, os("/Users/me"));
        assert_eq!(home, Ok(PathBuf::from("/Users/me/.shep/kelpie")));
        assert!(kelpie_home_from(None, os(""), os("")).is_err());
    }

    #[test]
    fn a_relative_shep_home_is_refused() {
        let error = kelpie_home_from(None, os("shep"), os("/Users/me")).unwrap_err();
        assert!(error.contains("absolute"), "{error}");
    }

    #[test]
    fn a_socket_past_the_limit_is_named() {
        let fits = format!("/{}", "a".repeat(LONGEST_SOCKET - 1));
        assert_eq!(socket_fits(Path::new(&fits)), Ok(()));
        let long = format!("{fits}b");
        let error = socket_fits(Path::new(&long)).unwrap_err();
        assert!(error.contains(&long), "{error}");
        assert!(error.contains("SHEP_HOME"), "{error}");
    }

    fn write(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn koji() -> ProjectName {
        ProjectName::try_from("koji").unwrap()
    }

    #[test]
    fn a_runner_beside_an_old_home_never_moved_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (old, home) = (root.path().join(".kelpie"), root.path().join("new"));
        assert_eq!(runner_may_start(&old, &home, &koji()), Ok(()));
        write(&old.join("totp/secret"));

        let error = runner_may_start(&old, &home, &koji()).unwrap_err();

        assert!(error.contains(&old.display().to_string()), "{error}");
        assert!(error.contains("totp"), "{error}");
        assert!(error.contains("previous shep-kelpie release"), "{error}");
        assert!(error.contains("by hand"), "{error}");
    }

    // A move left `.moved-to`, and links or copies of the shared files.
    #[test]
    fn a_moved_old_home_lets_a_runner_start_unless_its_project_stayed() {
        let root = tempfile::tempdir().unwrap();
        let (old, home) = (root.path().join(".kelpie"), root.path().join("new"));
        write(&old.join("settings.toml"));
        write(&old.join(MOVED_TO));
        std::os::unix::fs::symlink(home.join("totp"), old.join("totp")).unwrap();
        assert_eq!(runner_may_start(&old, &home, &koji()), Ok(()));

        write(&old.join("projects/koji/state.json"));

        let error = runner_may_start(&old, &home, &koji()).unwrap_err();
        assert!(error.contains("projects/koji"), "{error}");
        let lab = ProjectName::try_from("lab").unwrap();
        assert_eq!(runner_may_start(&old, &home, &lab), Ok(()));
    }

    #[test]
    fn a_shepherds_home_on_the_old_folder_is_not_an_unmoved_kelpie_home() {
        for marker in ["flock.json", "shep.toml", "run/shep.sock"] {
            let root = tempfile::tempdir().unwrap();
            let (old, home) = (root.path().join(".kelpie"), root.path().join("new"));
            write(&old.join("settings.toml"));
            write(&old.join("tools/srt"));
            write(&old.join("dog/book.json"));
            write(&old.join(marker));
            assert_eq!(runner_may_start(&old, &home, &koji()), Ok(()), "{marker}");
            assert_eq!(dog_may_start(&old, &home.join("dog")), Ok(()), "{marker}");
        }
    }

    #[test]
    fn a_dog_beside_its_old_book_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (old, dog) = (root.path().join(".kelpie"), root.path().join("new/dog"));
        write(&old.join("settings.toml"));
        assert_eq!(dog_may_start(&old, &dog), Ok(()));
        write(&old.join("dog/book.json"));

        let error = dog_may_start(&old, &dog).unwrap_err();

        assert!(error.contains(&old.display().to_string()), "{error}");
        assert!(error.contains("dog/book.json"), "{error}");
        assert!(error.contains(&dog.display().to_string()), "{error}");
    }

    #[test]
    fn the_old_home_is_under_home() {
        assert_eq!(
            old_home_of(os("/Users/me")),
            Some(PathBuf::from("/Users/me/.kelpie"))
        );
        assert_eq!(old_home_of(os("")), None);
    }
}
