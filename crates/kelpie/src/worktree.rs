//! A work item's worktree and build folder
//!
//! Each work item gets its own git worktree, on a branch cut from the latest
//! `origin/main`, and its own build folder. Preparing is idempotent, so a
//! restarted runner finds the worktree it made before and keeps it.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The branch every work item is cut from, on `origin`
const BASE: &str = "main";

/// A prepared worktree, and the git dirs a commit from it writes to
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The repo's common git dir: objects, refs and config
    pub git_common_dir: PathBuf,
    /// The worktree's own git dir: its index and HEAD
    pub git_dir: PathBuf,
}

/// Why a worktree or build folder could not be prepared
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeError {
    /// git could not be started, with the OS's reason
    Spawn(String),
    /// A git command exited unsuccessfully
    Git {
        /// Its arguments
        args: String,
        /// What it printed on stderr
        stderr: String,
    },
    /// The folder exists but is not a worktree on the work item's branch
    Foreign(PathBuf),
    /// The branch exists with no worktree, so it is not kelpie's to reuse
    BranchTaken(String),
    /// A folder could not be created
    Folder {
        /// The folder
        path: PathBuf,
        /// What creating it failed with
        kind: io::ErrorKind,
    },
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "cannot run git: {e}"),
            Self::Git { args, stderr } => write!(f, "git {args} failed: {}", stderr.trim()),
            Self::Foreign(path) => write!(
                f,
                "{} exists and is not this work item's worktree",
                path.display()
            ),
            Self::BranchTaken(branch) => {
                write!(f, "branch {branch} already exists without its worktree")
            }
            Self::Folder { path, kind } => {
                write!(f, "cannot create {}: {kind}", path.display())
            }
        }
    }
}

impl std::error::Error for WorktreeError {}

/// Makes sure `worktree` is a worktree of `repo` on `branch`, and `build` exists
///
/// A new worktree's branch is cut from `origin/main` just fetched, and does
/// not track it.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn prepare(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    build: &Path,
) -> Result<Worktree, WorktreeError> {
    if worktree.exists() {
        let head = match git(worktree, ["rev-parse", "--abbrev-ref", "HEAD"]) {
            Err(e @ WorktreeError::Spawn(_)) => return Err(e),
            head => head.ok(),
        };
        if head.as_deref() != Some(branch) {
            return Err(WorktreeError::Foreign(worktree.to_owned()));
        }
    } else {
        let full_ref = format!("refs/heads/{branch}");
        if git(repo, ["rev-parse", "--verify", "--quiet", &full_ref]).is_ok() {
            return Err(WorktreeError::BranchTaken(branch.to_owned()));
        }
        if let Some(parent) = worktree.parent() {
            create(parent)?;
        }
        git(repo, ["fetch", "--quiet", "origin", BASE])?;
        let base = format!("origin/{BASE}");
        let wt = worktree.as_os_str();
        git(
            repo,
            [
                "worktree".as_ref(),
                "add".as_ref(),
                "--quiet".as_ref(),
                "--no-track".as_ref(),
                "-b".as_ref(),
                OsStr::new(branch),
                wt,
                base.as_ref(),
            ],
        )?;
    }
    create(build)?;
    let dir = |flag: &str| {
        git(worktree, ["rev-parse", "--path-format=absolute", flag]).map(PathBuf::from)
    };
    Ok(Worktree {
        git_common_dir: dir("--git-common-dir")?,
        git_dir: dir("--git-dir")?,
    })
}

fn create(path: &Path) -> Result<(), WorktreeError> {
    std::fs::create_dir_all(path).map_err(|e| WorktreeError::Folder {
        path: path.to_owned(),
        kind: e.kind(),
    })
}

fn git<I, S>(cwd: &Path, args: I) -> Result<String, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    if !output.status.success() {
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        return Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
