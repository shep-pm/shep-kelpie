//! The copy on Windows, through folders held open on the way down
//!
//! Windows has no `openat`, so `worker_folder` opens each folder by its path,
//! from the worktree down, and holds every handle until the copy ends. A link
//! anywhere on the way is refused, and a file is made after the old one is
//! removed.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::Path;

use shep_kelpie_worker_folder::worker_folder::{Error, Held};

const LINK: &str = "in the worktree is a link, and results are not copied through one";

/// Copies what is in `staging` into the `results` folder of `worktree`,
/// making the folder if it is not there, leaving what else is in it, and
/// skipping any link in `staging`
///
/// # Errors
///
/// A message when `results` or a folder above it in the worktree is a link,
/// or when a folder or file cannot be made or written.
pub(in crate::dog::e2e::run) fn deliver(
    staging: &Path,
    worktree: &Path,
    results: &Path,
) -> Result<(), String> {
    let mut held =
        Held::open(worktree).map_err(|e| format!("cannot open {}: {e}", worktree.display()))?;
    let mut shown = Path::new("").to_owned();
    for part in results {
        shown.push(part);
        make_and_enter(&mut held, part, &shown)?;
    }
    copy_tree(staging, &mut held, &shown)
}

// Holds folder `name` of the last one held, making it first if it is not there.
fn make_and_enter(held: &mut Held, name: &OsStr, shown: &Path) -> Result<(), String> {
    held.make_and_enter(name).map_err(|e| match e {
        Error::NotFolder => format!("{} {LINK}", shown.display()),
        e => format!("cannot open {} in the worktree: {e}", shown.display()),
    })
}

fn copy_tree(from: &Path, held: &mut Held, shown: &Path) -> Result<(), String> {
    let read = |e: io::Error| format!("cannot read {}: {e}", from.display());
    for entry in std::fs::read_dir(from).map_err(read)? {
        let entry = entry.map_err(read)?;
        let name = entry.file_name();
        let shown = shown.join(&name);
        let kind = entry.file_type().map_err(read)?;
        if kind.is_dir() {
            make_and_enter(held, &name, &shown)?;
            let copied = copy_tree(&entry.path(), held, &shown);
            held.leave();
            copied?;
        } else if kind.is_file() {
            copy_file(&entry.path(), held, &name, &shown)?;
        }
    }
    Ok(())
}

fn copy_file(from: &Path, held: &Held, name: &OsStr, shown: &Path) -> Result<(), String> {
    let failed = |e: &dyn std::fmt::Display| format!("cannot write {}: {e}", shown.display());
    let mut source = File::open(from).map_err(|e| failed(&e))?;
    // A folder made a link in place since it was held is caught here.
    let mut dest = held.create(name).map_err(|e| match e {
        Error::NotFolder => format!("a folder above {} {LINK}", shown.display()),
        e => failed(&e),
    })?;
    io::copy(&mut source, &mut dest).map_err(|e| failed(&e))?;
    // rsync kept the host's times; so does this.
    if let Ok(modified) = source.metadata().and_then(|m| m.modified()) {
        let _ = dest.set_modified(modified);
    }
    Ok(())
}
