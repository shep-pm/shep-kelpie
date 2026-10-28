//! The local round, over a command or an OpenAI-compatible endpoint
//!
//! A reviewer's definition chooses the kind. A command keeps the README's
//! contract, as the maintainer's qwen-review script does, and queues its
//! model itself. An endpoint gets kelpie's own reviewer. An endpoint behind
//! a gateway holds the gateway's lease on its model for each round, and the
//! gateway queues the rounds. Kelpie holds no lock of its own around either.

mod command;
mod endpoint;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::files::is_executable;
use super::paddock;
use super::process::Processes;
use crate::forwarder::Upstream;
use crate::ports::{
    Finding, LocalLeases, ModelLoad, PaddockHolder, Reviewer, ReviewerError, RoundStage,
};
#[cfg(any(test, feature = "test-support"))]
use crate::settings::Timing;
use crate::settings::{Gateways, LocalRound};
use crate::worktree::Linked;

/// Runs each local round, of whichever kind the settings choose
///
/// Clones share their rounds in flight, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct LocalReviewer {
    temp_dir: PathBuf,
    pub(super) processes: Processes,
    // Where an endpoint behind a gateway sends its rounds, with the gateway's key
    pub(super) gateways: Gateways,
}

impl Default for LocalReviewer {
    fn default() -> Self {
        Self {
            temp_dir: temp_dir(),
            processes: Processes::default(),
            gateways: Gateways::default(),
        }
    }
}

/// The temporary folder the maintainer's qwen scripts use
///
/// The scripts build their own GPU lock under `${TMPDIR:-/tmp}`. A sheep runs
/// without `TMPDIR`, while the maintainer's shell has macOS's per-user
/// temporary folder, so an unset or empty `TMPDIR` falls back to that folder
/// before `/tmp`.
fn temp_dir() -> PathBuf {
    temp_dir_from(std::env::var_os("TMPDIR"), darwin_user_temp_dir)
}

fn temp_dir_from(tmpdir: Option<OsString>, darwin: impl FnOnce() -> Option<PathBuf>) -> PathBuf {
    match tmpdir {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => darwin().unwrap_or_else(|| PathBuf::from("/tmp")),
    }
}

// `getconf` reads the same confstr macOS sets TMPDIR from at login.
fn darwin_user_temp_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = crate::spawn::command("getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let dir = String::from_utf8(output.stdout).ok()?;
    let dir = dir.trim_end_matches('\n');
    (output.status.success() && !dir.is_empty()).then(|| PathBuf::from(dir))
}

impl LocalReviewer {
    /// Runs every command round with `TMPDIR` set to `temp_dir`
    ///
    /// A reviewer starts with the folder the maintainer's qwen scripts lock
    /// under. Without it the runner's rounds would lock `/tmp` while every
    /// interactive session locks the per-user temp folder, and the two
    /// queues would run the GPU at once.
    pub fn with_temp_dir(mut self, temp_dir: PathBuf) -> Self {
        self.temp_dir = temp_dir;
        self
    }

    /// Sends an endpoint's rounds behind a gateway through `gateways`
    pub fn with_gateways(mut self, gateways: Gateways) -> Self {
        self.gateways = gateways;
        self
    }

    /// Takes and renews a gateway lease on `paddock`'s schedule
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_paddock_timing(mut self, paddock: Timing) -> Self {
        self.gateways = self.gateways.with_paddock_timing(paddock);
        self
    }

    /// Refuses new rounds as the runner stops, sends the rounds in flight
    /// SIGTERM, and leaves the rest to shep's stop
    pub fn stop(&self) {
        self.processes.stop();
    }
}

impl LocalLeases for LocalReviewer {
    fn paddock_holder(&self, gateway: &Upstream, model: &str) -> Option<PaddockHolder> {
        paddock::holder(gateway, model).ok().flatten()
    }

    fn paddock_load(&self, gateway: &Upstream, model: &str) -> Option<ModelLoad> {
        paddock::load(gateway, model).ok()
    }
}

impl Reviewer for LocalReviewer {
    fn check(&self, local: &LocalRound) -> Result<(), String> {
        match local {
            LocalRound::Command(local) => check_command(&local.command),
            LocalRound::Endpoint(endpoint) => self.check_endpoint(endpoint),
        }
    }

    fn round(
        &self,
        local: &LocalRound,
        worktree: Linked<'_>,
        base: &str,
        out: &Path,
        round: u32,
        criteria: &str,
    ) -> Result<Vec<Finding>, ReviewerError> {
        self.round_watched(local, worktree, base, out, round, criteria, &|_| {})
    }

    fn round_watched(
        &self,
        local: &LocalRound,
        worktree: Linked<'_>,
        base: &str,
        out: &Path,
        round: u32,
        criteria: &str,
        watch: &(dyn Fn(RoundStage) + Sync),
    ) -> Result<Vec<Finding>, ReviewerError> {
        match local {
            LocalRound::Command(command) => {
                self.command_round(command, worktree, base, out, round, criteria)
            }
            LocalRound::Endpoint(endpoint) => {
                self.endpoint_round(endpoint, worktree, base, (out, round), criteria, watch)
            }
        }
    }
}

fn check_command(command: &Path) -> Result<(), String> {
    let cannot = |why: &str| format!("cannot run {}: {why}", command.display());
    let meta = std::fs::metadata(command).map_err(|e| cannot(&e.kind().to_string()))?;
    if !meta.is_file() {
        return Err(cannot("not a file"));
    }
    if !is_executable(&meta) {
        return Err(cannot("not executable"));
    }
    Ok(())
}

/// Removes everything a run of round `round` wrote to `out`, its findings
/// file and marker, request and reply files and hunks, so a rework or a
/// retried round never reads the last run's, and a kept round holds only
/// its own
fn clear_round(out: &Path, round: u32) -> Result<(), ReviewerError> {
    let hunks = out.join("hunks");
    let mut stale = vec![
        out.join(format!("round-{round}.txt")),
        out.join(format!("round-{round}.txt.done")),
        out.join(format!("round-{round}")),
        hunks.join(round.to_string()),
    ];
    let prefix = format!("{round}-out-");
    if let Ok(entries) = std::fs::read_dir(&hunks) {
        stale.extend(
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .map(|e| e.path()),
        );
    }
    for path in stale {
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ReviewerError::Failed(format!(
                    "cannot remove {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// The worktree's head commit
fn head(at: Linked<'_>) -> Result<String, ReviewerError> {
    crate::worktree::head(at.repo, at.worktree).map_err(|e| {
        ReviewerError::Failed(format!(
            "cannot read the head of {}: {e}",
            at.worktree.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LocalCommand;
    use crate::test::write_script;

    fn command(path: &Path) -> LocalRound {
        LocalRound::Command(LocalCommand {
            command: path.to_owned(),
        })
    }

    #[test]
    fn a_command_is_checked_for_being_there_and_runnable() {
        let dir = tempfile::tempdir().unwrap();
        let reviewer = LocalReviewer::default();
        let gone = dir.path().join("gone");
        let err = reviewer.check(&command(&gone)).unwrap_err();
        assert_eq!(
            err,
            format!("cannot run {}: entity not found", gone.display())
        );
        let plain = dir.path().join("plain.txt");
        std::fs::write(&plain, "text\n").unwrap();
        let err = reviewer.check(&command(&plain)).unwrap_err();
        assert!(err.ends_with("plain.txt: not executable"), "{err}");
        let err = reviewer.check(&command(dir.path())).unwrap_err();
        assert!(err.ends_with(": not a file"), "{err}");
        let script = dir.path().join("review");
        write_script(&script, "#!/bin/sh\nexit 0\n");
        assert_eq!(reviewer.check(&command(&script)), Ok(()));
    }

    #[test]
    fn tmpdir_wins_when_set_and_not_empty() {
        let never = || panic!("asked macOS with TMPDIR set");
        assert_eq!(
            temp_dir_from(Some("/var/folders/x/T/".into()), never),
            PathBuf::from("/var/folders/x/T/")
        );
    }

    #[test]
    fn no_tmpdir_falls_back_to_the_users_temp_folder_then_tmp() {
        let user = || Some(PathBuf::from("/var/folders/x/T/"));
        assert_eq!(
            temp_dir_from(None, user),
            PathBuf::from("/var/folders/x/T/")
        );
        assert_eq!(
            temp_dir_from(Some("".into()), user),
            PathBuf::from("/var/folders/x/T/")
        );
        assert_eq!(temp_dir_from(None, || None), PathBuf::from("/tmp"));
    }
}
