//! Writing the state file so a crash never leaves half of it

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

/// The rename is the commit point. Syncing the file first means the name
/// never points at unwritten bytes; syncing the folder keeps the rename. On
/// Windows, which cannot open a folder to sync it, NTFS journals the rename.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
    #[cfg(unix)]
    File::open(folder.unwrap_or(Path::new(".")))?.sync_all()?;
    #[cfg(windows)]
    let _ = folder;
    Ok(())
}
