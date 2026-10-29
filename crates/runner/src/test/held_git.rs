//! A git fetch or push the test holds open

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;

/// A fetch from or push to `origin` that waits here until this is dropped
///
/// The project's checkout runs `remote.origin.uploadpack` or `receivepack` as a script in place
/// of git's own, which holds the next fetch or push open. Every one made after it goes through.
#[derive(Debug)]
pub struct HeldGit {
    folder: PathBuf,
}

impl HeldGit {
    /// Whether a fetch or push reached origin and waits here, within `within`
    pub fn waiting(&self, within: Duration) -> bool {
        let until = Instant::now() + within;
        while !self.folder.join("entered").exists() {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

impl Drop for HeldGit {
    fn drop(&mut self) {
        let _ = std::fs::write(self.folder.join("release"), "");
    }
}

impl Rig {
    /// Holds the next fetch from origin open, and lets every other through, so a test can ask a
    /// trigger of a runner whose pass is in the middle of a git read
    pub fn hold_next_git_fetch(&self) -> HeldGit {
        self.hold_git_fetch_after(0)
    }

    /// Holds the fetch from origin after the next `skipped`, which go through
    ///
    /// A step fetches in more than one place, so a test names the read it holds by how many
    /// fetches the step makes first.
    pub fn hold_git_fetch_after(&self, skipped: usize) -> HeldGit {
        self.hold_git("held-git", "uploadpack", "upload-pack", skipped)
    }

    /// Holds the next push to origin open, and lets every other through, so a test can ask a
    /// trigger of a runner whose pass is in the middle of a rebase's push
    ///
    /// A rebase pushes from the work item's clone, which copies no `receivepack` of the
    /// checkout's, so the clone of issue 7 is given the script too.
    pub fn hold_next_git_push(&self) -> HeldGit {
        let held = self.hold_git("held-push", "receivepack", "receive-pack", 0);
        if self.git_dir_7().is_dir() {
            let script = path(&held.folder.join("hold.sh")).to_owned();
            git(
                &self.worktree_7(),
                &["config", "remote.origin.receivepack", &script],
            );
        }
        held
    }

    // Runs `remote.origin.<key>` as a script that holds the call after the next `skipped` open
    // and then runs git's own `tool`
    fn hold_git(&self, name: &str, key: &str, tool: &str, skipped: usize) -> HeldGit {
        let folder = self.home.path().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let script = folder.join("hold.sh");
        let (armed, entered, release, skips) = (
            path(&folder.join("armed")).to_owned(),
            path(&folder.join("entered")).to_owned(),
            path(&folder.join("release")).to_owned(),
            path(&folder.join("skips")).to_owned(),
        );
        let body = format!(
            "#!/bin/sh\n\
             skips=$(cat '{skips}')\n\
             if [ \"$skips\" -gt 0 ]; then\n\
             echo $((skips - 1)) > '{skips}'\n\
             elif mv '{armed}' '{entered}' 2>/dev/null; then\n\
             while [ ! -e '{release}' ]; do sleep 0.01; done\n\
             fi\n\
             exec git {tool} \"$@\"\n"
        );
        let _ = std::fs::remove_file(&script);
        write_script(&script, &body);
        std::fs::write(folder.join("armed"), "").unwrap();
        std::fs::write(folder.join("skips"), skipped.to_string()).unwrap();
        git(
            &self.repo(),
            &["config", &format!("remote.origin.{key}"), path(&script)],
        );
        HeldGit { folder }
    }
}
