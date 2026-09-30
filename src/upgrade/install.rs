//! The installed kelpie under kelpie's home, and the build kept beside it
//!
//! Every file is put in place by a rename, so a runner that restarts on its
//! own, or the maintainer's `shep kelpie`, finds the old build or the new
//! one and never half of either. The previous build is a hard link to the
//! inode it had, not a copy: a copy made while the build runs is a new file
//! macOS has not vetted.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the installed build and its predecessor live
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    bin: PathBuf,
}

impl Layout {
    /// The layout under kelpie's home
    pub fn under(kelpie_home: &Path) -> Self {
        Self {
            bin: kelpie_home.join("bin"),
        }
    }

    /// The binary every kelpie sheep runs
    pub fn installed(&self) -> PathBuf {
        self.bin.join("shep-kelpie")
    }

    /// The build the last upgrade replaced
    pub fn previous(&self) -> PathBuf {
        self.bin.join("shep-kelpie.previous")
    }

    fn staged(&self) -> PathBuf {
        self.bin.join(".shep-kelpie.staged")
    }

    fn held(&self) -> PathBuf {
        self.bin.join(".shep-kelpie.held")
    }
}

/// A build copied next to the installed one, ready to rename into place
///
/// Dropped without being installed, it removes itself.
#[derive(Debug)]
pub struct Staged {
    path: PathBuf,
    live: bool,
}

impl Staged {
    /// Where the copy is
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.live {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// What [`install`] did
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// There was no installed build, and now there is
    First,
    /// The installed build became the previous one
    Replaced,
    /// The installed build is this build already, and nothing moved
    Unchanged,
}

/// Copies the build at `source` into kelpie's `bin` folder, executable and signed
///
/// # Errors
///
/// A message when the copy, its mode or its signature fails.
pub fn stage(layout: &Layout, source: &Path) -> Result<Staged, String> {
    let path = layout.staged();
    let failed = |what: &str, e: &dyn core::fmt::Display| {
        format!("cannot {what} {}: {e}", layout.staged().display())
    };
    fs::create_dir_all(&layout.bin).map_err(|e| failed("make the folder of", &e))?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(failed("clear", &e)),
    }
    fs::copy(source, &path).map_err(|e| {
        format!(
            "cannot copy {} to {}: {e}",
            source.display(),
            path.display()
        )
    })?;
    let staged = Staged { path, live: true };
    fs::set_permissions(&staged.path, fs::Permissions::from_mode(0o755))
        .map_err(|e| failed("make executable", &e))?;
    sign(&staged.path)?;
    Ok(staged)
}

/// Makes `staged` the installed build, keeping the one it replaces as the previous
///
/// A build byte for byte the installed one changes nothing, so running an
/// upgrade again to finish its restarts does not push the real previous
/// build out.
///
/// # Errors
///
/// A message when a file cannot be read, linked or renamed.
pub fn install(layout: &Layout, mut staged: Staged) -> Result<Change, String> {
    let installed = layout.installed();
    let change = if installed.exists() {
        if same_bytes(&installed, &staged.path)? {
            return Ok(Change::Unchanged);
        }
        link_as(&installed, &layout.held(), &layout.previous())?;
        Change::Replaced
    } else {
        Change::First
    };
    fs::rename(&staged.path, &installed)
        .map_err(|e| format!("cannot install {}: {e}", installed.display()))?;
    staged.live = false;
    Ok(change)
}

/// Puts the previous build back as the installed one, and the installed one
/// in its place, so a second rollback undoes the first
///
/// # Errors
///
/// A message when there is no previous build, or a file cannot be moved.
pub fn swap_with_previous(layout: &Layout) -> Result<(), String> {
    let (installed, previous) = (layout.installed(), layout.previous());
    if !previous.is_file() {
        return Err(format!(
            "there is no previous build to put back: {} is missing",
            previous.display()
        ));
    }
    let held = layout.held();
    if installed.exists() {
        link_as(&installed, &held, &held)?;
    }
    fs::rename(&previous, &installed)
        .map_err(|e| format!("cannot put back {}: {e}", installed.display()))?;
    if held.exists() {
        fs::rename(&held, &previous)
            .map_err(|e| format!("cannot keep the build this replaced: {e}"))?;
    }
    Ok(())
}

// Makes `to` another name for `from`'s file, through `via` so `to` is never absent.
fn link_as(from: &Path, via: &Path, to: &Path) -> Result<(), String> {
    match fs::remove_file(via) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot clear {}: {e}", via.display())),
    }
    fs::hard_link(from, via)
        .map_err(|e| format!("cannot keep {} as {}: {e}", from.display(), via.display()))?;
    if via != to {
        fs::rename(via, to).map_err(|e| format!("cannot keep the previous build: {e}"))?;
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> Result<bool, String> {
    let read = |p: &Path| fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
    Ok(read(a)? == read(b)?)
}

// A build that has moved on macOS must be signed again, or the system kills it
// on launch. Only a Mach-O binary takes a signature: a script stands in fine.
fn sign(path: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") || !is_mach_o(path) {
        return Ok(());
    }
    let output = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run codesign: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "codesign refused {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn is_mach_o(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    let read = fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic));
    read.is_ok()
        && matches!(
            u32::from_be_bytes(magic),
            0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe | 0xbeba_feca
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn only_a_mach_o_binary_takes_a_signature() {
        let dir = tempfile::tempdir().unwrap();
        for magic in [[0xcf, 0xfa, 0xed, 0xfe], [0xca, 0xfe, 0xba, 0xbe]] {
            assert!(is_mach_o(&file(dir.path(), "m", &magic)));
        }
        assert!(!is_mach_o(&file(dir.path(), "s", b"#!/bin/sh\n")));
        assert!(!is_mach_o(&file(dir.path(), "short", b"ab")));
        assert!(!is_mach_o(&dir.path().join("missing")));
    }

    #[test]
    fn a_staged_copy_is_executable_and_a_dropped_one_goes() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::under(dir.path());
        let source = file(dir.path(), "src", b"#!/bin/sh\n");
        let staged = stage(&layout, &source).unwrap();
        let path = staged.path().to_owned();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
        drop(staged);
        assert!(!path.exists());
    }

    // The test binary is a real executable, so this signs on macOS and is a copy elsewhere.
    #[test]
    fn a_real_executable_is_staged_and_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::under(dir.path());
        let me = std::env::current_exe().unwrap();
        let staged = stage(&layout, &me).unwrap();
        let ran = Command::new(staged.path()).arg("--list").output().unwrap();
        assert!(ran.status.success(), "{ran:?}");
    }

    #[test]
    fn installing_keeps_the_replaced_build_and_swapping_trades_places() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::under(dir.path());
        let build = |bytes: &[u8]| {
            let source = file(dir.path(), "src", bytes);
            stage(&layout, &source).unwrap()
        };
        let read = |p: PathBuf| fs::read_to_string(p).unwrap();

        assert_eq!(install(&layout, build(b"one")), Ok(Change::First));
        assert_eq!(install(&layout, build(b"two")), Ok(Change::Replaced));
        assert_eq!(install(&layout, build(b"two")), Ok(Change::Unchanged));
        assert_eq!(read(layout.installed()), "two");
        assert_eq!(read(layout.previous()), "one");

        swap_with_previous(&layout).unwrap();
        assert_eq!(read(layout.installed()), "one");
        assert_eq!(read(layout.previous()), "two");
        assert!(!layout.staged().exists() && !layout.held().exists());
    }

    #[test]
    fn a_swap_with_no_previous_build_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = swap_with_previous(&Layout::under(dir.path())).unwrap_err();
        assert!(err.contains("no previous build"), "{err}");
    }
}
