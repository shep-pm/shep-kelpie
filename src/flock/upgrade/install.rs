//! The files an install is made of: versioned builds, the link kelpie's
//! sheep run through, the link to the previous build, and the upgrade lock

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

/// Kelpie's builds and the links to them, under kelpie's home
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    home: PathBuf,
}

impl Install {
    /// The install under `home`
    pub fn under(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
        }
    }

    /// The link every kelpie sheep runs through
    pub fn link(&self) -> PathBuf {
        self.home.join("bin/kelpie")
    }

    /// The link to the build the current one replaced
    pub fn previous(&self) -> PathBuf {
        self.home.join("bin/kelpie.previous")
    }

    /// Where builds are kept, one file each, never overwritten
    pub fn builds(&self) -> PathBuf {
        self.home.join("builds")
    }

    /// The scratch folder builds are made in, target folder included
    pub fn staging(&self) -> PathBuf {
        self.home.join("upgrade")
    }

    pub(super) fn lock(&self) -> PathBuf {
        self.home.join("upgrade.lock")
    }

    /// The build the link points at
    ///
    /// # Errors
    ///
    /// A message when there is no link there, or it is a file.
    pub fn current(&self) -> Result<PathBuf, String> {
        let link = self.link();
        std::fs::read_link(&link).map_err(|e| {
            format!(
                "{} is not a link to a build ({e}): kelpie's sheep must run through it, so move \
                 the current build into {} and link it there",
                link.display(),
                self.builds().display()
            )
        })
    }

    /// The build the previous link points at, if it still exists
    ///
    /// # Errors
    ///
    /// A message when there is no previous build to go back to.
    pub fn before(&self) -> Result<PathBuf, String> {
        let previous = self.previous();
        let target = std::fs::read_link(&previous).map_err(|_| {
            "there is no previous build to go back to: no upgrade has run here".to_owned()
        })?;
        if target.is_file() {
            Ok(target)
        } else {
            Err(format!(
                "the previous build {} is gone, so there is nothing to go back to",
                target.display()
            ))
        }
    }

    /// Puts `binary` in the builds folder as `kelpie-<label>`, returning its
    /// path and whether the file is new
    ///
    /// A file already there with the same bytes is reused, and one with other
    /// bytes is never overwritten: the label gets a number instead, since a
    /// running binary overwritten in place has to be signed again.
    ///
    /// # Errors
    ///
    /// A message when the folder cannot be made or the file cannot be copied.
    pub fn keep(&self, binary: &Path, label: &str) -> Result<(PathBuf, bool), String> {
        let builds = self.builds();
        if binary.parent() == Some(builds.as_path()) {
            return Ok((binary.to_owned(), false));
        }
        std::fs::create_dir_all(&builds)
            .map_err(|e| format!("cannot make {}: {e}", builds.display()))?;
        let wanted =
            std::fs::read(binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
        for number in 1.. {
            let name = match number {
                1 => format!("kelpie-{label}"),
                n => format!("kelpie-{label}-{n}"),
            };
            let target = builds.join(name);
            match std::fs::read(&target) {
                Ok(held) if held == wanted => return Ok((target, false)),
                Ok(_) => {}
                Err(_) => {
                    std::fs::write(&target, &wanted)
                        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
                        .map_err(|e| format!("cannot make {} executable: {e}", target.display()))?;
                    return Ok((target, true));
                }
            }
        }
        unreachable!("the numbering never ends")
    }

    /// Points the link at `build`, and the previous link at what it pointed at
    ///
    /// # Errors
    ///
    /// A message when a link cannot be written.
    pub fn relink(&self, build: &Path) -> Result<(), String> {
        let old = std::fs::read_link(self.link()).ok();
        if old.as_deref() == Some(build) {
            return Ok(());
        }
        if let Some(old) = old {
            replace_link(&self.previous(), &old)?;
        }
        replace_link(&self.link(), build)
    }
}

// A link replaced by a rename, so a reader sees the old target or the new one.
fn replace_link(link: &Path, target: &Path) -> Result<(), String> {
    let folder = link.parent().ok_or("a link has no folder")?;
    std::fs::create_dir_all(folder)
        .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    let mut name = link.as_os_str().to_owned();
    name.push(".new");
    let temporary = PathBuf::from(name);
    let _ = std::fs::remove_file(&temporary);
    symlink(target, &temporary).map_err(|e| format!("cannot link {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, link).map_err(|e| format!("cannot replace {}: {e}", link.display()))
}

/// The upgrade lock, held until it is dropped
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Takes the lock for the process `pid`, clearing one whose holder is gone
    ///
    /// # Errors
    ///
    /// A message naming the upgrade in progress when a live process holds it.
    pub fn take(install: &Install, pid: u32, alive: impl Fn(u32) -> bool) -> Result<Self, String> {
        let path = install.lock();
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        }
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    writeln!(file, "{pid}")
                        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|text| text.trim().parse::<u32>().ok());
                    if let Some(holder) = holder.filter(|&holder| alive(holder)) {
                        return Err(format!(
                            "an upgrade is already in progress (process {holder}): wait for it to \
                             end"
                        ));
                    }
                    std::fs::remove_file(&path)
                        .map_err(|e| format!("cannot clear {}: {e}", path.display()))?;
                }
                Err(e) => return Err(format!("cannot take {}: {e}", path.display())),
            }
        }
        Err(format!("cannot take {}", path.display()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
