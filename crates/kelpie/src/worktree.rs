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
    /// A build folder could not be removed
    Remove {
        /// The folder
        path: PathBuf,
        /// What removing it failed with
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
            Self::Remove { path, kind } => {
                write!(f, "cannot remove {}: {kind}", path.display())
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

/// Removes a work item's worktree, its branch and its build folder
///
/// `remote` also deletes the branch on `origin`. Whatever is already gone
/// is skipped, so a removal cut short can run again.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn remove(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    build: &Path,
    remote: bool,
) -> Result<(), WorktreeError> {
    if worktree.exists() {
        let wt = worktree.as_os_str();
        git(
            repo,
            [
                "worktree".as_ref(),
                "remove".as_ref(),
                "--force".as_ref(),
                wt,
            ],
        )?;
    }
    git(repo, ["worktree", "prune"])?;
    let full_ref = format!("refs/heads/{branch}");
    if git(repo, ["rev-parse", "--verify", "--quiet", &full_ref]).is_ok() {
        git(repo, ["branch", "--quiet", "-D", branch])?;
    }
    if remote && !git(repo, ["ls-remote", "--heads", "origin", &full_ref])?.is_empty() {
        git(repo, ["push", "--quiet", "origin", "--delete", &full_ref])?;
    }
    match std::fs::remove_dir_all(build) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(WorktreeError::Remove {
            path: build.to_owned(),
            kind: e.kind(),
        }),
        _ => Ok(()),
    }
}

/// Where a pull request's head stands against `origin`, just fetched
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// The head is the branch on `origin`, and has the latest `main`
    Current,
    /// The head is the branch on `origin`, and lacks the latest `main`
    Behind,
    /// The branch on `origin` is not the head the forge reported, which
    /// lags a push by a moment
    Lagging,
}

/// Fetches `origin`, and says where `head`, the forge's head of `branch`, stands
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn base_of(repo: &Path, branch: &str, head: &str) -> Result<Base, WorktreeError> {
    git(repo, ["fetch", "--quiet", "origin", BASE, branch])?;
    let tracking = format!("refs/remotes/origin/{branch}");
    if git(repo, ["rev-parse", "--verify", "--quiet", &tracking])? != head {
        return Ok(Base::Lagging);
    }
    // `--is-ancestor` answers no with exit 1, and fails with any other code.
    let base = format!("origin/{BASE}");
    let args = ["merge-base", "--is-ancestor", &base, head];
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    match output.status.code() {
        Some(0) => Ok(Base::Current),
        Some(1) => Ok(Base::Behind),
        _ => Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        }),
    }
}

/// Fetches `branch` from `origin`, and returns its head there
///
/// Git's own answer, which the forge's lags by a moment after a push.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn pushed_head(repo: &Path, branch: &str) -> Result<String, WorktreeError> {
    git(repo, ["fetch", "--quiet", "origin", branch])?;
    let tracking = format!("refs/remotes/origin/{branch}");
    git(repo, ["rev-parse", "--verify", "--quiet", &tracking])
}

/// What a rebase onto `origin/main` came to
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebase {
    /// Rebased and pushed: the branch's new head
    Pushed(String),
    /// Left as it was, for this reason, which only the maintainer can settle
    Refused(String),
}

/// Rebases the worktree's branch, at `head`, onto `origin/main` and pushes it
///
/// The push is forced with a lease on `head`, so it fails rather than drop a
/// commit pushed since. A conflict aborts the rebase, and a failed push
/// puts the branch back at `head`. Run [`base_of`] first, which fetches.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn rebase(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    head: &str,
) -> Result<Rebase, WorktreeError> {
    let in_worktree = trusted(repo, worktree)?;
    let full_ref = format!("refs/heads/{branch}");
    let on_branch = in_worktree(&["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .as_deref()
        == Some(&full_ref);
    if !on_branch || in_worktree(&["rev-parse", "HEAD"])? != head {
        let short = head.get(..7).unwrap_or(head);
        return Ok(Rebase::Refused(format!(
            "its worktree is not at the pull request's head {short}"
        )));
    }
    if !in_worktree(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Ok(Rebase::Refused(
            "its worktree has changes that are not committed".into(),
        ));
    }
    // The rebased commits take the head's committer, the worker's one
    // identity, so the rebase needs no identity of its own.
    let name = format!(
        "user.name={}",
        in_worktree(&["log", "-1", "--format=%cn", head])?
    );
    let email = format!(
        "user.email={}",
        in_worktree(&["log", "-1", "--format=%ce", head])?
    );
    let base = format!("origin/{BASE}");
    if let Err(e) = in_worktree(&["-c", &name, "-c", &email, "rebase", "--quiet", &base]) {
        let conflicts =
            in_worktree(&["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let aborted = in_worktree(&["rebase", "--abort"]);
        if conflicts.is_empty() {
            return Err(e);
        }
        aborted?;
        let files: Vec<&str> = conflicts.lines().collect();
        return Ok(Rebase::Refused(format!(
            "it conflicts with main in {}",
            files.join(", ")
        )));
    }
    let rebased = in_worktree(&["rev-parse", "HEAD"])?;
    let lease = format!("--force-with-lease={full_ref}:{head}");
    let target = format!("HEAD:{full_ref}");
    if let Err(e) = in_worktree(&["push", "--quiet", &lease, "origin", &target]) {
        // Best effort: a branch left off the head is refused on the next look.
        let _ = in_worktree(&["reset", "--quiet", "--hard", head]);
        return Err(e);
    }
    Ok(Rebase::Pushed(rebased))
}

// Git for the worktree, with its git dirs named rather than found. The
// worker can write the worktree's own git dir, so its `commondir` is checked
// against the repo's, and hooks are off: none of them is kelpie's to run.
fn trusted<'a>(
    repo: &Path,
    worktree: &'a Path,
) -> Result<impl Fn(&[&str]) -> Result<String, WorktreeError> + 'a, WorktreeError> {
    let foreign = || WorktreeError::Foreign(worktree.to_owned());
    let common = git(
        repo,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = canonical(Path::new(&common));
    let own = own_git_dir(&common, worktree).ok_or_else(foreign)?;
    let named = std::fs::read_to_string(own.join("commondir")).map_err(|_| foreign())?;
    if canonical(&own.join(named.trim())) != common {
        return Err(foreign());
    }
    let prefix = [
        "--git-dir".into(),
        own.into_os_string(),
        "--work-tree".into(),
        worktree.as_os_str().to_owned(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
    ];
    Ok(move |args: &[&str]| {
        let args = prefix.iter().cloned().chain(args.iter().map(Into::into));
        git(worktree, args.collect::<Vec<std::ffi::OsString>>())
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
