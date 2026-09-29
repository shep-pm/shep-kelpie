//! What every project's runner shares about the codes sent on the topic
//!
//! All under one folder: `used/<step>` names the message that first sent a
//! step's code, `failed/<message>` each message whose code was wrong, and
//! `locked` whether answers from the topic are off. Every runner reads the
//! same topic, so each records by message: two runners that see the same
//! reply agree it is the one that used its step, and count it as one
//! failure, not two.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use super::{STEP, private_dir, step_of};
use crate::ports::Timestamp;

/// Wrong codes, in messages kelpie has not seen before, after which answers
/// from the topic are off until the maintainer turns them back on (the
/// throttling RFC 4226 section 7.3 asks for)
pub const FAILURES: usize = 5;

/// Whose a step is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The message asking first sent the step's code
    Ours,
    /// Another message sent it first: this one repeats a code already sent
    Replayed,
}

/// What a wrong code did
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// It was counted, with fewer than [`FAILURES`] so far
    Counted,
    /// It was the one that turned answers off
    LockedNow,
}

/// The shared record of codes sent on the topic
#[derive(Debug, Clone)]
pub struct Answers(PathBuf);

impl Answers {
    /// The record under `folder`
    pub fn in_folder(folder: PathBuf) -> Self {
        Self(folder)
    }

    /// Claims `step` for `message`, whose code is right for it. Steps too
    /// old to be sent again are let go.
    ///
    /// The claim is written beside its name, synced, then linked into place,
    /// which fails if the name exists, so a claim is whole or absent.
    ///
    /// # Errors
    ///
    /// The OS's error when the claim cannot be written or read; the reply
    /// then answers nothing.
    pub fn claim(&self, step: u64, message: &str, now: Timestamp) -> io::Result<Claim> {
        let used = self.0.join("used");
        private_dir(&used)?;
        let name = used.join(step.to_string());
        let fresh = used.join(format!(".{step}.{message}.{}", std::process::id()));
        let _ = fs::remove_file(&fresh);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&fresh)?;
        file.write_all(message.as_bytes())?;
        file.sync_all()?;
        let linked = fs::hard_link(&fresh, &name);
        let _ = fs::remove_file(&fresh);
        let claim = match linked {
            Ok(()) => {
                fs::File::open(&used)?.sync_all()?;
                Claim::Ours
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if fs::read_to_string(&name)? == message {
                    Claim::Ours
                } else {
                    Claim::Replayed
                }
            }
            Err(e) => return Err(e),
        };
        // A reply is read at most an hour after it is sent.
        let oldest = step_of(now).saturating_sub(3600 / STEP);
        for entry in fs::read_dir(&used)?.flatten() {
            let old = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u64>().ok());
            if old.is_some_and(|old| old < oldest) {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(claim)
    }

    /// Counts `message`'s wrong code, once however many runners see it, and
    /// turns answers off at the [`FAILURES`]th
    ///
    /// # Errors
    ///
    /// The OS's error when the count cannot be written.
    pub fn fail(&self, message: &str) -> io::Result<Failure> {
        let failed = self.0.join("failed");
        private_dir(&failed)?;
        let counted = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(failed.join(message));
        match counted {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(Failure::Counted),
            Err(e) => return Err(e),
        }
        if fs::read_dir(&failed)?.count() < FAILURES {
            return Ok(Failure::Counted);
        }
        let locked = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.0.join("locked"));
        match locked {
            Ok(_) => Ok(Failure::LockedNow),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Failure::Counted),
            Err(e) => Err(e),
        }
    }

    /// Whether answers from the topic are off. One that cannot be told is
    /// off.
    pub fn locked(&self) -> bool {
        !matches!(
            fs::symlink_metadata(self.0.join("locked")),
            Err(e) if e.kind() == io::ErrorKind::NotFound
        )
    }

    /// Forgets the wrong codes, once a right one has answered
    ///
    /// # Errors
    ///
    /// The OS's error when the count cannot be removed.
    pub fn forgive(&self) -> io::Result<()> {
        match fs::remove_dir_all(self.0.join("failed")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Turns answers from the topic back on, forgetting the wrong codes
    ///
    /// # Errors
    ///
    /// The OS's error when the lock or the count cannot be removed.
    pub fn unlock(&self) -> io::Result<()> {
        self.forgive()?;
        match fs::remove_file(self.0.join("locked")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_111_111_109);

    #[test]
    fn a_step_is_the_first_messages_across_every_runner() {
        let home = tempfile::tempdir().unwrap();
        let (one, two) = (
            Answers::in_folder(home.path().to_owned()),
            Answers::in_folder(home.path().to_owned()),
        );
        let step = step_of(NOW);
        assert_eq!(one.claim(step, "m1", NOW).unwrap(), Claim::Ours);
        // Another runner reading the same reply agrees it is the step's.
        assert_eq!(two.claim(step, "m1", NOW).unwrap(), Claim::Ours);
        assert_eq!(two.claim(step, "m2", NOW).unwrap(), Claim::Replayed);
        assert_eq!(one.claim(step + 1, "m2", NOW).unwrap(), Claim::Ours);

        let later = Timestamp(NOW.0 + 2 * 3600);
        assert_eq!(one.claim(step_of(later), "m3", later).unwrap(), Claim::Ours);
        assert!(!home.path().join(format!("used/{step}")).exists(), "let go");
        let left: Vec<_> = fs::read_dir(home.path().join("used")).unwrap().collect();
        assert_eq!(left.len(), 1, "no half-written claim left beside them");
    }

    #[test]
    fn the_fifth_wrong_code_turns_answers_off_until_unlocked() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let (one, two) = (
            Answers::in_folder(home.path().to_owned()),
            Answers::in_folder(home.path().to_owned()),
        );
        for message in ["m1", "m2", "m3", "m4"] {
            assert_eq!(one.fail(message).unwrap(), Failure::Counted);
            assert_eq!(two.fail(message).unwrap(), Failure::Counted, "counted once");
        }
        assert!(!one.locked());
        assert_eq!(two.fail("m5").unwrap(), Failure::LockedNow);
        assert_eq!(one.fail("m5").unwrap(), Failure::Counted, "locked once");
        assert!(one.locked() && two.locked());
        let mode = fs::metadata(home.path().join("failed"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);

        one.unlock().unwrap();
        assert!(!two.locked());
        for message in ["m6", "m7", "m8", "m9"] {
            assert_eq!(
                two.fail(message).unwrap(),
                Failure::Counted,
                "counts from 0"
            );
        }
        // A right answer forgives them.
        one.forgive().unwrap();
        assert_eq!(two.fail("m10").unwrap(), Failure::Counted);
        assert!(!one.locked());
    }
}
