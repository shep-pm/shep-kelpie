//! A path's real spelling in a form another program can open
//!
//! `Path::canonicalize` on Windows gives a verbatim path (`\\?\C:\...`),
//! which Git for Windows and most other programs refuse. [`canonical`]
//! drops the prefix where the path has a plain form.

use std::path::{Path, PathBuf};

/// The longest plain path Windows opens without the verbatim prefix
const MAX_PLAIN: usize = 259;

/// Device names Windows reads as the device in any plain path
const DEVICES: [&str; 30] = [
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "CONIN$",
    "CONOUT$",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

/// `path` made absolute with its links resolved, as a path other programs
/// can open
///
/// On Windows a `\\?\C:\` or `\\?\UNC\server\share\` prefix is dropped when
/// the path has a plain form. On other platforms this is
/// [`Path::canonicalize`].
///
/// # Errors
///
/// [`std::io::Error`] when `path` does not exist or cannot be resolved.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let real = path.canonicalize()?;
    if cfg!(windows)
        && let Some(plain) = real.to_str().and_then(plain)
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(real)
}

/// The plain spelling of the verbatim path `raw`, when it has one
///
/// `None` when `raw` is not verbatim, or when a component, or the length,
/// needs the prefix: Windows reads a plain path's trailing dot or space,
/// `.` and `..`, device names and `<>:"|?*` differently.
fn plain(raw: &str) -> Option<String> {
    let (head, body, least) = match raw.strip_prefix(r"\\?\UNC\") {
        Some(share) => (r"\\", share, 2),
        None => {
            let drive = raw.strip_prefix(r"\\?\")?;
            let bytes = drive.as_bytes();
            if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
                return None;
            }
            drive
                .split_at_checked(3)
                .map(|(head, body)| (head, body, 0))?
        }
    };
    let parts: Vec<&str> = body.split('\\').filter(|_| !body.is_empty()).collect();
    if parts.len() < least || !parts.into_iter().all(plain_part) {
        return None;
    }
    let spelled = format!("{head}{body}");
    (spelled.len() <= MAX_PLAIN).then_some(spelled)
}

fn plain_part(part: &str) -> bool {
    // Windows drops the spaces before the extension's dot: `nul .txt` is `NUL`.
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part.ends_with(['.', ' '])
        && !part.contains(['<', '>', ':', '"', '/', '|', '?', '*'])
        && !part.chars().any(char::is_control)
        && !DEVICES
            .iter()
            .any(|device| stem.eq_ignore_ascii_case(device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_path_loses_its_prefix() {
        assert_eq!(
            plain(r"\\?\C:\crabpot-smoke\work\.git\worktrees\22").as_deref(),
            Some(r"C:\crabpot-smoke\work\.git\worktrees\22"),
        );
        assert_eq!(plain(r"\\?\C:\").as_deref(), Some(r"C:\"));
    }

    #[test]
    fn a_unc_path_becomes_a_share_path() {
        assert_eq!(
            plain(r"\\?\UNC\server\share\repo\.git").as_deref(),
            Some(r"\\server\share\repo\.git"),
        );
    }

    #[test]
    fn a_path_that_needs_the_prefix_keeps_it() {
        for kept in [
            r"\\?\C:\work\trailing.\x",
            r"\\?\C:\work\trailing \x",
            r"\\?\C:\work\nul\x",
            r"\\?\C:\work\com1.txt",
            r"\\?\C:\work\nul .txt",
            "\\\\?\\C:\\work\\COM\u{b9}\\x",
            "\\\\?\\C:\\work\\lpt\u{b3}.log",
            r"\\?\C:\work\CONIN$",
            r"\\?\C:\work\conout$.txt",
            r"\\?\C:\work\..\x",
            r"\\?\C:\work\a:b",
            r"\\?\UNC\server",
            r"\\?\Volume{0d5f}\work",
            r"\\?\GLOBALROOT\Device\x",
            r"\\?\1:\work",
            r"\\?\C:work",
        ] {
            assert_eq!(plain(kept), None, "{kept}");
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(plain(&long), None);
    }

    #[test]
    fn a_path_that_is_not_verbatim_is_left_alone() {
        for other in [r"C:\work", r"\\server\share\x", "/tmp/work", "relative/x"] {
            assert_eq!(plain(other), None, "{other}");
        }
    }

    #[test]
    fn an_existing_folder_comes_back_as_canonicalize_gives_it() {
        let dir = tempfile::tempdir().unwrap();
        let real = canonical(dir.path()).unwrap();
        assert!(real.is_absolute());
        assert!(!real.to_string_lossy().starts_with(r"\\?\"));
        #[cfg(not(windows))]
        assert_eq!(real, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn a_missing_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(canonical(&dir.path().join("missing")).is_err());
    }
}
