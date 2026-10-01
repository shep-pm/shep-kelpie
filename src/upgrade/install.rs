//! Putting a build over the installed kelpie, the way shep upgrades itself
//!
//! The installed kelpie is the file the adopted dog runs, wherever that is.
//! The new build is written beside it and renamed over it, so the running
//! file's bytes are never edited in place, and a runner that restarts on its
//! own finds the old build or the new one and never half of either. A fresh
//! inode needs no re-signing by the system, so the copy is signed once, before
//! the rename.
//!
//! Before the swap the file it replaces is copied into kelpie's `builds`
//! folder, resolved if the installed path is a symlink, and `--rollback`
//! puts that copy back the same way.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the installed build lives, and where its predecessor is kept
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    installed: PathBuf,
    builds: PathBuf,
}

impl Layout {
    /// The layout for the kelpie at `installed`, keeping builds under `kelpie_home`
    pub fn new(kelpie_home: &Path, installed: &Path) -> Self {
        Self {
            installed: installed.to_owned(),
            builds: kelpie_home.join("builds"),
        }
    }

    /// The path every kelpie sheep runs, as the shepherd names it
    pub fn installed(&self) -> &Path {
        &self.installed
    }

    /// The build the last upgrade replaced
    pub fn previous(&self) -> PathBuf {
        self.builds.join("shep-kelpie.previous")
    }

    // The file the installed path leads to: what an upgrade replaces, so a
    // link stays a link.
    fn target(&self) -> PathBuf {
        fs::canonicalize(&self.installed).unwrap_or_else(|_| self.installed.clone())
    }

    // What a signature calls the build: the installed file's name, not the
    // staged copy's, so the same build signs to the same bytes every time.
    fn identifier(&self) -> String {
        self.target().file_name().map_or_else(
            || "shep-kelpie".into(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    fn staged(&self) -> PathBuf {
        let target = self.target();
        let name = target.file_name().map_or_else(
            || "shep-kelpie".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        // One per upgrade process, so two upgrades of one file never share it.
        target.with_file_name(format!(".{name}.staged.{}", std::process::id()))
    }

    fn held(&self) -> PathBuf {
        self.builds.join(".shep-kelpie.held")
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

/// Copies the build at `source` beside the installed kelpie, executable and signed
///
/// # Errors
///
/// A message when the copy, its mode or its signature fails.
pub fn stage(layout: &Layout, source: &Path) -> Result<Staged, String> {
    let path = layout.staged();
    let failed =
        |what: &str, e: &dyn core::fmt::Display| format!("cannot {what} {}: {e}", path.display());
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(|e| failed("make the folder of", &e))?;
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(failed("clear", &e)),
    }
    // The guard first, so a copy that fails half way leaves nothing behind.
    let staged = Staged {
        path: path.clone(),
        live: true,
    };
    fs::copy(source, &path).map_err(|e| {
        format!(
            "cannot copy {} to {}: {e}",
            source.display(),
            path.display()
        )
    })?;
    fs::set_permissions(&staged.path, fs::Permissions::from_mode(0o755))
        .map_err(|e| failed("make executable", &e))?;
    sign(&staged.path, &layout.identifier())?;
    flush(&staged.path)?;
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
/// A message when a file cannot be read, copied or renamed.
pub fn install(layout: &Layout, mut staged: Staged) -> Result<Change, String> {
    let target = layout.target();
    let change = if target.is_file() {
        if same_bytes(&target, &staged.path)? {
            return Ok(Change::Unchanged);
        }
        keep(layout, &target)?;
        Change::Replaced
    } else {
        Change::First
    };
    fs::rename(&staged.path, &target)
        .map_err(|e| format!("cannot install {}: {e}", target.display()))?;
    staged.live = false;
    Ok(change)
}

/// Stages the previous build, ready for [`install`], which keeps the installed
/// one in its place, so a second rollback undoes the first
///
/// # Errors
///
/// A message when there is no previous build, or it cannot be copied.
pub fn stage_previous(layout: &Layout) -> Result<Staged, String> {
    let previous = layout.previous();
    if !previous.is_file() {
        return Err(format!(
            "there is no previous build to put back: {} is missing",
            previous.display()
        ));
    }
    stage(layout, &previous)
}

// Copies the file at `target` into the builds folder as the previous build.
fn keep(layout: &Layout, target: &Path) -> Result<(), String> {
    let (held, previous) = (layout.held(), layout.previous());
    if let Some(folder) = held.parent() {
        fs::create_dir_all(folder).map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    }
    let kept = fs::copy(target, &held)
        .map_err(|e| {
            format!(
                "cannot keep {} as {}: {e}",
                target.display(),
                held.display()
            )
        })
        .and_then(|_| flush(&held))
        .and_then(|()| {
            fs::rename(&held, &previous).map_err(|e| {
                format!(
                    "cannot keep the previous build at {}: {e}",
                    previous.display()
                )
            })
        });
    if kept.is_err() {
        let _ = fs::remove_file(&held);
    }
    kept
}

// Writes the file to disk before a rename makes it the installed build, so a
// power loss soon after cannot leave an empty file under the name.
fn flush(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("cannot flush {}: {e}", path.display()))
}

fn same_bytes(a: &Path, b: &Path) -> Result<bool, String> {
    let read = |p: &Path| fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
    Ok(read(a)? == read(b)?)
}

// A build that has moved on macOS must be signed again, or the system kills it
// on launch. Only a Mach-O binary takes a signature: a script stands in fine.
fn sign(path: &Path, identifier: &str) -> Result<(), String> {
    if !cfg!(target_os = "macos") || !is_mach_o(path) {
        return Ok(());
    }
    let output = Command::new("codesign")
        .args(["--force", "--sign", "-", "--identifier", identifier])
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
    use std::os::unix::fs::MetadataExt;

    use super::*;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn layout(dir: &Path) -> Layout {
        fs::create_dir_all(dir.join("bin")).unwrap();
        Layout::new(&dir.join("home"), &dir.join("bin/shep-kelpie"))
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
    fn a_staged_copy_sits_beside_the_installed_file_and_a_dropped_one_goes() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let source = file(dir.path(), "src", b"#!/bin/sh\n");
        let staged = stage(&layout, &source).unwrap();
        let path = staged.path().to_owned();
        assert_eq!(path.parent(), layout.installed().parent());
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
        drop(staged);
        assert!(!path.exists());
    }

    // The test binary is a real executable, so this signs on macOS and is a copy elsewhere.
    #[test]
    fn a_real_executable_is_staged_and_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let me = std::env::current_exe().unwrap();
        let staged = stage(&layout, &me).unwrap();
        let ran = Command::new(staged.path()).arg("--list").output().unwrap();
        assert!(ran.status.success(), "{ran:?}");
    }

    // A signature names the file it signs unless told otherwise, and a staged
    // copy's name changes with the process: the same build must still sign to
    // the same bytes, or installing it again counts as a new build.
    #[test]
    fn the_same_build_signs_to_the_same_bytes_whatever_the_copy_is_called() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::env::current_exe().unwrap();
        let (one, two) = (
            dir.path().join(".x.staged.1"),
            dir.path().join(".x.staged.22"),
        );
        for copy in [&one, &two] {
            fs::copy(&me, copy).unwrap();
            sign(copy, "shep-kelpie").unwrap();
        }
        assert!(fs::read(&one).unwrap() == fs::read(&two).unwrap());
    }

    #[test]
    fn a_real_executable_installed_twice_is_unchanged_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let me = std::env::current_exe().unwrap();
        let first = stage(&layout, &me).unwrap();
        assert_eq!(install(&layout, first), Ok(Change::First));
        let again = stage(&layout, &me).unwrap();
        assert_eq!(install(&layout, again), Ok(Change::Unchanged));
    }

    #[test]
    fn installing_renames_a_fresh_file_over_the_old_one_and_keeps_it_in_builds() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let build = |bytes: &[u8]| {
            let source = file(dir.path(), "src", bytes);
            stage(&layout, &source).unwrap()
        };
        let read = |p: &Path| fs::read_to_string(p).unwrap();

        assert_eq!(install(&layout, build(b"one")), Ok(Change::First));
        let first = fs::metadata(layout.installed()).unwrap().ino();
        assert_eq!(install(&layout, build(b"two")), Ok(Change::Replaced));
        assert_ne!(
            fs::metadata(layout.installed()).unwrap().ino(),
            first,
            "the running file's bytes are not edited in place"
        );
        assert_eq!(install(&layout, build(b"two")), Ok(Change::Unchanged));
        assert_eq!(read(layout.installed()), "two");
        assert_eq!(read(&layout.previous()), "one");
        assert!(
            layout
                .previous()
                .starts_with(dir.path().join("home/builds"))
        );

        let staged = stage_previous(&layout).unwrap();
        assert_eq!(install(&layout, staged), Ok(Change::Replaced));
        assert_eq!(read(layout.installed()), "one");
        assert_eq!(read(&layout.previous()), "two");
        let left: Vec<_> = fs::read_dir(dir.path().join("bin")).unwrap().collect();
        assert_eq!(left.len(), 1, "no staged copy is left: {left:?}");
        assert!(!layout.held().exists());
    }

    #[test]
    fn an_installed_symlink_stays_a_link_and_its_target_is_what_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let real = file(dir.path(), "real", b"old");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let layout = Layout::new(&dir.path().join("home"), &link);
        let staged = stage(&layout, &file(dir.path(), "src", b"new")).unwrap();
        assert_eq!(install(&layout, staged), Ok(Change::Replaced));
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        let previous = layout.previous();
        assert!(!fs::symlink_metadata(&previous).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(previous).unwrap(), "old");
    }

    #[test]
    fn putting_back_with_no_previous_build_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = stage_previous(&layout(dir.path())).unwrap_err();
        assert!(err.contains("no previous build"), "{err}");
    }
}
