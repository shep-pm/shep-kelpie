//! A work item's worktree and build folder
//!
//! Each work item gets its own git worktree, on a branch cut from the latest
//! `origin/main`, and its own build folder. A rework's branch starts from
//! itself on `origin` instead. Preparing is idempotent, so a restarted runner
//! keeps the worktree it made before.
//!
//! `main` and `origin/main` in these docs are the project's base branch, which
//! each function takes as `base`: `main` unless the project's settings name
//! another.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

mod active;
mod adopt;
mod borrowed;
#[cfg(test)]
mod borrowed_tests;
mod cache;
mod catch_up;
mod changed;
mod common;
mod contribution;
#[cfg(test)]
mod line_endings_tests;
mod push;
mod ref_folders;
mod remote;
mod stale_lock;
mod workflows;
mod writable;

pub use active::touched_since;
pub use adopt::adopt;
pub use borrowed::borrowed;
pub use cache::{Pruned, TurnMark, prune};
pub use catch_up::{Rebase, rebase};
pub use changed::changed_files;
pub use contribution::{
    Change, Opaque, Touched, change as contribution_change, changed_paths,
    checkout as contribution_checkout,
};
pub use push::push_commits;
pub use remote::Remote;
pub use workflows::runs_on_pull_requests;
pub use writable::remove_tree;

/// Where a new worktree's branch starts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Cut from `origin/main`
    Main,
    /// The branch as its [`Remote`] holds it, whoever pushed to it
    Pushed,
}

/// A work item's worktree and the project repo it belongs to, whose git
/// dirs kelpie checks it against before running git on it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Linked<'a> {
    /// The project's repo
    pub repo: &'a Path,
    /// The work item's worktree, which its worker writes
    pub worktree: &'a Path,
}

/// A prepared worktree, and the git dirs a commit from it writes to
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The repo's common git dir: objects, refs and config
    pub git_common_dir: PathBuf,
    /// The worktree's own git dir: its index and HEAD
    pub git_dir: PathBuf,
    /// What the fetch of `origin/main` failed with, when it failed
    ///
    /// The worktree keeps the `origin/main` kelpie last fetched, and the
    /// runner notes this for `status`, since the worker merges that ref and
    /// runs no fetch of its own.
    pub main_fetch_failed: Option<String>,
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
    /// The worktree is off the head kelpie knew, or has changes not committed
    Unsettled(PathBuf),
    /// A folder could not be created
    Folder {
        /// The folder
        path: PathBuf,
        /// What creating it failed with
        kind: io::ErrorKind,
    },
    /// A file or folder in the worktree could not be read
    Unreadable {
        /// The file or folder
        path: PathBuf,
        /// What reading it failed with
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
            Self::Unsettled(path) => write!(
                f,
                "{} holds work kelpie has not seen pushed",
                path.display()
            ),
            Self::Folder { path, kind } => {
                write!(f, "cannot create {}: {kind}", path.display())
            }
            Self::Unreadable { path, kind } => {
                write!(f, "cannot read {}: {kind}", path.display())
            }
            Self::Remove { path, kind } => {
                write!(f, "cannot remove {}: {kind}", path.display())
            }
        }
    }
}

impl core::error::Error for WorktreeError {}

/// Makes sure `worktree` is a worktree of `repo` on `branch`, and `build` exists
///
/// A new worktree's branch starts where `start` says, just fetched, and does
/// not track it. [`Start::Pushed`] reads the branch from `remote`. Every call
/// fetches `origin/main`. Where that fetch fails the worktree keeps the last
/// one and [`Worktree::main_fetch_failed`] says why, so the turn goes on. A
/// [`Start::Main`] branch starts on that ref, so there a failed fetch errors.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn prepare(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    start: Start,
    base: &str,
    remote: &Remote,
    build: &Path,
) -> Result<Worktree, WorktreeError> {
    let foreign = || WorktreeError::Foreign(worktree.to_owned());
    stale_lock::clear(&repo.join(".git"), SystemTime::now());
    let full_ref = format!("refs/heads/{branch}");
    let mut main_fetch_failed = None;
    if worktree.exists() {
        if listed_branch(repo, worktree)?.as_deref() != Some(full_ref.as_str()) {
            return Err(foreign());
        }
        // The worker merges `origin/main` and runs no fetch of its own, so
        // kelpie brings main to it before each of its turns.
        main_fetch_failed = fetch_main(repo, base);
    } else {
        let local = git(repo, ["rev-parse", "--verify", "--quiet", &full_ref]).ok();
        if local.is_some() && start == Start::Main {
            return Err(WorktreeError::BranchTaken(branch.to_owned()));
        }
        if let Some(parent) = worktree.parent() {
            create(parent)?;
        }
        let start_ref = match start {
            Start::Main => {
                git(repo, ["fetch", "--quiet", "--", "origin", base])?;
                format!("origin/{base}")
            }
            // A branch that is already pushed needs main only for the merge
            // its worker runs, so a failed fetch of it is reported rather
            // than fatal.
            Start::Pushed => {
                let tracking = remote.fetch(repo, branch)?;
                main_fetch_failed = fetch_main(repo, base);
                tracking
            }
        };
        let wt = worktree.as_os_str();
        if let Some(local) = local {
            // A rework's branch left behind by an earlier attempt is reused
            // when it is at the same commit as the remote's. One ahead holds
            // work that is not kelpie's to throw away, and one behind is
            // not the pull request's branch as the remote holds it.
            if git(repo, ["rev-parse", "--verify", "--quiet", &start_ref])? != local {
                return Err(WorktreeError::BranchTaken(branch.to_owned()));
            }
            checkout_git(
                repo,
                [
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "--quiet".as_ref(),
                    wt,
                    OsStr::new(branch),
                ],
            )?;
        } else {
            checkout_git(
                repo,
                [
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "--quiet".as_ref(),
                    "--no-track".as_ref(),
                    "-b".as_ref(),
                    OsStr::new(branch),
                    wt,
                    start_ref.as_ref(),
                ],
            )?;
        }
    }
    let git_common_dir = common_dir(repo)?;
    let git_dir = own_git_dir(&git_common_dir, worktree).ok_or_else(foreign)?;
    ref_folders::ensure(&git_common_dir, branch)?;
    create(build)?;
    Ok(Worktree {
        git_common_dir,
        git_dir,
        main_fetch_failed,
    })
}

// Fetch `origin/main`, and say what it failed with. A fetch that fails leaves
// the ref where kelpie last put it, and the gate fetches main again before it
// reads a head.
fn fetch_main(repo: &Path, base: &str) -> Option<String> {
    git(repo, ["fetch", "--quiet", "--", "origin", base])
        .err()
        .map(|e| e.to_string())
}

/// Removes a work item's worktree, its branch and its build folder
///
/// `remote` also deletes the branch on that [`Remote`]. Whatever is already
/// gone, a branch the forge deleted first included, is skipped, so a removal
/// cut short can run again.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn remove(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    build: &Path,
    remote: Option<&Remote>,
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
    // Deleting the last loose ref in a folder removes the folder, and the
    // workers still running have no leave to make it again. They must not
    // wait out the round trips to origin, or an error from them, for it.
    let common = common_dir(repo)?;
    ref_folders::ensure(&common, branch)?;
    let on_remote_deleted = match remote {
        Some(remote) => delete_on_remote(repo, remote, branch),
        None => Ok(()),
    };
    ref_folders::ensure(&common, branch)?;
    on_remote_deleted?;
    match remove_tree(build) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(WorktreeError::Remove {
            path: build.to_owned(),
            kind: e.kind(),
        }),
        _ => Ok(()),
    }
}

/// Deletes the work item's `branch` on `remote` when it is there
fn delete_on_remote(repo: &Path, remote: &Remote, branch: &str) -> Result<(), WorktreeError> {
    let full_ref = remote.branch_ref(branch);
    if !on_remote(repo, remote, &full_ref)? {
        return Ok(());
    }
    // The forge may delete a merged head branch itself between the check
    // above and this push. Git's error for that varies, so a failed delete
    // is judged by whether the ref is still there. The repo's own pre-push
    // hook has no say over a branch kelpie deletes.
    let target = remote.target();
    match git(
        repo,
        [
            "push",
            "--quiet",
            "--no-verify",
            "--delete",
            "--",
            target,
            &full_ref,
        ],
    ) {
        Err(e) if on_remote(repo, remote, &full_ref)? => Err(e),
        _ => Ok(()),
    }
}

/// Makes `view` a detached worktree of `repo` at `origin/main`, just
/// fetched, for a session that reads the repo and changes nothing
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn view(repo: &Path, view: &Path, base: &str) -> Result<(), WorktreeError> {
    if let Some(parent) = view.parent() {
        create(parent)?;
    }
    git(repo, ["fetch", "--quiet", "origin", base])?;
    let tip = format!("origin/{base}");
    let args: [&OsStr; 5] = [
        "worktree".as_ref(),
        "add".as_ref(),
        "--detach".as_ref(),
        view.as_os_str(),
        tip.as_ref(),
    ];
    checkout_git(repo, args).map(drop)
}

/// Removes the detached worktree [`view`] made, if it is there
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn remove_view(repo: &Path, view: &Path) -> Result<(), WorktreeError> {
    if view.exists() {
        let args: [&OsStr; 4] = [
            "worktree".as_ref(),
            "remove".as_ref(),
            "--force".as_ref(),
            view.as_os_str(),
        ];
        git(repo, args)?;
    }
    git(repo, ["worktree", "prune"]).map(drop)
}

/// The worktree other than `own` that has `branch` checked out, if one has:
/// the project's checkout and a maintainer's own worktree included
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn holder_of(repo: &Path, branch: &str, own: &Path) -> Result<Option<PathBuf>, WorktreeError> {
    let list = git(repo, ["worktree", "list", "--porcelain"])?;
    let (own, want) = (canonical(own), format!("refs/heads/{branch}"));
    let mut at = None;
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            at = Some(PathBuf::from(path));
        } else if line.strip_prefix("branch ") == Some(want.as_str())
            && let Some(path) = at.take().filter(|path| canonical(path) != own)
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Whether a work item's branch or worktree holds work `origin/main` lacks:
/// a commit on `branch` or at the worktree's head, or a file not committed
///
/// Fetches `origin/main` first. A branch or worktree that is not there
/// holds none.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn holds_work(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
) -> Result<bool, WorktreeError> {
    git(repo, ["fetch", "--quiet", "origin", base])?;
    let mut tips = Vec::new();
    let full_ref = format!("refs/heads/{branch}");
    if has_ref(repo, &full_ref)? {
        tips.push(full_ref);
    }
    if worktree.exists() {
        if !uncommitted(repo, worktree)?.is_empty() {
            return Ok(true);
        }
        tips.push(head(repo, worktree)?);
    }
    if tips.is_empty() {
        return Ok(false);
    }
    let behind = format!("^origin/{base}");
    let mut args = vec!["rev-list", "--count", behind.as_str()];
    args.extend(tips.iter().map(String::as_str));
    Ok(git(repo, args)? != "0")
}

/// Whether `full_ref` is in `repo`, telling a ref that is not there from a
/// git that cannot answer
fn has_ref(repo: &Path, full_ref: &str) -> Result<bool, WorktreeError> {
    // `show-ref --verify` answers a missing ref with exit 1.
    let args = ["show-ref", "--verify", "--quiet", full_ref];
    let output = in_repo(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        }),
    }
}

/// Whether `full_ref` is a branch on `remote` right now, asked of the remote
fn on_remote(repo: &Path, remote: &Remote, full_ref: &str) -> Result<bool, WorktreeError> {
    let asked = git(
        repo,
        ["ls-remote", "--heads", "--", remote.target(), full_ref],
    )?;
    Ok(!asked.is_empty())
}

/// Where a pull request's head stands against `origin/main`, just fetched
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// The head is the branch on its remote, and has the latest `main`
    Current,
    /// The head is the branch on its remote, and lacks the latest `main`
    Behind,
    /// The branch on its remote is not the head the forge reported, which
    /// lags a push by a moment
    Lagging,
}

/// Fetches `origin/main` and `remote`'s branch, and says where `head`, the
/// forge's head of `branch`, stands
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn base_of(
    repo: &Path,
    branch: &str,
    head: &str,
    remote: &Remote,
    base: &str,
) -> Result<Base, WorktreeError> {
    let tracking = remote.fetch_with_main(repo, branch, base)?;
    if git(repo, ["rev-parse", "--verify", "--quiet", &tracking])? != head {
        return Ok(Base::Lagging);
    }
    // `--is-ancestor` answers no with exit 1, and fails with any other code.
    let tip = format!("origin/{base}");
    let args = ["merge-base", "--is-ancestor", &tip, head];
    let output = in_repo(repo)
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

/// Fetches `branch` from `remote`, and returns its head there
///
/// Ask git rather than the forge: the forge's head lags a push by a moment.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn origin_head(repo: &Path, branch: &str, remote: &Remote) -> Result<String, WorktreeError> {
    let tracking = remote.fetch(repo, branch)?;
    git(repo, ["rev-parse", "--verify", "--quiet", &tracking])
}

/// Fetches `origin/main` and `branch` from `remote` together, and returns the
/// branch's head
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn head_with_main(
    repo: &Path,
    branch: &str,
    remote: &Remote,
    base: &str,
) -> Result<String, WorktreeError> {
    let tracking = remote.fetch_with_main(repo, branch, base)?;
    git(repo, ["rev-parse", "--verify", "--quiet", &tracking])
}

/// The commit `worktree` has checked out
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn head(repo: &Path, worktree: &Path) -> Result<String, WorktreeError> {
    trusted(repo, worktree)?(&["rev-parse", "HEAD"])
}

/// Abandons the merge in progress in `worktree`, putting it back at its head
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn abort_merge(repo: &Path, worktree: &Path) -> Result<(), WorktreeError> {
    trusted(repo, worktree)?(&["merge", "--abort"]).map(drop)
}

/// The files `worktree` holds that its last commit does not: changed, staged
/// or new and not ignored
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn uncommitted(repo: &Path, worktree: &Path) -> Result<Vec<String>, WorktreeError> {
    // One `status` sees the index, the working tree and the new files. Its
    // second format, since the first starts an entry with a space that `git`
    // trims off the first one.
    let status =
        trusted(repo, worktree)?(&["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
    let mut files = status_names(&status);
    files.sort();
    files.dedup();
    Ok(files)
}

// The paths in `git status --porcelain=v2 -z`, where an entry is a kind, its
// fields and then the path, and a rename or copy adds the old path as an
// entry of its own.
fn status_names(status: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let fields = match entry.chars().next() {
            Some('?') => 2,
            Some('1') => 9,
            Some('u') => 11,
            Some('2') => {
                names.extend(entries.next().map(str::to_owned));
                10
            }
            _ => continue,
        };
        names.extend(entry.splitn(fields, ' ').last().map(str::to_owned));
    }
    names
}

/// The range of a work item's own change for `git diff`: from the merge base
/// of `base` and the worktree's `HEAD` to `HEAD`
///
/// A branch behind `main` read against `main`'s tip shows what `main` gained
/// as the branch undoing it. An adopted item's `base` is a commit on the
/// branch, where the merge base is that commit itself.
pub fn change_since(base: &str) -> String {
    format!("{base}...HEAD")
}

/// The commit a work item's own change starts from: the merge base of `base`
/// and the worktree's `HEAD`
///
/// For a script that diffs a ref of its own, which a range would not suit.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn change_start(repo: &Path, worktree: &Path, base: &str) -> Result<String, WorktreeError> {
    trusted(repo, worktree)?(&["merge-base", base, "HEAD"])
}

/// Git for the worktree, with its git dirs named rather than found, since
/// git that finds them reads the config of whatever repo the worker points
/// it at. The worktree's `commondir` is checked against the repo's, hooks
/// are off, and replace refs are ignored.
///
/// # Errors
///
/// [`WorktreeError`] when the worktree's git dirs cannot be named, or its
/// `commondir` is not the repo's.
pub fn trusted<'a>(
    repo: &Path,
    worktree: &'a Path,
) -> Result<impl Fn(&[&str]) -> Result<String, WorktreeError> + 'a, WorktreeError> {
    let dirs = git_dirs(repo, worktree)?;
    Ok(move |args: &[&str]| output(trusted_git(worktree, &dirs), args))
}

/// `git` about `worktree`, as [`trusted`] runs it, for a call that reads
/// its exit code or its output whole
pub fn trusted_command(repo: &Path, worktree: &Path) -> Result<Command, WorktreeError> {
    Ok(trusted_git(worktree, &git_dirs(repo, worktree)?))
}

/// Sets the environment that makes the git `program` runs on `worktree` the
/// trusted one, as [`trusted`] runs it
///
/// # Errors
///
/// [`WorktreeError`] when the worktree's git dirs are not `repo`'s.
pub fn trust_git_of(
    program: &mut Command,
    repo: &Path,
    worktree: &Path,
) -> Result<(), WorktreeError> {
    let (own, common) = git_dirs(repo, worktree)?;
    program
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_DIR", own)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_COMMON_DIR", common)
        .env("GIT_CONFIG_COUNT", TRUSTED_CONFIG.len().to_string())
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    for (n, (key, value)) in TRUSTED_CONFIG.iter().enumerate() {
        program
            .env(format!("GIT_CONFIG_KEY_{n}"), key)
            .env(format!("GIT_CONFIG_VALUE_{n}"), value);
    }
    Ok(())
}

// The config every trusted git runs with.
const TRUSTED_CONFIG: [(&str, &str); 3] = [
    // None of the hooks is kelpie's to run.
    ("core.hooksPath", "/dev/null"),
    // `status`, `diff`, `reset`, `merge` and `rebase` start this program.
    ("core.fsmonitor", "false"),
    // Git for Windows' system config sets `true`, and a file checked out with
    // CRs differs from its blob in the fence's byte comparison.
    ("core.autocrlf", "false"),
];

// The worktree's own git dir and the repo's common one, once the own dir's
// `commondir` is checked to name the repo's.
fn git_dirs(repo: &Path, worktree: &Path) -> Result<(PathBuf, PathBuf), WorktreeError> {
    let foreign = || WorktreeError::Foreign(worktree.to_owned());
    let common = common_dir(repo)?;
    let own = own_git_dir(&common, worktree).ok_or_else(foreign)?;
    let named = std::fs::read_to_string(own.join("commondir")).map_err(|_| foreign())?;
    if canonical(&own.join(named.trim())) != common {
        return Err(foreign());
    }
    Ok((own, common))
}

// `git` about `worktree` with its git dirs named. The common dir is named
// too, so git never reads `commondir` again after the check. Config passed
// down from kelpie's own environment is dropped.
fn trusted_git(worktree: &Path, (own, common): &(PathBuf, PathBuf)) -> Command {
    let mut git = in_repo(worktree);
    git.env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env("GIT_COMMON_DIR", common)
        .arg("--git-dir")
        .arg(own)
        .arg("--work-tree")
        .arg(worktree);
    for (key, value) in TRUSTED_CONFIG {
        git.arg("-c").arg(format!("{key}={value}"));
    }
    git.arg("--no-replace-objects");
    git
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

fn common_dir(repo: &Path) -> Result<PathBuf, WorktreeError> {
    common::of(repo)
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

/// `git -C cwd`, waiting its turn for a ref another git holds
///
/// The open work items' worktrees share one repo's refs, and their calls
/// run at once, so a ref or `packed-refs` lock one git holds makes another
/// wait for it rather than fail at once.
pub fn in_repo(cwd: &Path) -> Command {
    let mut git = crate::spawn::command("git");
    git.args(["-c", "core.filesRefLockTimeout=2000"])
        .args(["-c", "core.packedRefsTimeout=5000"])
        .arg("-C")
        .arg(cwd);
    git
}

// Runs `git args` in `cwd` as `git` does, with files checked out as committed:
// the worktree has no git dirs of its own yet to carry the setting.
fn checkout_git<I, S>(cwd: &Path, args: I) -> Result<String, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut git = in_repo(cwd);
    git.args(["-c", "core.autocrlf=false"]);
    output(git, args)
}

/// Runs `git args` in `cwd`, with the git dir found there, and returns its trimmed stdout
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn git<I, S>(cwd: &Path, args: I) -> Result<String, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    output(in_repo(cwd), args)
}

// Runs `git` with `args` added, and returns its trimmed stdout.
fn output<I, S>(mut git: Command, args: I) -> Result<String, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let output = git
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use super::{git, has_ref, remove, status_names};

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    // A repo with nothing to remove but the build folder
    fn remove_build(repo: &Path, build: &Path) -> Result<(), super::WorktreeError> {
        git(repo, ["init", "--quiet"]).unwrap();
        remove(repo, &repo.join("none"), "kelpie/7", build, None)
    }

    #[cfg(unix)]
    #[test]
    fn a_build_folder_holding_read_only_folders_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build");
        let cache = build.join("gomod/cache/mod");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("go.mod"), "module x\n").unwrap();
        set_mode(&cache.join("go.mod"), 0o444);
        set_mode(&cache, 0o555);
        set_mode(&build.join("gomod"), 0o555);

        remove_build(dir.path(), &build).unwrap();
        assert!(!build.exists());
    }

    #[test]
    fn removing_a_work_item_keeps_its_claude_config_folder() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build/7");
        std::fs::create_dir_all(&build).unwrap();
        let transcript = dir.path().join("claude/7/worker/projects/x/s.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{}\n").unwrap();

        remove_build(dir.path(), &build).unwrap();
        assert!(!build.exists());
        assert!(transcript.is_file());
    }

    #[test]
    fn a_build_folder_already_gone_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        remove_build(dir.path(), &dir.path().join("build")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_build_folder_leaves_what_it_points_at_alone() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        set_mode(&outside, 0o555);
        let build = dir.path().join("build");
        let cache = build.join("mod");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("go.mod"), "module x\n").unwrap();
        set_mode(&cache, 0o555);
        symlink(&outside, build.join("link")).unwrap();

        let removed = remove_build(dir.path(), &build);
        let mode = mode_of(&outside);
        set_mode(&outside, 0o755);
        removed.unwrap();
        assert!(!build.exists());
        assert!(outside.exists());
        assert_eq!(mode, 0o555);
    }

    #[test]
    fn a_missing_ref_is_absent_but_a_folder_git_cannot_read_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(has_ref(dir.path(), "refs/heads/kelpie/7").is_err());
        git(dir.path(), ["init", "--quiet"]).unwrap();
        assert!(!has_ref(dir.path(), "refs/heads/kelpie/7").unwrap());
    }

    #[test]
    fn a_status_names_every_changed_staged_renamed_and_new_path() {
        let status = "1 .M N... 100644 100644 100644 aaaa bbbb src/a b.rs\0\
                      1 AD N... 000000 100644 000000 0000 cccc staged.txt\0\
                      2 R. N... 100644 100644 100644 dddd eeee R100 new.rs\0old.rs\0\
                      u UU N... 100644 100644 100644 100644 ffff gggg hhhh clash.rs\0\
                      ? left.txt\0";
        assert_eq!(
            status_names(status),
            [
                "src/a b.rs",
                "staged.txt",
                "old.rs",
                "new.rs",
                "clash.rs",
                "left.txt"
            ]
        );
    }
}
