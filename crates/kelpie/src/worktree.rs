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
pub const BASE: &str = "main";

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
    let foreign = || WorktreeError::Foreign(worktree.to_owned());
    let full_ref = format!("refs/heads/{branch}");
    if worktree.exists() {
        if listed_branch(repo, worktree)?.as_deref() != Some(full_ref.as_str()) {
            return Err(foreign());
        }
    } else {
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
    let common = git(
        repo,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let git_common_dir = canonical(Path::new(&common));
    let git_dir = own_git_dir(&git_common_dir, worktree).ok_or_else(foreign)?;
    create(build)?;
    Ok(Worktree {
        git_common_dir,
        git_dir,
    })
}

// Everything about the worktree is read from the project's repo, never from
// inside the worktree. The worker can rewrite its worktree's `.git` file,
// and git run there would believe it.

/// The branch `git worktree list` shows checked out at `worktree`
fn listed_branch(repo: &Path, worktree: &Path) -> Result<Option<String>, WorktreeError> {
    let list = git(repo, ["worktree", "list", "--porcelain"])?;
    let want = canonical(worktree);
    let mut here = false;
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            here = canonical(Path::new(path)) == want;
        } else if let Some(branch) = line.strip_prefix("branch ")
            && here
        {
            return Ok(Some(branch.to_owned()));
        }
    }
    Ok(None)
}

/// The folder under `<common>/worktrees/` whose `gitdir` names `worktree`
fn own_git_dir(common: &Path, worktree: &Path) -> Option<PathBuf> {
    let want = canonical(&worktree.join(".git"));
    std::fs::read_dir(common.join("worktrees"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| {
            std::fs::read_to_string(dir.join("gitdir"))
                .is_ok_and(|named| canonical(Path::new(named.trim())) == want)
        })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
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
