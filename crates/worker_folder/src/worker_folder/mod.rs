//! Reading and writing files in a folder a worker can write
//!
//! A worker can plant a link anywhere in its folders, and a path opened the
//! ordinary way follows it to any file the maintainer can use. So kelpie
//! reads and writes there only through here, which follows no link.
//!
//! - A read opens with `O_NOFOLLOW` and takes a regular file of at most [`MAX`] bytes.
//! - A write makes a new file with `O_EXCL` beside the file, then renames it over.
//! - Each folder of a path is opened with `O_NOFOLLOW` and `O_DIRECTORY`.
//! - On Windows folders are held open (`Held`); a link count and the owner are not checked.

use std::fmt;
use std::io;
use std::path::Path;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;
#[cfg(windows)]
pub use windows::Held;

/// The most a file read from a worker's folder may hold, in bytes
pub const MAX: u64 = 1 << 20;

/// Why a file in a worker's folder was not read or written
///
/// None of its text holds a file's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// It is a link or hard link, a FIFO, a device, a folder, or owned by someone else
    NotRegular,
    /// A folder on the way to it is a link or not a folder
    NotFolder,
    /// It holds more than [`MAX`] bytes
    TooBig,
    /// The system refused for this reason
    Io(io::ErrorKind),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegular => f.write_str("it is not a regular file, so kelpie leaves it"),
            Self::NotFolder => f.write_str("a folder on its path is not a plain folder"),
            Self::TooBig => write!(f, "it is over {MAX} bytes"),
            Self::Io(kind) => write!(f, "{kind}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(kind) => kind.into(),
            e => Self::other(e.to_string()),
        }
    }
}

/// The text of `rel` below `root`, or none when it, or a folder on the way to
/// it, is not there
///
/// # Errors
///
/// [`Error::NotRegular`] or [`Error::NotFolder`] for a link or anything else
/// that is not a plain file in plain folders, [`Error::TooBig`] for a file over
/// [`MAX`], and [`Error::Io`] for the rest, which includes text that is not
/// UTF-8.
pub fn read(root: &Path, rel: &Path) -> Result<Option<String>, Error> {
    sys::read(root, rel)
}

/// Writes `text` to `rel` below `root`, making the root and the folders on the
/// way, and replacing a file or link there
///
/// # Errors
///
/// [`Error::NotFolder`] for a link or a file where a folder should be, and
/// [`Error::Io`] for the rest.
pub fn write(root: &Path, rel: &Path, text: &str) -> Result<(), Error> {
    sys::write(root, rel, text, false)
}

/// As [`write()`], for the user alone, whatever mode a file there had
///
/// # Errors
///
/// As [`write()`].
pub fn write_private(root: &Path, rel: &Path, text: &str) -> Result<(), Error> {
    sys::write(root, rel, text, true)
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    // A build folder, and beside it a file that is none of the worker's
    fn folders() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (build, secret) = (dir.path().join("build"), dir.path().join("secret"));
        fs::create_dir(&build).unwrap();
        fs::write(&secret, "key\n").unwrap();
        (dir, build, secret)
    }

    // The call, on a thread, so a read that waits fails the test rather than hang it
    fn within_a_second<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || tx.send(call()));
        rx.recv_timeout(Duration::from_secs(1))
            .expect("the call hung")
    }

    #[test]
    fn a_plain_file_is_read_and_a_missing_one_is_none() {
        let (_dir, build, _) = folders();
        fs::write(build.join("a.md"), "text\n").unwrap();

        assert_eq!(read(&build, Path::new("a.md")), Ok(Some("text\n".into())));
        assert_eq!(read(&build, Path::new("b.md")), Ok(None));
        assert_eq!(read(&build, Path::new("gone/b.md")), Ok(None));
        assert_eq!(read(&build.join("gone"), Path::new("b.md")), Ok(None));
    }

    #[test]
    fn a_link_to_a_file_outside_the_folder_is_refused_without_its_content() {
        let (_dir, build, secret) = folders();
        symlink(&secret, build.join("a.md")).unwrap();

        let refused = read(&build, Path::new("a.md"));

        assert_eq!(refused, Err(Error::NotRegular));
        assert!(!refused.unwrap_err().to_string().contains("key"));
    }

    // A hard link is a regular file with the user's name on it, which O_NOFOLLOW
    // does not see.
    #[test]
    fn a_hard_link_to_a_file_outside_the_folder_is_refused_without_its_content() {
        let (_dir, build, secret) = folders();
        fs::hard_link(&secret, build.join("a.md")).unwrap();

        let refused = read(&build, Path::new("a.md"));

        assert_eq!(refused, Err(Error::NotRegular));
        assert!(!refused.unwrap_err().to_string().contains("key"));
    }

    #[test]
    fn a_written_file_has_the_mode_an_ordinary_write_gives_under_the_same_umask() {
        let (_dir, build, _) = folders();
        fs::write(build.join("ordinary.md"), "x").unwrap();

        write(&build, Path::new("a.md"), "x").unwrap();

        let mode = |name: &str| fs::metadata(build.join(name)).unwrap().permissions().mode();
        assert_eq!(mode("a.md"), mode("ordinary.md"));
    }

    #[test]
    fn a_link_to_a_missing_file_is_refused_too() {
        let (dir, build, _) = folders();
        symlink(dir.path().join("nothing"), build.join("a.md")).unwrap();

        assert_eq!(read(&build, Path::new("a.md")), Err(Error::NotRegular));
    }

    #[test]
    #[cfg_attr(windows, ignore = "a FIFO is a Unix file")]
    fn a_fifo_is_refused_and_not_waited_on() {
        let (_dir, build, _) = folders();
        let made = Command::new("mkfifo").arg(build.join("a.md")).status();
        assert!(made.unwrap().success());

        let refused = within_a_second(move || read(&build, Path::new("a.md")));

        assert_eq!(refused, Err(Error::NotRegular));
    }

    #[test]
    fn a_folder_is_refused_as_a_file() {
        let (_dir, build, _) = folders();
        fs::create_dir(build.join("a.md")).unwrap();

        assert_eq!(read(&build, Path::new("a.md")), Err(Error::NotRegular));
    }

    #[test]
    fn a_file_over_the_cap_is_refused_and_one_at_it_is_read() {
        let (_dir, build, _) = folders();
        let at = "x".repeat(usize::try_from(MAX).unwrap());
        fs::write(build.join("at.md"), &at).unwrap();
        fs::write(build.join("over.md"), format!("{at}x")).unwrap();

        assert_eq!(read(&build, Path::new("at.md")), Ok(Some(at)));
        assert_eq!(read(&build, Path::new("over.md")), Err(Error::TooBig));
    }

    #[test]
    fn a_folder_that_is_a_link_is_not_followed_to_read_or_write() {
        let (dir, build, _) = folders();
        let theirs = dir.path().join("theirs");
        fs::create_dir(&theirs).unwrap();
        fs::write(theirs.join("hosts.yml"), "theirs\n").unwrap();
        symlink(&theirs, build.join("gh")).unwrap();
        let file = Path::new("gh/hosts.yml");

        assert_eq!(read(&build, file), Err(Error::NotFolder));
        assert_eq!(write(&build, file, "mine\n"), Err(Error::NotFolder));
        assert_eq!(write_private(&build, file, "mine\n"), Err(Error::NotFolder));

        assert_eq!(
            fs::read_to_string(theirs.join("hosts.yml")).unwrap(),
            "theirs\n"
        );
        assert_eq!(fs::read_dir(&theirs).unwrap().count(), 1);
    }

    #[test]
    fn a_write_replaces_a_link_and_leaves_what_it_names_alone() {
        let (_dir, build, secret) = folders();
        symlink(&secret, build.join("a.md")).unwrap();

        write(&build, Path::new("a.md"), "mine\n").unwrap();

        assert_eq!(fs::read_to_string(&secret).unwrap(), "key\n");
        assert!(
            !fs::symlink_metadata(build.join("a.md"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(build.join("a.md")).unwrap(), "mine\n");
    }

    #[test]
    fn a_link_planted_where_the_write_stages_its_file_is_not_written_through() {
        let (_dir, build, secret) = folders();
        symlink(&secret, build.join("a.md.tmp")).unwrap();

        write(&build, Path::new("a.md"), "mine\n").unwrap();

        assert_eq!(fs::read_to_string(&secret).unwrap(), "key\n");
        assert_eq!(fs::read_to_string(build.join("a.md")).unwrap(), "mine\n");
        assert!(!build.join("a.md.tmp").exists());
    }

    #[test]
    fn a_staged_file_a_crash_left_does_not_stop_the_next_write() {
        let (_dir, build, _) = folders();
        fs::write(build.join("a.md.tmp"), "half").unwrap();

        write(&build, Path::new("a.md"), "whole\n").unwrap();

        assert_eq!(fs::read_to_string(build.join("a.md")).unwrap(), "whole\n");
    }

    #[test]
    fn a_write_makes_the_folders_on_its_way_and_the_private_one_is_for_the_user_alone() {
        let (_dir, build, _) = folders();
        let file = Path::new("gh/deep/hosts.yml");
        write_private(&build, file, "first\n").unwrap();
        fs::set_permissions(build.join(file), fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&build, file, "second\n").unwrap();

        assert_eq!(fs::read_to_string(build.join(file)).unwrap(), "second\n");
        let mode = fs::metadata(build.join(file)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_folder_is_refused() {
        let (_dir, build, _) = folders();

        assert!(read(&build, Path::new("../secret")).is_err());
        assert!(write(&build, Path::new("/etc/x"), "").is_err());
        assert!(write(&build, Path::new(""), "").is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests;
