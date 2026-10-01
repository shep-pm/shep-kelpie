//! One upgrade at a time
//!
//! The lock is a file under kelpie's home holding the pid of the upgrade that
//! took it. A file left by an upgrade that died is taken over; one held by a
//! live process refuses the second upgrade.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The upgrade lock, held until it is dropped
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Takes the lock under `kelpie_home`
    ///
    /// # Errors
    ///
    /// A message naming the upgrade that holds it, or the file that cannot be made.
    pub fn take(kelpie_home: &Path) -> Result<Self, String> {
        let path = kelpie_home.join("upgrade.lock");
        fs::create_dir_all(kelpie_home)
            .map_err(|e| format!("cannot make {}: {e}", kelpie_home.display()))?;
        for _ in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    write!(file, "{}", std::process::id())
                        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder = fs::read_to_string(&path)
                        .ok()
                        .and_then(|text| text.trim().parse::<u32>().ok());
                    match holder {
                        Some(pid) if !alive(pid) => {
                            let _ = fs::remove_file(&path);
                        }
                        Some(pid) => return Err(held(&path, &format!("process {pid}"))),
                        None => return Err(held(&path, "a process")),
                    }
                }
                Err(e) => return Err(format!("cannot take {}: {e}", path.display())),
            }
        }
        Err(held(&path, "a process"))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn held(path: &Path, by: &str) -> String {
    format!(
        "another upgrade is running ({by} holds {}): wait for it, or remove that file if no \
         upgrade is running",
        path.display()
    )
}

// Whether a process with this pid exists.
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_is_refused_until_the_first_is_dropped() {
        let home = tempfile::tempdir().unwrap();
        let first = Lock::take(home.path()).unwrap();
        let err = Lock::take(home.path()).unwrap_err();
        assert!(err.contains("another upgrade is running"), "{err}");
        drop(first);
        Lock::take(home.path()).unwrap();
    }

    #[test]
    fn a_lock_left_by_a_dead_upgrade_is_taken_over() {
        let home = tempfile::tempdir().unwrap();
        let mut gone = Command::new("true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        fs::write(home.path().join("upgrade.lock"), pid.to_string()).unwrap();
        Lock::take(home.path()).unwrap();
    }
}
