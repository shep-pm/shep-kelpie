//! The Windows implementation: folders opened by path from the root down and held
//!
//! Sharing only read access keeps a folder from being renamed, replaced or
//! opened for writing. A reparse point (a symlink or a junction) is refused.
//! Setting one on an open folder needs only write-attributes access, which
//! sharing cannot deny, so each held folder is read again before a file is
//! made or renamed. A hard link is read, not refused, and `write_private`
//! sets no access list.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use super::{Error, MAX};

// The Win32 header values, kept here so the crate takes no crate for five numbers.
// `FILE_FLAG_*` are in winbase.h, `FILE_SHARE_*` and `FILE_ATTRIBUTE_*` in winnt.h.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const FILE_SHARE_READ: u32 = 0x1;

fn io_error(e: &io::Error) -> Error {
    Error::Io(e.kind())
}

/// The folders from a root down to the one being worked in, each held open
///
/// A path is opened a folder at a time, and no folder on the way can be
/// swapped for a link or moved while the `Held` lives. Every method that
/// makes or reads a file reads the attributes of every held folder again
/// first. A worker that turns a held folder into a junction in the instant
/// after that read can still land one file outside it, which Unix has no gap
/// for: closing it needs a handle-relative open, which needs `unsafe`.
#[derive(Debug)]
pub struct Held {
    // The root first, each with the path it was opened by.
    folders: Vec<(File, PathBuf)>,
}

impl Held {
    /// Holds `root`, which may itself be reached through a link
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when it cannot be opened, [`io::ErrorKind::NotFound`]
    /// among them.
    pub fn open(root: &Path) -> Result<Self, Error> {
        let folder = options()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(root)
            .map_err(|e| io_error(&e))?;
        Ok(Self {
            folders: vec![(folder, root.to_owned())],
        })
    }

    /// Holds folder `name` of the one held last, or says it is not there
    ///
    /// # Errors
    ///
    /// [`Error::NotFolder`] when it is a link or not a folder, and
    /// [`Error::Io`] for the rest.
    pub fn enter(&mut self, name: &OsStr) -> Result<bool, Error> {
        let path = self.here().join(name);
        match open_folder(&path) {
            Ok(folder) => self.folders.push((folder, path)),
            Err(Error::Io(io::ErrorKind::NotFound)) => return Ok(false),
            Err(e) => return Err(e),
        }
        Ok(true)
    }

    /// As [`enter()`](Self::enter), making the folder first if it is not there
    ///
    /// # Errors
    ///
    /// As [`enter()`](Self::enter), and [`Error::Io`] when it cannot be made.
    pub fn make_and_enter(&mut self, name: &OsStr) -> Result<(), Error> {
        match std::fs::create_dir(self.here().join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(&e)),
        }
        self.enter(name).map(drop)
    }

    /// Lets go of the folder held last, never the root
    pub fn leave(&mut self) {
        if self.folders.len() > 1 {
            self.folders.pop();
        }
    }

    /// Refuses when a held folder has become a link
    ///
    /// # Errors
    ///
    /// [`Error::NotFolder`] for a folder that is a link now, and
    /// [`Error::Io`] when its attributes cannot be read.
    pub fn check(&self) -> Result<(), Error> {
        for (folder, _) in &self.folders {
            let attributes = folder
                .metadata()
                .map_err(|e| io_error(&e))?
                .file_attributes();
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(Error::NotFolder);
            }
        }
        Ok(())
    }

    /// Makes a new file `name` in the folder held last, after removing what
    /// was there, and opens it for writing
    ///
    /// What was there is removed, not opened: opening a hard link would write
    /// through it to a file outside the folder. A link put back in between is
    /// refused by `create_new`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFolder`] as [`check()`](Self::check), and [`Error::Io`] for
    /// the rest.
    pub fn create(&self, name: &OsStr) -> Result<File, Error> {
        self.check()?;
        let path = self.here().join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(&e)),
        }
        create_new(&path).map_err(|e| io_error(&e))
    }

    fn here(&self) -> &Path {
        self.folders.last().map_or(Path::new(""), |(_, at)| at)
    }

    fn read(&self, name: &OsStr) -> Result<Option<String>, Error> {
        self.check()?;
        // Shared as std shares by default: a file the worker holds open is still read.
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.here().join(name));
        let file = match opened {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(&e)),
        };
        let found = file.metadata().map_err(|e| io_error(&e))?;
        if found.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !found.is_file() {
            return Err(Error::NotRegular);
        }
        if found.len() > MAX {
            return Err(Error::TooBig);
        }
        let mut text = String::new();
        // One byte past the cap, so a file that grew since the metadata is caught.
        file.take(MAX + 1)
            .read_to_string(&mut text)
            .map_err(|e| io_error(&e))?;
        if text.len() as u64 > MAX {
            return Err(Error::TooBig);
        }
        Ok(Some(text))
    }

    fn put(&self, name: &OsStr, text: &str) -> Result<(), Error> {
        self.check()?;
        let mut beside = OsString::from(name);
        beside.push(".tmp");
        let (to, beside) = (self.here().join(name), self.here().join(beside));
        let mut file = match create_new(&beside) {
            Ok(file) => file,
            // A file or link left there, by a crash or the worker. Removing it
            // removes a link itself, never what it names.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&beside).map_err(|e| io_error(&e))?;
                create_new(&beside).map_err(|e| io_error(&e))?
            }
            Err(e) => return Err(io_error(&e)),
        };
        let written = file.write_all(text.as_bytes());
        drop(file);
        let staged = written
            .map_err(|e| io_error(&e))
            .and_then(|()| self.check());
        let done = staged.and_then(|()| match std::fs::rename(&beside, &to) {
            Ok(()) => Ok(()),
            // A rename by path may open the held folder for writing, which its
            // share mode refuses. The old file is then removed and the new one
            // made in its place, as `create` does.
            Err(_) => self
                .create(name)
                .and_then(|mut file| file.write_all(text.as_bytes()).map_err(|e| io_error(&e))),
        });
        // Whatever is left of the staged file goes, a renamed one being gone already.
        let _ = std::fs::remove_file(&beside);
        done
    }
}

// Only read access is shared: that keeps an open folder in its place and
// closed to a writer.
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.share_mode(FILE_SHARE_READ);
    options
}

fn create_new(path: &Path) -> io::Result<File> {
    options()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

// Opens the folder at `path` and refuses it when it is a link or not a folder.
// The check is on the handle that was opened, which is the link itself and not
// what it points to.
fn open_folder(path: &Path) -> Result<File, Error> {
    let folder = options()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| io_error(&e))?;
    let found = folder.metadata().map_err(|e| io_error(&e))?;
    if found.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !found.is_dir() {
        return Err(Error::NotFolder);
    }
    Ok(folder)
}

// The folders and the file's name of `rel`, which holds nothing but names.
fn split(rel: &Path) -> Result<(Vec<&OsStr>, &OsStr), Error> {
    let mut names = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => names.push(name),
            _ => return Err(Error::Io(io::ErrorKind::InvalidInput)),
        }
    }
    let file = names.pop().ok_or(Error::Io(io::ErrorKind::InvalidInput))?;
    Ok((names, file))
}

pub(super) fn read(root: &Path, rel: &Path) -> Result<Option<String>, Error> {
    let (folders, file) = split(rel)?;
    let mut held = match Held::open(root) {
        Ok(held) => held,
        Err(Error::Io(io::ErrorKind::NotFound)) => return Ok(None),
        Err(e) => return Err(e),
    };
    for folder in folders {
        if !held.enter(folder)? {
            return Ok(None);
        }
    }
    held.read(file)
}

// A file on Windows takes the folder's access list, so `private` changes nothing.
pub(super) fn write(root: &Path, rel: &Path, text: &str, _private: bool) -> Result<(), Error> {
    let (folders, file) = split(rel)?;
    std::fs::create_dir_all(root).map_err(|e| io_error(&e))?;
    let mut held = Held::open(root)?;
    for folder in folders {
        held.make_and_enter(folder)?;
    }
    held.put(file, text)
}
