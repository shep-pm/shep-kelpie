//! Kelpie's home: `$SHEP_HOME/kelpie`, or the folder `KELPIE_HOME` names
//!
//! Kelpie is a plugin of shep, so what it keeps lives under the shepherd's
//! home: its own files at the top, and each project's in a folder named for
//! it, with the project's worktrees and build folders inside. The dog always
//! keeps its book and door under `$SHEP_HOME/kelpie/dog`, since shep starts
//! the adopted dog with `SHEP_HOME` and without `KELPIE_HOME`.

pub mod migrate;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The variable that names kelpie's home in place of the shepherd's
pub const KELPIE_VAR: &str = "KELPIE_HOME";

/// Kelpie's folder under the shepherd's home
const UNDER_SHEP: &str = "kelpie";

// shep's own default when `SHEP_HOME` is unset (shep-core's `DEFAULT_HOME_DIR`).
const SHEP_DEFAULT: &str = ".shep";

/// The folder kelpie kept everything in before it moved under shep's home
pub const OLD: &str = ".kelpie";

/// Kelpie's own entries in its home, and the old layout's that a home kept
/// in place still has, which no project may be named for
pub const OWN: [&str; 15] = [
    "builds",
    "codex",
    "dog",
    "playwright",
    "projects",
    "relay",
    "rulings",
    "settings.toml",
    "shots",
    "targets",
    "tools",
    "totp",
    "upgrade",
    "upgrade.lock",
    "wt",
];

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

/// The home kelpie used before, whose files [`migrate`] moves: the one
/// `KELPIE_HOME` names, else `~/.kelpie`
pub fn old_home() -> Option<PathBuf> {
    let set = std::env::var_os(KELPIE_VAR).filter(|v| !v.is_empty());
    set.map(PathBuf::from).or_else(|| {
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(Path::new(&home).join(OLD))
    })
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

/// `path` under kelpie's home, or where the old home kept it while no runner
/// has moved it yet: `old`, relative to the old home
pub fn or_old(path: PathBuf, old: &str) -> PathBuf {
    if path.exists() {
        return path;
    }
    old_home()
        .map(|home| home.join(old))
        .filter(|was| fs_is_file(was))
        .unwrap_or(path)
}

fn fs_is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
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
}
